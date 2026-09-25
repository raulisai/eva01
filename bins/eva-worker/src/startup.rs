//! Building the worker's [`WorkerContext`] from the machine it runs on: the
//! config file, the database, the speech model, the formatter, the agents,
//! and the gateway socket agents will use.

use crate::context::{AudioContext, Events, WorkerContext, WorkerDeps};
use crate::rpc;
use eva_config::models::ModelChoice;
use eva_config::{support_dir, Config};
use eva_text::Formatter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;

/// A started worker: its context, the events to write out, and where its
/// gateway socket is (for cleanup at shutdown).
pub struct Started {
    /// The worker's shared state.
    pub ctx: Arc<WorkerContext>,
    /// The events to write to stdout.
    pub events: UnboundedReceiver<eva_ipc::WorkerToShell>,
    /// The gateway socket, if it could be created.
    pub gateway_socket: Option<PathBuf>,
}

/// Loads everything and starts serving the gateway socket.
///
/// # Errors
/// The database could not be opened — the one thing the worker cannot
/// degrade around, since the dictionary, sessions and audit trail live in it.
pub async fn build() -> Result<Started, Box<dyn std::error::Error>> {
    let loaded = Config::load();
    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    let config = loaded.config;

    let support = support_dir();
    std::fs::create_dir_all(&support)?;
    let store = eva_store::Store::open(&support.join("eva.sqlite3"))?;
    // Another worker still running (the app's, while `eva doctor` starts
    // this one) may be the one running those tasks: they are only
    // interrupted if no worker is left to finish them.
    let peers = rpc::other_live_workers(&support.join("run"));
    let unfinished =
        if peers { Ok(0) } else { store.mark_unfinished_tasks_as_interrupted("interrumpida: el worker se reinició") };
    match unfinished {
        Ok(0) => {}
        Ok(count) => {
            tracing::warn!(count, "tareas que estaban en curso cuando el worker murió; marcadas como interrumpidas")
        }
        Err(e) => tracing::warn!("no se pudo revisar las tareas interrumpidas: {e}"),
    }

    match store.prune_transcripts(config.history.keep_days) {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!(count, keep_days = config.history.keep_days, "dictados viejos borrados del historial")
        }
        Err(e) => tracing::warn!("no se pudo limpiar el historial: {e}"),
    }

    let pruned = crate::training::prune(&crate::training::dir_of(&support.join("harvest")), config.history.keep_days);
    if pruned > 0 {
        tracing::info!(count = pruned, "audios de entrenamiento sin revisar borrados por su edad");
    }

    let worker_process = crate::orphans::Process::current();
    let stopped =
        crate::orphans::AgentLedger::new(support.join("run").join("agents"), worker_process.clone()).reap().await;
    if stopped > 0 {
        tracing::warn!(stopped, "agentes que seguían corriendo tras la muerte de su worker; se detuvieron");
    }

    let wake_word: String = store.get_setting("wake_word")?.unwrap_or_else(|| config.wake_word.0.clone());
    let (events, rx) = Events::channel();
    let (formatter, formatter_name) = load_formatter(&config);

    // The gateway socket. Failing to create it degrades the agents (they run
    // without EVA's tools), never the dictation.
    let (endpoint, mcp) = match rpc::bind(&support.join("run")) {
        Ok(endpoint) => {
            let mcp = eva_mcp_binary().map(|binary| endpoint.injection(binary));
            if mcp.is_none() {
                tracing::warn!(
                    "no se encontró eva-mcp junto a eva-worker; los agentes correrán sin las herramientas de EVA"
                );
            }
            (Some(endpoint), mcp)
        }
        Err(e) => {
            tracing::warn!(
                "no se pudo crear el socket del gateway: {e}; los agentes correrán sin las herramientas de EVA"
            );
            (None, None)
        }
    };
    let gateway_socket = endpoint.as_ref().map(|e| e.socket.clone());

    let audio = load_audio(&config, &support, &wake_word);
    let app_catalog =
        crate::apps::AppCatalog::new(scan_applications).with_learned(store.list_app_aliases().unwrap_or_default());
    let ctx = Arc::new(WorkerContext::new(WorkerDeps {
        store,
        app_index: app_catalog,
        wake_word,
        base_dir: std::env::current_dir()?,
        desktop: Arc::new(eva_mcp::SystemDesktop::with_voice(config.feedback.voice.clone())),
        agents: eva_agents::registry_with_priority(&config.agents.priority),
        audio,
        // Remembers what it formats, so a long dictation can be formatted ahead while it is spoken.
        formatter: Arc::new(eva_text::CachingFormatter::new(formatter)),
        events,
        mcp,
        worktrees_dir: support.join("worktrees"),
        formatter_name,
        config_warnings: loaded.warnings,
        harvest_dir: support.join("harvest"),
        agent_ledger_dir: support.join("run").join("agents"),
        worker_process,
        config,
    }));

    // A command made in the panel works on the next thing said, no restart.
    ctx.commands.watch(&Config::default_path());
    // A crash in the middle of a recording must not leave the Mac muted.
    eva_macos::duck::recover(&eva_config::support_dir().join("media-duck.state"));

    if let Some(endpoint) = endpoint {
        endpoint.serve(&ctx);
    }
    warm_up_formatter(&ctx.formatter);
    Ok(Started { ctx, events: rx, gateway_socket })
}

