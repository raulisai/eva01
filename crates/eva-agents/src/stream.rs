//! What every CLI-backed provider shares: spawning the child in its own
//! process group, turning its stdout JSON-lines into [`AgentEvent`]s through
//! the provider's own line parser, and deciding the final [`AgentOutcome`].
//! Providers only supply the command line and the parser — before this
//! module existed, `codex.rs` and `claude_code.rs` each carried a
//! near-identical copy of all of this.

use crate::event::AgentEvent;
use crate::provider::{AgentError, AgentOutcome, ProviderStatus, RunningAgent};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// Turns one line of a CLI's stdout into zero or more events. A line the
/// parser does not recognize yields no events — it must never fail the run.
pub(crate) type LineParser = fn(&str) -> Vec<AgentEvent>;

/// How much of the child's stderr to keep for the failure message: enough
/// for a usage error or a stack trace's head, not a whole log.
const STDERR_TAIL_BYTES: usize = 1_500;

/// Spawns `command` and returns a [`RunningAgent`] streaming its events.
///
/// The child gets its own process group, so cancelling it (see
/// [`RunningAgent::cancel`]) reaches the node/shell subprocesses an agent CLI
/// spawns, not just the CLI itself, and `kill_on_drop` so nothing outlives a
/// worker that shuts down normally.
pub(crate) fn launch(
    provider: &'static str,
    mut command: Command,
    events: UnboundedSender<AgentEvent>,
    parse: LineParser,
) -> Result<RunningAgent, AgentError> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|source| AgentError::Spawn { provider, source })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Io { provider, source: std::io::Error::other("no stdout pipe") })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::Io { provider, source: std::io::Error::other("no stderr pipe") })?;

    let _ = events.send(AgentEvent::Started);
    let stderr_task = tokio::spawn(read_tail(stderr));
    let output_task = tokio::spawn(read_loop(stdout, stderr_task, events, parse));
    Ok(RunningAgent::new(child, output_task))
}

async fn read_loop(
    stdout: tokio::process::ChildStdout,
    stderr_task: tokio::task::JoinHandle<String>,
    events: UnboundedSender<AgentEvent>,
    parse: LineParser,
) -> AgentOutcome {
    let mut lines = BufReader::new(stdout).lines();
    let mut outcome: Option<AgentOutcome> = None;
    let mut last_message: Option<String> = None;

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                for mut event in parse(&line) {
                    match &mut event {
                        AgentEvent::Message { text } => last_message = Some(text.clone()),
                        // Codex's `turn.completed` carries no text of its
                        // own; its last message is the closest thing to a
                        // summary, and is what gets spoken when it ends.
                        AgentEvent::Completed { summary } if summary.is_none() => summary.clone_from(&last_message),
                        _ => {}
                    }
                    match &event {
                        AgentEvent::Completed { summary } => {
                            outcome = Some(AgentOutcome::Completed { summary: summary.clone() });
                        }
                        AgentEvent::Failed { message } => {
                            outcome = Some(AgentOutcome::Failed { message: message.clone() });
                        }
                        _ => {}
                    }
                    let _ = events.send(event);
                }
            }
            Ok(None) => break,
            Err(e) => {
                outcome = Some(AgentOutcome::Failed { message: format!("error leyendo la salida: {e}") });
                break;
            }
        }
    }

    match outcome {
        Some(outcome) => outcome,
        None => {
            // The process ended without ever reporting a result — usually a
            // startup failure (bad flag, not logged in) whose only
            // explanation is on stderr. Surface it instead of a bare
            // "ended without a result" nobody can act on.
            let tail = stderr_task.await.unwrap_or_default();
            let message = if tail.trim().is_empty() {
                "el proceso terminó sin emitir un resultado".to_string()
            } else {
                format!("el proceso terminó sin emitir un resultado: {}", tail.trim())
            };
            let _ = events.send(AgentEvent::Failed { message: message.clone() });
            AgentOutcome::Failed { message }
        }
    }
}

