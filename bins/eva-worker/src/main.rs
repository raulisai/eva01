//! The worker process: does everything that can crash (`docs/PLAN.md` §3.3)
//! — audio, STT, text cleanup, intent parsing, desktop actions, agents —
//! isolated from `eva-shell`, which supervises it. Reads [`ShellToWorker`]
//! commands as JSON-lines on stdin, writes [`WorkerToShell`] events as
//! JSON-lines on stdout.

mod dispatch;

use dispatch::WorkerContext;
use eva_ipc::{ShellToWorker, WorkerToShell};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_logging();
    tracing::info!("eva-worker iniciando");

    let ctx = match build_context().await {
        Ok(ctx) => ctx,
        Err(e) => {
            // Nothing to recover into yet — report on stdout so eva-shell's
            // supervisor sees why the worker could not even start, then
            // exit so the restart-with-backoff loop in eva-shell takes over.
            let _ = write_line(&mut tokio::io::stdout(), &WorkerToShell::Error {
                request_id: None,
                message: format!("eva-worker no pudo iniciar: {e}"),
                recoverable: false,
            })
            .await;
            tracing::error!("no se pudo construir el contexto del worker: {e}");
            std::process::exit(1);
        }
    };

    let mut stdout = tokio::io::stdout();
    if write_line(&mut stdout, &WorkerToShell::Ready).await.is_err() {
        tracing::error!("no se pudo escribir en stdout; terminando");
        return;
    }

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                tracing::info!("stdin cerrado; eva-shell terminó — apagando");
                break;
            }
            Err(e) => {
                tracing::error!("error leyendo stdin: {e}");
                break;
            }
        };

        let command: ShellToWorker = match eva_ipc::decode_line(&line) {
            Ok(cmd) => cmd,
            Err(e) => {
                tracing::warn!("línea no reconocida de eva-shell, se ignora: {e}");
                continue;
            }
        };

        let is_shutdown = matches!(command, ShellToWorker::Shutdown);
        for event in dispatch::handle(&ctx, command).await {
            if write_line(&mut stdout, &event).await.is_err() {
                tracing::error!("no se pudo escribir en stdout; terminando");
                return;
            }
        }
        if is_shutdown {
            tracing::info!("apagado solicitado por eva-shell");
            break;
        }
    }
}

async fn build_context() -> Result<WorkerContext, Box<dyn std::error::Error>> {
    let store = eva_store::Store::open(&store_path()?)?;
    let wake_word: String = store.get_setting("wake_word")?.unwrap_or_else(|| "Adán".to_string());
    let app_index = scan_applications();
    let audio = load_audio_context();

    Ok(WorkerContext::new(
        store,
        app_index,
        wake_word,
        std::env::current_dir()?,
        Arc::new(eva_mcp::SystemDesktop),
        eva_agents::default_registry(),
        audio,
        load_formatter(),
    ))
}

/// Picks the context-aware formatter (`docs/PLAN.md` §3 fase 3 point 4):
/// Apple Intelligence when this device reports it available right now,
/// [`eva_text::RuleOnlyFormatter`] otherwise — the graceful-degradation
/// default from §3.3 point 5, never "no formatter at all".
fn load_formatter() -> Arc<dyn eva_text::Formatter> {
    match eva_text::AppleIntelligenceFormatter::new() {
        Some(formatter) => {
            tracing::info!("formateador: Apple Intelligence (Foundation Models on-device)");
            Arc::new(formatter)
        }
        None => {
            tracing::warn!(
                "formateador: Apple Intelligence no disponible en este equipo, usando reglas únicamente \
                 (revisa Ajustes del Sistema → Apple Intelligence y Siri)"
            );
            Arc::new(eva_text::RuleOnlyFormatter)
        }
    }
}