/// Loads the formatter's model in the background, so the first dictation does
/// not pay for it (see [`eva_text::warm_up`]). A plain thread: the call blocks
/// on the model and the worker's runtime has better things to do meanwhile.
fn warm_up_formatter(formatter: &Arc<dyn Formatter>) {
    let formatter = Arc::clone(formatter);
    std::thread::spawn(move || {
        let took = eva_text::warm_up(formatter.as_ref());
        tracing::info!(ms = took.as_millis() as u64, "formateador calentado");
    });
}

/// The `eva-mcp` binary next to this one — the same layout `eva-shell` uses
/// for the worker, whether a plain `cargo build` `target/` directory or the
/// packaged `EVA01.app/Contents/MacOS/`.
fn eva_mcp_binary() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name("eva-mcp");
    path.is_file().then_some(path)
}

/// Picks the context-aware formatter (`docs/PLAN.md` §3 fase 3 point 4):
/// Apple Intelligence when this device reports it available right now,
/// [`eva_text::RuleOnlyFormatter`] otherwise — the graceful-degradation
/// default from §3.3 point 5, never "no formatter at all". If the config
/// explicitly enables a remote model, it is layered on top for exactly the
/// jobs it names (`docs/PLAN.md` fase 9); the key comes from the environment
/// variable the config names, and without it the remote model stays off.
fn load_formatter(config: &Config) -> (Arc<dyn Formatter>, String) {
    let (local, mut name): (Arc<dyn Formatter>, String) = match eva_text::AppleIntelligenceFormatter::new() {
        Some(formatter) => {
            tracing::info!("formateador: Apple Intelligence (Foundation Models on-device)");
            (Arc::new(formatter), "apple_intelligence".to_string())
        }
        None => {
            tracing::warn!(
                "formateador: Apple Intelligence no disponible en este equipo, usando reglas únicamente \
                 (revisa Ajustes del Sistema → Apple Intelligence y Siri)"
            );
            (Arc::new(eva_text::RuleOnlyFormatter), "reglas".to_string())
        }
    };

    let remote = &config.remote;
    if !remote.enabled {
        return (local, name);
    }
    let api_key = std::env::var(&remote.api_key_env).unwrap_or_default();
    if api_key.trim().is_empty() {
        tracing::warn!(
            "remote.enabled está activo pero la variable {} no tiene la clave; el modelo remoto queda apagado",
            remote.api_key_env
        );
        return (local, name);
    }

    let client = eva_text::OpenAiCompatibleFormatter::new(&remote.base_url, &api_key, &remote.model, &remote.use_for);
    tracing::warn!(
        model = %remote.model,
        use_for = ?remote.use_for,
        "modelo remoto activado: los textos de esos trabajos SALEN de este equipo hacia {}",
        remote.base_url
    );
    name = format!("{name} + remoto ({})", remote.use_for.join(", "));
    (Arc::new(eva_text::RemoteAssisted::new(local, client)), name)
}

