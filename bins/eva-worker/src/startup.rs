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
    match store.mark_unfinished_tasks_as_interrupted("interrumpida: el worker se reinició") {
        Ok(0) => {}
        Ok(count) => tracing::warn!(count, "tareas que estaban en curso cuando el worker murió; marcadas como interrumpidas"),
        Err(e) => tracing::warn!("no se pudo revisar las tareas interrumpidas: {e}"),
    }

    let wake_word: String = store.get_setting("wake_word")?.unwrap_or_else(|| config.wake_word.0.clone());
    let (events, rx) = Events::channel();
    let (formatter, formatter_name) = load_formatter();

    // The gateway socket. Failing to create it degrades the agents (they run
    // without EVA's tools), never the dictation.
    let (endpoint, mcp) = match rpc::bind(&support.join("run")) {
        Ok(endpoint) => {
            let mcp = eva_mcp_binary().map(|binary| endpoint.injection(binary));
            if mcp.is_none() {
                tracing::warn!("no se encontró eva-mcp junto a eva-worker; los agentes correrán sin las herramientas de EVA");
            }
            (Some(endpoint), mcp)
        }
        Err(e) => {
            tracing::warn!("no se pudo crear el socket del gateway: {e}; los agentes correrán sin las herramientas de EVA");
            (None, None)
        }
    };
    let gateway_socket = endpoint.as_ref().map(|e| e.socket.clone());

    let ctx = Arc::new(WorkerContext::new(WorkerDeps {
        store,
        app_index: scan_applications(),
        wake_word,
        base_dir: std::env::current_dir()?,
        desktop: Arc::new(eva_mcp::SystemDesktop::with_voice(config.feedback.voice.clone())),
        agents: eva_agents::registry_with_priority(&config.agents.priority),
        audio: load_audio(&config, &support),
        formatter,
        events,
        mcp,
        worktrees_dir: support.join("worktrees"),
        formatter_name,
        config_warnings: loaded.warnings,
        config,
    }));

    if let Some(endpoint) = endpoint {
        endpoint.serve(&ctx);
    }
    Ok(Started { ctx, events: rx, gateway_socket })
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
/// default from §3.3 point 5, never "no formatter at all".
fn load_formatter() -> (Arc<dyn Formatter>, String) {
    match eva_text::AppleIntelligenceFormatter::new() {
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
    }
}

fn load_audio(config: &Config, support: &Path) -> Option<AudioContext> {
    let language = config.stt.language.clone();
    match eva_config::models::discover(config, support, &|key| std::env::var(key).ok()) {
        ModelChoice::Canary(path) => match eva_audio::CanarySpeechToText::load(&path, language) {
            Ok(stt) => {
                tracing::info!(model = %path.display(), "modelo Canary cargado");
                Some(AudioContext {
                    source: Arc::new(eva_audio::MicrophoneSource),
                    stt: Arc::new(stt),
                    model_id: format!("canary:{}", path.display()),
                })
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
            tracing::warn!("no hay ningún modelo de voz configurado ni en {}/models; `eva doctor` dice cómo instalarlo", support.display());
            None
        }
    }
}

/// The applications EVA can open by name: the `.app` bundles directly inside
/// the standard application folders. Deliberately simple (one level, no
/// bundle metadata beyond the file name); the full "índice de apps" from
/// `docs/PLAN.md` fase 5 can grow this later.
fn scan_applications() -> eva_intent::AppIndex {
    let mut folders = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = dirs::home_dir() {
        folders.push(home.join("Applications"));
    }

    let mut names: Vec<String> = folders
        .iter()
        .flat_map(|folder| std::fs::read_dir(folder).into_iter().flatten().filter_map(Result::ok))
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "app"))
        .filter_map(|entry| entry.path().file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    names.sort();
    names.dedup();

    tracing::info!(count = names.len(), "aplicaciones indexadas");
    eva_intent::AppIndex::new(names.into_iter().map(eva_intent::AppEntry::new).collect())
}
