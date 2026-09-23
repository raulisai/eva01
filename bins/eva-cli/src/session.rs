//! One conversation with a freshly spawned `eva-worker`: start it, wait for
//! its `Ready`, send one command, print what comes back until that request
//! is over, and shut it down. It does not look for a worker `eva-shell`
//! already started — `eva-store`'s SQLite file is safe to open concurrently
//! (WAL mode), so a one-shot CLI invocation and the running app coexist even
//! though they are two separate workers.

use crate::render::{self, Line};
use eva_ipc::{ShellToWorker, WorkerState, WorkerToShell};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

/// Spawns a worker, sends `command`, prints every event for `request_id`
/// until a terminal one arrives (or `timeout` elapses), then shuts the
/// worker down. Returns the process exit code: `0` on success, `1` if the
/// command itself reported an error, `2` on a CLI/transport-level failure
/// (couldn't spawn the worker, malformed protocol, timeout).
pub async fn run(command: ShellToWorker, request_id: Uuid, timeout: Duration) -> i32 {
    let events = collect(command, request_id, timeout, |line| line.print()).await;
    events.exit_code
}

/// What a session produced.
pub struct Outcome {
    /// Every event for the request, in order.
    pub events: Vec<WorkerToShell>,
    /// `0`, `1` or `2`, as described on [`run`].
    pub exit_code: i32,
}

/// Like [`run`], but hands each rendered line to `show` and returns the
/// events too — `eva doctor` uses the health report, not the printout.
pub async fn collect(
    command: ShellToWorker,
    request_id: Uuid,
    timeout: Duration,
    mut show: impl FnMut(&Line),
) -> Outcome {
    let mut events = Vec::new();

    let mut child = match tokio::process::Command::new(worker_binary_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("no se pudo iniciar eva-worker: {e}");
            return Outcome { events, exit_code: 2 };
        }
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        eprintln!("no se pudo abrir la entrada/salida de eva-worker");
        return Outcome { events, exit_code: 2 };
    };
    let mut lines = BufReader::new(stdout).lines();

    // The first line out of a healthy worker is always `Ready` — wait for
    // it before sending anything, so the real command is never lost to a
    // startup race. Loading a speech model can take several seconds.
    match tokio::time::timeout(Duration::from_secs(60), lines.next_line()).await {
        Ok(Ok(Some(line))) => match eva_ipc::decode_line::<WorkerToShell>(&line) {
            Ok(WorkerToShell::Ready) => {}
            Ok(other) => eprintln!("aviso: se esperaba 'ready', llegó otra cosa primero: {other:?}"),
            Err(e) => {
                eprintln!("eva-worker no habló el protocolo esperado al iniciar: {e}");
                return Outcome { events, exit_code: 2 };
            }
        },
        _ => {
            eprintln!("eva-worker no respondió a tiempo al iniciar");
            return Outcome { events, exit_code: 2 };
        }
    }

    if send(&mut stdin, &command).await.is_err() {
        eprintln!("no se pudo enviar el comando a eva-worker");
        return Outcome { events, exit_code: 2 };
    }

    let mut exit_code = 0;
    let read_loop = async {
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let Ok(event) = eva_ipc::decode_line::<WorkerToShell>(&line) else {
                        eprintln!("línea no reconocida de eva-worker, se ignora");
                        continue;
                    };

                    if let WorkerToShell::ConfirmationRequested { confirmation_id, title, detail, timeout_secs } =
                        &event
                    {
                        let approved = ask_yes_no(title, detail, *timeout_secs).await;
                        let answer =
                            ShellToWorker::ConfirmationResponse { confirmation_id: *confirmation_id, approved };
                        if send(&mut stdin, &answer).await.is_err() {
                            exit_code = 2;
                            break;
                        }
                        continue;
                    }

                    if let Some(line) = render::render(&event) {
                        if line.is_error {
                            exit_code = 1;
                        }
                        show(&line);
                    }
                    let finished = is_terminal(&event, request_id);
                    events.push(event);
                    if finished {
                        break;
                    }
                }
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

    let _ = send(&mut stdin, &ShellToWorker::Shutdown).await;
    let _ = stdin.flush().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;

    Outcome { events, exit_code }
}