/// Reads a stream to its end, keeping only the last [`STDERR_TAIL_BYTES`].
async fn read_tail<R: tokio::io::AsyncRead + Unpin>(mut reader: R) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.len() > STDERR_TAIL_BYTES {
                    let excess = buffer.len() - STDERR_TAIL_BYTES;
                    buffer.drain(..excess);
                }
            }
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Detects a CLI the way both providers do: `--version` proves it is
/// installed, `session_check` (their own `doctor`/`login status`) proves it
/// is logged in. Not installed and installed-but-signed-out are different
/// answers, because they need different instructions from the user.
pub(crate) async fn detect_cli(binary: &str, session_check: &[&str]) -> ProviderStatus {
    let version = match Command::new(binary).arg("--version").output().await {
        Ok(output) if output.status.success() => Some(String::from_utf8_lossy(&output.stdout).trim().to_string()),
        Ok(_) => None,
        Err(_) => return ProviderStatus::NotInstalled,
    };

    let has_session =
        Command::new(binary).args(session_check).output().await.map(|o| o.status.success()).unwrap_or(false);

    match (version, has_session) {
        (Some(version), true) => ProviderStatus::Active { version },
        // The session check succeeded but `--version` did not parse — treat
        // it as active with an unknown version rather than reporting "no
        // session" when there plainly is one.
        (None, true) => ProviderStatus::Active { version: "desconocida".to_string() },
        (version, false) => ProviderStatus::InstalledNoSession { version },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn parse_echo(line: &str) -> Vec<AgentEvent> {
        match line {
            "hola" => vec![AgentEvent::Message { text: "hola".into() }],
            "listo" => vec![AgentEvent::Completed { summary: None }],
            "fallo" => vec![AgentEvent::Failed { message: "roto".into() }],
            _ => Vec::new(),
        }
    }

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    async fn run(script: &str) -> (AgentOutcome, Vec<AgentEvent>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let running = launch("test", sh(script), tx, parse_echo).expect("sh must spawn");
        let outcome = running.wait().await;
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        (outcome, events)
    }

    #[tokio::test]
    async fn started_is_emitted_and_a_completed_line_ends_the_run_successfully() {
        let (outcome, events) = run("echo listo").await;
        assert_eq!(events.first(), Some(&AgentEvent::Started));
        assert_eq!(outcome, AgentOutcome::Completed { summary: None });
    }

    #[tokio::test]
    async fn a_completed_event_without_text_inherits_the_last_message_as_its_summary() {
        let (outcome, _events) = run("echo hola; echo listo").await;
        assert_eq!(outcome, AgentOutcome::Completed { summary: Some("hola".to_string()) });
    }

    #[tokio::test]
    async fn a_failed_line_ends_the_run_as_failed() {
        let (outcome, _events) = run("echo fallo").await;
        assert_eq!(outcome, AgentOutcome::Failed { message: "roto".to_string() });
    }

    #[tokio::test]
    async fn a_run_that_never_reports_a_result_surfaces_its_stderr() {
        let (outcome, events) = run("echo 'Error: bandera desconocida' >&2; exit 2").await;
        let AgentOutcome::Failed { message } = outcome else { panic!("expected a failure") };
        assert!(message.contains("bandera desconocida"), "the stderr tail must reach the user: {message}");
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Failed { .. })));
    }

    #[tokio::test]
    async fn a_run_with_no_output_at_all_still_fails_with_a_clear_message() {
        let (outcome, _events) = run("exit 0").await;
        assert!(matches!(outcome, AgentOutcome::Failed { message } if message.contains("sin emitir un resultado")));
    }

    #[tokio::test]
    async fn unrecognized_lines_are_skipped_not_fatal() {
        let (outcome, _events) = run("echo basura; echo mas basura; echo listo").await;
        assert_eq!(outcome, AgentOutcome::Completed { summary: None });
    }

    #[tokio::test]
    async fn spawning_a_missing_binary_is_a_typed_error() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = launch("test", Command::new("/definitivamente/no/existe"), tx, parse_echo);
        assert!(matches!(result, Err(AgentError::Spawn { provider: "test", .. })));
    }

    #[tokio::test]
    async fn cancel_terminates_the_whole_process_group_not_just_the_shell() {
        // `sh` forks `sleep` as a grandchild; killing only the shell would
        // orphan it. With the child in its own process group, cancel must
        // take both down.
        let marker = std::env::temp_dir().join(format!("eva-pg-{}", uuid::Uuid::new_v4()));
        let script = format!("sleep 30 & echo $! > {}; wait", marker.display());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let running = launch("test", sh(&script), tx, parse_echo).expect("sh must spawn");

        let grandchild: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&marker) {
                if let Ok(pid) = text.trim().parse() {
                    break pid;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert!(process_alive(grandchild), "the grandchild must be running before the cancel");

        assert_eq!(running.cancel().await, AgentOutcome::Cancelled);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!process_alive(grandchild), "cancel must also have stopped the shell's own subprocess");
        let _ = std::fs::remove_file(marker);
    }

    fn process_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks existence and permission; nothing is delivered.
        unsafe { libc::kill(pid, 0) == 0 }
    }
}
