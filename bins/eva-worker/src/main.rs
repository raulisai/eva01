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

    Ok(WorkerContext {
        store,
        // Empty for the MVP — see the field's own doc in dispatch.rs for why.
        app_index: eva_intent::AppIndex::new(Vec::new()),
        wake_word,
        project_dir: std::env::current_dir()?,
        desktop: Arc::new(eva_mcp::SystemDesktop),
        agents: eva_agents::default_registry(),
    })
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