/// Scans `/Applications` for `.app` bundles and builds an [`eva_intent::AppIndex`]
/// from their names — closing the gap from `docs/PLAN.md` §10 decision 8's
/// "the focused project/app is used instead" default: an empty index meant
/// "Adán, abre Brave" always fell through to an agent instead of actually
/// opening Brave. This is deliberately simple (one directory, no recursion
/// into `/System/Applications` or `~/Applications`, no bundle metadata
/// beyond the file name) — the full "índice de apps" from `docs/PLAN.md`
/// fase 5 can grow this later; this closes the immediate, user-visible gap.
fn scan_applications() -> eva_intent::AppIndex {
    let entries = std::fs::read_dir("/Applications")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "app"))
        .filter_map(|entry| {
            let name = entry.path().file_stem()?.to_str()?.to_string();
            Some(eva_intent::AppEntry::new(name))
        })
        .collect::<Vec<_>>();

    tracing::info!(count = entries.len(), "aplicaciones indexadas desde /Applications");
    eva_intent::AppIndex::new(entries)
}

/// Loads the STT model named by `EVA_STT_MODEL_PATH`, if set. Returns `None`
/// (not an error) when unset or unloadable — [`WorkerContext`]'s `audio`
/// field being `None` is exactly the documented, honest "not configured"
/// state `dispatch::handle_start_recording` reports to the user, per
/// `docs/PLAN.md` §3.3 point 5. There is no bundled default model: per
/// `docs/PLAN.md` §5, the production default is `canary-1b-flash`, a
/// multi-file ONNX model that is a packaging decision (fase 1's download
/// step), not something this binary can reasonably embed.
/// Prefers Canary (`EVA_CANARY_MODEL_DIR`) — `docs/PLAN.md` §5's actual
/// production default, native Spanish, no translation bolt-on — and falls
/// back to Whisper (`EVA_STT_MODEL_PATH`) if only that is configured. Both
/// are real, working `SpeechToText` implementations as of this increment;
/// which one loads is purely a matter of which environment variable is set,
/// not a code difference.
fn load_audio_context() -> Option<dispatch::AudioContext> {
    if let Ok(canary_dir) = std::env::var("EVA_CANARY_MODEL_DIR") {
        let language = std::env::var("EVA_STT_LANGUAGE").unwrap_or_else(|_| "es".to_string());
        let path = std::path::PathBuf::from(&canary_dir);
        return match eva_audio::CanarySpeechToText::load(&path, language) {
            Ok(stt) => {
                tracing::info!(model = %canary_dir, "modelo Canary cargado");
                Some(dispatch::AudioContext {
                    source: Arc::new(eva_audio::MicrophoneSource),
                    stt: Arc::new(stt),
                    model_id: format!("canary:{canary_dir}"),
                })
            }
            Err(e) => {
                tracing::error!(model = %canary_dir, error = %e, "no se pudo cargar el modelo Canary");
                None
            }
        };
    }

    let model_path = std::env::var("EVA_STT_MODEL_PATH").ok()?;
    let path = std::path::PathBuf::from(&model_path);

    match eva_audio::WhisperSpeechToText::load(&path) {
        Ok(stt) => {
            tracing::info!(model = %model_path, "modelo Whisper cargado");
            Some(dispatch::AudioContext {
                source: Arc::new(eva_audio::MicrophoneSource),
                stt: Arc::new(stt),
                model_id: format!("whisper:{model_path}"),
            })
        }
        Err(e) => {
            tracing::error!(model = %model_path, error = %e, "no se pudo cargar el modelo Whisper");
            None
        }
    }
}

fn store_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let mut dir = dirs::data_local_dir().ok_or("no se encontró el directorio de datos de la aplicación")?;
    dir.push("EVA01");
    std::fs::create_dir_all(&dir)?;
    dir.push("eva.sqlite3");
    Ok(dir)
}

/// Sets up `tracing`, writing to `~/Library/Logs/EVA01/eva-worker.log`
/// (`docs/PLAN.md` §3.4) as well as stderr — never stdout, which is
/// reserved for the [`WorkerToShell`] JSON-line protocol.
fn init_logging() {
    let log_dir = dirs::home_dir().map(|h| h.join("Library/Logs/EVA01")).unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "eva-worker.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // Leaking the guard is deliberate: it must live for the process's
    // entire lifetime to keep flushing the non-blocking writer, and this
    // process has no earlier point to store it that would outlive `main`.
    std::mem::forget(guard);

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &WorkerToShell,
) -> std::io::Result<()> {
    let line = eva_ipc::encode_line(msg).map_err(std::io::Error::other)?;
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}
