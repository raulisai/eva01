//! `eva`: the CLI from `docs/PLAN.md` §3.1 — "toda orden de voz debe ser
//! expresable como comando... esto es lo que hace testeable la capa de
//! intención sin grabar audio." Before this binary existed, exercising
//! `eva-worker`'s protocol meant hand-crafting JSON-lines over its stdin,
//! which is exactly how this project's own manual verification worked all
//! session — this closes that gap with a real, friendly entry point instead.
//!
//! Spawns a fresh `eva-worker` for the single command given, prints its
//! events as they arrive, and shuts it down cleanly on exit. It does not
//! attempt to find or reuse an already-running worker (e.g. one `eva-shell`
//! started) — `eva-store`'s SQLite file is safe to open concurrently (WAL
//! mode, enabled since `docs/PLAN.md` fase 3), so a one-shot CLI invocation
//! and a long-running app can coexist without corrupting each other's data,
//! even though they are two separate worker processes.

use clap::{Parser, Subcommand};
use eva_ipc::{ShellToWorker, WorkerState, WorkerToShell};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "eva", about = "EVA01 — la capa de voz, desde la línea de comandos", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Cuánto esperar una respuesta antes de rendirse, en segundos. Las
    /// tareas de agente pueden tardar minutos de verdad; los comandos
    /// simples (intent, health, dictionary) responden casi al instante.
    #[arg(long, global = true, default_value_t = 120)]
    timeout_secs: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Interpreta un texto como si acabara de ser transcrito — el mismo
    /// camino que usa el dictado real, sin necesitar audio ni micrófono.
    Intent {
        /// El texto a interpretar, p. ej. "Adán, abre brave".
        text: String,
    },
    /// Pide el reporte de salud de eva-worker.
    Health,
    /// Administra el diccionario personal.
    Dictionary {
        #[command(subcommand)]
        action: DictionaryAction,
    },
    /// Cambia la palabra de activación (aplica al próximo inicio).
    WakeWord {
        /// La nueva palabra de activación.
        word: String,
    },
}

#[derive(Subcommand)]
enum DictionaryAction {
    /// Agrega una palabra.
    Add {
        /// La palabra a agregar, en su forma preferida (p. ej. "García").
        word: String,
    },
    /// Quita una palabra.
    Remove {
        /// La palabra a quitar.
        word: String,
    },
    /// Lista todas las palabras del diccionario.
    List,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let request_id = Uuid::new_v4();

    let command = match cli.command {
        Command::Intent { text } => ShellToWorker::RunIntentText { request_id, text },
        Command::Health => ShellToWorker::HealthCheck { request_id },
        Command::WakeWord { word } => ShellToWorker::SetWakeWord { request_id, word },
        Command::Dictionary { action } => match action {
            DictionaryAction::Add { word } => ShellToWorker::AddCustomWord { request_id, word },
            DictionaryAction::Remove { word } => ShellToWorker::RemoveCustomWord { request_id, word },
            DictionaryAction::List => ShellToWorker::ListCustomWords { request_id },
        },
    };

    let exit_code = run(command, request_id, Duration::from_secs(cli.timeout_secs)).await;
    std::process::exit(exit_code);
}

