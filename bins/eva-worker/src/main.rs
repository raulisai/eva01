//! The worker process: does everything that can crash (`docs/PLAN.md` §3.3)
//! — audio, STT, text cleanup, intent parsing, desktop actions, agents —
//! isolated from `eva-shell`, which supervises it. Reads [`ShellToWorker`]
//! commands as JSON-lines on stdin, writes [`WorkerToShell`] events as
//! JSON-lines on stdout.

mod apps;
mod calibration;
mod commands;
mod confirm;
mod context;
mod dictation;
mod handler;
mod harvest;
mod housekeeping;
mod orphans;
mod recording;
mod rpc;
mod startup;
mod streaming;
mod tasks;
#[cfg(test)]
mod testkit;

use eva_ipc::{ShellToWorker, WorkerToShell};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use tracing_subscriber::EnvFilter;

/// How long a shutting-down worker waits for in-flight work (a dictation
/// mid-paste, a task announcing itself) before leaving anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() {
    init_logging();
    tracing::info!("eva-worker iniciando");

    let started = match startup::build().await {
        Ok(started) => started,
        Err(e) => {
            // Nothing to recover into yet — report on stdout so eva-shell's
            // supervisor sees why the worker could not even start, then
            // exit so the restart-with-backoff loop in eva-shell takes over.
            let _ = write_line(
                &mut tokio::io::stdout(),
                &WorkerToShell::Error {
                    request_id: None,
                    message: format!("eva-worker no pudo iniciar: {e}"),
                    recoverable: false,
                },
            )
            .await;
            tracing::error!("no se pudo construir el contexto del worker: {e}");
            std::process::exit(1);
        }
    };
    let ctx = started.ctx;
    let gateway_socket = started.gateway_socket.as_deref().map(|p| p.to_string_lossy().into_owned());

    let (stop_writer, writer_stopped) = oneshot::channel();
    let writer = tokio::spawn(write_events(started.events, writer_stopped));
    ctx.events.emit(WorkerToShell::Ready);

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
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
            Ok(command) => command,
            Err(e) => {
                tracing::warn!("línea no reconocida de eva-shell, se ignora: {e}");
                continue;
            }
        };

        if matches!(command, ShellToWorker::Shutdown) {
            tracing::info!("apagado solicitado por eva-shell");
            break;
        }
        handler::handle(&ctx, command, gateway_socket.as_deref());
    }

    // Shutdown: stop the agents (their processes must not outlive us), let
    // whatever is mid-flight finish, flush every event, remove the socket.
    let cancelled = ctx.tasks.cancel_all();
    if cancelled > 0 {
        tracing::info!(cancelled, "tareas de agente canceladas por el apagado");
    }
    if tokio::time::timeout(SHUTDOWN_GRACE, ctx.wait_idle()).await.is_err() {
        tracing::warn!("el apagado no esperó más por trabajo en curso");
    }
    let _ = stop_writer.send(());
    let _ = writer.await;
    if let Some(socket) = started.gateway_socket {
        let _ = std::fs::remove_file(socket);
    }
}

/// Writes every event to stdout as it arrives. On `stop`, drains what is
/// already queued and returns, so nothing sent before shutdown is lost.
async fn write_events(mut events: UnboundedReceiver<WorkerToShell>, mut stop: oneshot::Receiver<()>) {
    let mut stdout = tokio::io::stdout();
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { return };
                if write_line(&mut stdout, &event).await.is_err() {
                    tracing::error!("no se pudo escribir en stdout; terminando");
                    std::process::exit(1);
                }
            }
            _ = &mut stop => {
                while let Ok(event) = events.try_recv() {
                    let _ = write_line(&mut stdout, &event).await;
                }
                return;
            }
        }
    }
}

/// Sets up `tracing`, writing to `~/Library/Logs/EVA01/eva-worker.log`
/// (`docs/PLAN.md` §3.4) — never stdout, which is reserved for the
/// [`WorkerToShell`] JSON-line protocol.
fn init_logging() {
    let log_dir = dirs::home_dir().map(|h| h.join("Library/Logs/EVA01")).unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "eva-worker.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // Leaking the guard is deliberate: it must live for the process's
    // entire lifetime to keep flushing the non-blocking writer, and this
    // process has no earlier point to store it that would outlive `main`.
    std::mem::forget(guard);

    tracing_subscriber::fmt().with_env_filter(log_filter()).with_writer(non_blocking).with_ansi(false).init();
}

/// INFO for EVA01's own lines and the transcriptions, WARN for the speech
/// runtime's per-load chatter (a day of it was 13 MB, burying the few lines
/// that say what happened). `RUST_LOG` overrides all of it.
fn log_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,ort=warn,transcribe_rs::onnx::session=warn,transcribe_rs::onnx::canary::vocab=warn")
    })
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, msg: &WorkerToShell) -> std::io::Result<()> {
    let line = eva_ipc::encode_line(msg).map_err(std::io::Error::other)?;
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}