fn load_audio(config: &Config, support: &Path, wake_word: &str) -> Option<AudioContext> {
    let language = config.stt.language.clone();
    let padding = eva_audio::transcribe::Padding::from_name(&config.stt.padding).unwrap_or_default();
    match eva_config::models::discover(config, support, &|key| std::env::var(key).ok()) {
        ModelChoice::Canary(path) => match eva_audio::CanarySpeechToText::load(&path, language.clone()) {
            Ok(stt) => {
                tracing::info!(model = %path.display(), "modelo Canary cargado");
                let second = second_opinion_model(config, support, &path, &language, padding);
                let model_id = match &second {
                    Some(_) => format!("canary:{} (+ canary-180m-flash para frases cortas)", path.display()),
                    None => format!("canary:{}", path.display()),
                };
                let wake_word = wake_word.to_string();
                let wanted = Arc::new(move |text: &str| {
                    let text = eva_text::filler::remove_universal_fillers(text);
                    eva_intent::looks_like_command(&text, &wake_word, &[])
                });
                let stt =
                    eva_audio::second_opinion::SecondOpinion::new(Arc::new(stt.with_padding(padding)), second, wanted);
                Some(AudioContext { source: Arc::new(eva_audio::MicrophoneSource), stt: Arc::new(stt), model_id })
            }
            Err(e) => {
                tracing::error!(model = %path.display(), error = %e, "no se pudo cargar el modelo Canary");
                None
            }
        },
        ModelChoice::Whisper(path) => match eva_audio::WhisperSpeechToText::load(&path) {
            Ok(stt) => {
                tracing::info!(model = %path.display(), "modelo Whisper cargado");
                Some(AudioContext {
                    source: Arc::new(eva_audio::MicrophoneSource),
                    stt: Arc::new(stt),
                    model_id: format!("whisper:{}", path.display()),
                })
            }
            Err(e) => {
                tracing::error!(model = %path.display(), error = %e, "no se pudo cargar el modelo Whisper");
                None
            }
        },
        ModelChoice::None => {
            tracing::warn!(
                "no hay ningún modelo de voz configurado ni en {}/models; `eva doctor` dice cómo instalarlo",
                support.display()
            );
            None
        }
    }
}

/// `canary-180m-flash`, to double-check short clips, when the config wants
/// it, it is installed and it is not already the main model.
fn second_opinion_model(
    config: &Config,
    support: &Path,
    main: &Path,
    language: &str,
    padding: eva_audio::transcribe::Padding,
) -> Option<Arc<dyn eva_audio::SpeechToText>> {
    let spec = eva_config::models::find("canary-180m-flash")?;
    let dir = eva_config::models::model_dir(support, spec.id);
    if !config.stt.second_opinion || !spec.is_installed(&dir) || dir == main {
        return None;
    }
    match eva_audio::CanarySpeechToText::load(&dir, language) {
        Ok(stt) => {
            tracing::info!("canary-180m-flash cargado como segunda opinión para frases cortas");
            Some(Arc::new(stt.with_padding(padding)))
        }
        Err(e) => {
            tracing::warn!("no se pudo cargar canary-180m-flash como segunda opinión: {e}");
            None
        }
    }
}

/// The applications EVA can open by name: everything `eva_macos::installed_apps`
/// finds, with "el navegador" and "el correo" meaning this Mac's own defaults.
fn scan_applications() -> eva_intent::AppIndex {
    let names = eva_macos::installed_apps();
    tracing::info!(count = names.len(), "aplicaciones indexadas");
    eva_intent::AppIndex::for_this_mac(names, eva_macos::default_app_for)
}