/// Spawns a worker, sends `command`, prints every event for `request_id`
/// until a terminal one arrives (or `timeout` elapses), then shuts the
/// worker down. Returns the process exit code: `0` on success, `1` if the
/// command itself reported an error, `2` on a CLI/transport-level failure
/// (couldn't spawn the worker, malformed protocol, timeout).
async fn run(command: ShellToWorker, request_id: Uuid, timeout: Duration) -> i32 {
    let mut child = match tokio::process::Command::new(worker_binary_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("no se pudo iniciar eva-worker: {e}");
            return 2;
        }
    };

    let Some(mut stdin) = child.stdin.take() else {
        eprintln!("no se pudo abrir la entrada de eva-worker");
        return 2;
    };
    let Some(stdout) = child.stdout.take() else {
        eprintln!("no se pudo abrir la salida de eva-worker");
        return 2;
    };
    let mut lines = BufReader::new(stdout).lines();

    // The first line out of a healthy worker is always `Ready` — wait for
    // it before sending anything, so the real command is never lost to a
    // startup race.
    match tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await {
        Ok(Ok(Some(line))) => match eva_ipc::decode_line::<WorkerToShell>(&line) {
            Ok(WorkerToShell::Ready) => {}
            Ok(other) => eprintln!("aviso: se esperaba 'ready', llegó otra cosa primero: {other:?}"),
            Err(e) => {
                eprintln!("eva-worker no habló el protocolo esperado al iniciar: {e}");
                return 2;
            }
        },
        _ => {
            eprintln!("eva-worker no respondió a tiempo al iniciar");
            return 2;
        }
    }

    let Ok(encoded) = eva_ipc::encode_line(&command) else {
        eprintln!("no se pudo codificar el comando");
        return 2;
    };
    if let Err(e) = stdin.write_all(encoded.as_bytes()).await {
        eprintln!("no se pudo enviar el comando a eva-worker: {e}");
        return 2;
    }

    let mut exit_code = 0;
    let read_loop = async {
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match eva_ipc::decode_line::<WorkerToShell>(&line) {
                    Ok(event) => {
                        let is_error = print_event(&event);
                        if is_error {
                            exit_code = 1;
                        }
                        if is_terminal(&event, request_id) {
                            break;
                        }
                    }
                    Err(e) => eprintln!("línea no reconocida de eva-worker, se ignora: {e}"),
                },
                Ok(None) => {
                    eprintln!("eva-worker cerró su salida antes de responder");
                    exit_code = 2;
                    break;
                }
                Err(e) => {
                    eprintln!("error leyendo la salida de eva-worker: {e}");
                    exit_code = 2;
                    break;
                }
            }
        }
    };

    if tokio::time::timeout(timeout, read_loop).await.is_err() {
        eprintln!("se agotó el tiempo de espera ({timeout:?}) sin una respuesta final");
        exit_code = 2;
    }

    let shutdown_line = eva_ipc::encode_line(&ShellToWorker::Shutdown).unwrap_or_default();
    let _ = stdin.write_all(shutdown_line.as_bytes()).await;
    let _ = stdin.flush().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;

    exit_code
}

/// Prints one event in a human-readable form. Returns `true` if it was an
/// unrecoverable-looking problem worth a non-zero exit code.
fn print_event(event: &WorkerToShell) -> bool {
    match event {
        WorkerToShell::Ready => false,
        WorkerToShell::StateChanged { .. } => false, // internal bookkeeping; not interesting on a CLI
        WorkerToShell::Transcript { raw, cleaned, .. } => {
            println!("transcript crudo:     {raw}");
            println!("transcript limpio:    {cleaned}");
            false
        }
        WorkerToShell::IntentRecognized { intent_json, .. } => {
            println!("intent: {intent_json}");
            false
        }
        WorkerToShell::AgentEvent { event_json, .. } => {
            println!("agente: {event_json}");
            false
        }
        WorkerToShell::Error { message, recoverable, .. } => {
            eprintln!("error{}: {message}", if *recoverable { "" } else { " (fatal)" });
            true
        }
        WorkerToShell::Health { report, .. } => {
            println!("modelo STT cargado: {}", report.stt_model_loaded);
            if let Some(id) = &report.stt_model_id {
                println!("modelo:             {id}");
            }
            println!("base de datos OK:   {}", report.store_ok);
            false
        }
        WorkerToShell::CustomWords { words, .. } => {
            if words.is_empty() {
                println!("(el diccionario personal está vacío)");
            } else {
                for word in words {
                    println!("- {word}");
                }
            }
            false
        }
        WorkerToShell::Ack { .. } => {
            println!("listo.");
            false
        }
    }
}