async fn send(stdin: &mut tokio::process::ChildStdin, command: &ShellToWorker) -> std::io::Result<()> {
    let line = eva_ipc::encode_line(command).map_err(std::io::Error::other)?;
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await
}

/// Asks the person at the terminal, the way the overlay would ask them on
/// screen — and, like the overlay, never on their behalf: without a
/// terminal to ask on, the answer is no.
async fn ask_yes_no(title: &str, detail: &str, timeout_secs: u64) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!("⚠ Confirmación necesaria ({title}), pero no hay una terminal interactiva: se rechaza.");
        return false;
    }
    eprintln!("\n⚠ {title}\n  {detail}");
    eprint!("¿Lo hago? [s/N] ({timeout_secs} s): ");

    let mut answer = String::new();
    let read = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        BufReader::new(tokio::io::stdin()).read_line(&mut answer),
    )
    .await;
    matches!(read, Ok(Ok(n)) if n > 0) && render::is_yes(&answer)
}

/// Whether `event` is the last one this CLI invocation should wait for.
pub fn is_terminal(event: &WorkerToShell, request_id: Uuid) -> bool {
    match event {
        WorkerToShell::StateChanged { state, request_id: Some(rid) } => {
            *rid == request_id && matches!(state, WorkerState::Done(_) | WorkerState::Idle)
        }
        WorkerToShell::CustomWords { request_id: rid, .. }
        | WorkerToShell::Health { request_id: rid, .. }
        | WorkerToShell::TaskList { request_id: rid, .. }
        | WorkerToShell::Ack { request_id: rid } => *rid == request_id,
        WorkerToShell::Error { request_id: Some(rid), recoverable, .. } => *rid == request_id && !recoverable,
        _ => false,
    }
}

/// The worker binary sits next to this one — same layout as `bins/eva-shell`
/// uses, whether that is a plain `cargo build` `target/` directory or
/// `packaging/build-app.sh`'s `EVA01.app/Contents/MacOS/`.
pub fn worker_binary_path() -> PathBuf {
    sibling_binary("eva-worker")
}

/// Another EVA01 binary in the directory this one runs from.
pub fn sibling_binary(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eva"));
    path.set_file_name(name);
    path
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use eva_ipc::HealthReport;

    #[test]
    fn done_state_change_for_our_request_is_terminal() {
        let request_id = Uuid::new_v4();
        let event = WorkerToShell::StateChanged { state: WorkerState::Done(true), request_id: Some(request_id) };
        assert!(is_terminal(&event, request_id));
    }

    #[test]
    fn idle_for_our_request_is_terminal_too() {
        // Silence dropped by the worker's gate, or a cancel: nothing more is coming.
        let request_id = Uuid::new_v4();
        let event = WorkerToShell::StateChanged { state: WorkerState::Idle, request_id: Some(request_id) };
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
        let recoverable = WorkerToShell::Error { request_id: Some(request_id), message: "x".into(), recoverable: true };
        let fatal = WorkerToShell::Error { request_id: Some(request_id), message: "x".into(), recoverable: false };
        assert!(!is_terminal(&recoverable, request_id));
        assert!(is_terminal(&fatal, request_id));
    }

    #[test]
    fn replies_to_our_own_query_are_terminal() {
        let request_id = Uuid::new_v4();
        assert!(is_terminal(&WorkerToShell::CustomWords { request_id, words: vec![] }, request_id));
        assert!(is_terminal(&WorkerToShell::Health { request_id, report: HealthReport::default() }, request_id));
        assert!(is_terminal(&WorkerToShell::TaskList { request_id, tasks: vec![] }, request_id));
        assert!(is_terminal(&WorkerToShell::Ack { request_id }, request_id));
        assert!(!is_terminal(&WorkerToShell::Ack { request_id: Uuid::new_v4() }, request_id));
    }

    #[test]
    fn ready_transcript_intent_agent_and_task_events_are_never_terminal_on_their_own() {
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
        assert!(!is_terminal(&WorkerToShell::AgentEvent { request_id, event_json: serde_json::json!({}) }, request_id));
        assert!(
            !is_terminal(
                &WorkerToShell::TaskFinished { request_id, success: true, summary: String::new() },
                request_id
            ),
            "the Done state follows it"
        );
    }
}