/// Whether `event` is the last one this CLI invocation should wait for.
fn is_terminal(event: &WorkerToShell, request_id: Uuid) -> bool {
    match event {
        WorkerToShell::StateChanged { state, request_id: Some(rid) } => {
            *rid == request_id && matches!(state, WorkerState::Done(_))
        }
        WorkerToShell::CustomWords { request_id: rid, .. } => *rid == request_id,
        WorkerToShell::Health { request_id: rid, .. } => *rid == request_id,
        WorkerToShell::Ack { request_id: rid } => *rid == request_id,
        WorkerToShell::Error { request_id: Some(rid), recoverable, .. } => *rid == request_id && !recoverable,
        _ => false,
    }
}

/// The worker binary sits next to this one — same layout as
/// `bins/eva-shell` uses, whether that is a plain `cargo build` `target/`
/// directory or `packaging/build-app.sh`'s `EVA01.app/Contents/MacOS/`.
fn worker_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eva"));
    path.set_file_name("eva-worker");
    path
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn done_state_change_for_our_request_is_terminal() {
        let request_id = Uuid::new_v4();
        let event = WorkerToShell::StateChanged { state: WorkerState::Done(true), request_id: Some(request_id) };
        assert!(is_terminal(&event, request_id));
    }

    #[test]
    fn state_change_for_a_different_request_is_not_terminal() {
        let request_id = Uuid::new_v4();
        let event = WorkerToShell::StateChanged { state: WorkerState::Done(true), request_id: Some(Uuid::new_v4()) };
        assert!(!is_terminal(&event, request_id));
    }

    #[test]
    fn non_done_state_change_is_not_terminal() {
        let request_id = Uuid::new_v4();
        let event = WorkerToShell::StateChanged { state: WorkerState::Thinking, request_id: Some(request_id) };
        assert!(!is_terminal(&event, request_id));
    }

    #[test]
    fn a_recoverable_error_is_not_terminal_but_a_fatal_one_is() {
        let request_id = Uuid::new_v4();
        let recoverable =
            WorkerToShell::Error { request_id: Some(request_id), message: "x".into(), recoverable: true };
        let fatal = WorkerToShell::Error { request_id: Some(request_id), message: "x".into(), recoverable: false };
        assert!(!is_terminal(&recoverable, request_id));
        assert!(is_terminal(&fatal, request_id));
    }

    #[test]
    fn custom_words_health_and_ack_are_terminal_for_their_own_request() {
        let request_id = Uuid::new_v4();
        assert!(is_terminal(&WorkerToShell::CustomWords { request_id, words: vec![] }, request_id));
        assert!(is_terminal(
            &WorkerToShell::Health { request_id, report: eva_ipc::HealthReport::default() },
            request_id
        ));
        assert!(is_terminal(&WorkerToShell::Ack { request_id }, request_id));
    }

    #[test]
    fn ready_transcript_intent_and_agent_events_are_never_terminal_on_their_own() {
        let request_id = Uuid::new_v4();
        assert!(!is_terminal(&WorkerToShell::Ready, request_id));
        assert!(!is_terminal(
            &WorkerToShell::Transcript { request_id, raw: "a".into(), cleaned: "A.".into() },
            request_id
        ));
        assert!(!is_terminal(
            &WorkerToShell::IntentRecognized { request_id, intent_json: serde_json::json!({}) },
            request_id
        ));
        assert!(!is_terminal(
            &WorkerToShell::AgentEvent { request_id, event_json: serde_json::json!({}) },
            request_id
        ));
    }

    #[test]
    fn print_event_flags_errors_and_only_errors_as_exit_worthy() {
        assert!(!print_event(&WorkerToShell::Ready));
        assert!(!print_event(&WorkerToShell::Ack { request_id: Uuid::new_v4() }));
        assert!(print_event(&WorkerToShell::Error {
            request_id: None,
            message: "x".into(),
            recoverable: true
        }));
    }
}
