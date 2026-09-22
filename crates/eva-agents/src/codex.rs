//! The Codex provider: spawns `codex exec --json` and normalizes its JSONL
//! to [`AgentEvent`]s.
//!
//! [`parse_line`] is verified against a real, captured session, saved at
//! `tests/fixtures/codex_exec_sample.jsonl` (produced by literally running
//! `codex exec "di hola en una frase corta" --json --skip-git-repo-check`
//! on this machine on 2026-09-22, with stderr captured separately so the
//! fixture is exactly what `--json` puts on stdout). That run failed —
//! this account's configured default model needed a newer CLI — which,
//! again, is useful: it exercises `thread.started`, a top-level `error`,
//! `item.completed` with an error item, and `turn.failed`, all from one real
//! run.
//!
//! **Known gap, stated rather than papered over:** Codex's documented
//! protocol also has non-error `item.completed` shapes for an agent's text
//! output, shell commands it ran, and files it changed, which this parser
//! deliberately does not attempt to map yet — this fixture has no example
//! of any of them, and guessing field names for a wire format instead of
//! reading them off a real capture is exactly the kind of thing this
//! project's research kept finding bugs in (`docs/PLAN.md` §2A). Closing
//! this needs a Codex run that actually reaches a successful turn (a working
//! model config, i.e. not this environment's current `gpt-5.6-terra`
//! setting) to capture real samples of those item types before parsing them.
//! Until then, such lines fall through [`RawEvent::Unknown`] and are simply
//! skipped, per this parser's stated behavior for any unrecognized line.

use crate::event::AgentEvent;
use crate::provider::{AgentError, AgentOutcome, AgentProvider, AgentTask, ProviderStatus, RunningAgent};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// The Codex CLI binary name, resolved via `PATH`.
const BINARY: &str = "codex";

/// Talks to the `codex` CLI in non-interactive, streaming mode.
pub struct CodexProvider;

#[async_trait]
impl AgentProvider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    async fn detect(&self) -> ProviderStatus {
        let version = match Command::new(BINARY).arg("--version").output().await {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
            Ok(_) => None,
            Err(_) => return ProviderStatus::NotInstalled,
        };

        let has_session = Command::new(BINARY)
            .arg("login")
            .arg("status")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        match (version, has_session) {
            (Some(version), true) => ProviderStatus::Active { version },
            // The session check succeeded but `--version` did not parse —
            // treat it as active with an unknown version rather than
            // reporting "no session" when there plainly is one.
            (None, true) => ProviderStatus::Active { version: "desconocida".to_string() },
            (version, false) => ProviderStatus::InstalledNoSession { version },
        }
    }

    async fn execute(
        &self,
        task: &AgentTask,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<RunningAgent, AgentError> {
        let mut command = Command::new(BINARY);
        command
            .arg("exec")
            .arg(&task.prompt)
            .arg("--json")
            .arg("--skip-git-repo-check")
            .arg("-C")
            .arg(&task.project_dir)
            .arg("-s")
            .arg("workspace-write")
            .current_dir(&task.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()); // protocol lives on stdout only; see the module doc's capture notes

        if let Some(resume_id) = task.resume_session_id {
            command.arg("resume").arg(resume_id.to_string());
        }

        let mut child = command.spawn().map_err(|source| AgentError::Spawn { provider: "codex", source })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Io { provider: "codex", source: std::io::Error::other("no stdout pipe") })?;

        let _ = events.send(AgentEvent::Started);
        let output_task = tokio::spawn(read_loop(stdout, events));

        Ok(RunningAgent::new(child, output_task))
    }
}

async fn read_loop(stdout: tokio::process::ChildStdout, events: UnboundedSender<AgentEvent>) -> AgentOutcome {
    let mut lines = BufReader::new(stdout).lines();
    let mut outcome = AgentOutcome::Failed { message: "el proceso terminó sin emitir un resultado".to_string() };

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                for event in parse_line(&line) {
                    if let AgentEvent::Completed { summary } = &event {
                        outcome = AgentOutcome::Completed { summary: summary.clone() };
                    }
                    if let AgentEvent::Failed { message } = &event {
                        outcome = AgentOutcome::Failed { message: message.clone() };
                    }
                    let _ = events.send(event);
                }
            }
            Ok(None) => break,
            Err(e) => {
                outcome = AgentOutcome::Failed { message: format!("error leyendo la salida: {e}") };
                break;
            }
        }
    }

    outcome
}

/// Parses one line of `codex exec --json` output into zero or more
/// [`AgentEvent`]s. See the module doc for exactly what is and is not
/// covered. An unrecognized line yields no events rather than an error.
fn parse_line(line: &str) -> Vec<AgentEvent> {
    let Ok(value) = serde_json::from_str::<RawEvent>(line) else {
        return Vec::new();
    };

    match value {
        RawEvent::ThreadStarted { .. } => vec![AgentEvent::Started],
        RawEvent::TurnCompleted {} => vec![AgentEvent::Completed { summary: None }],
        RawEvent::TurnFailed { error } => vec![AgentEvent::Failed { message: error.message }],
        RawEvent::Error { message } => vec![AgentEvent::Failed { message }],
        RawEvent::ItemCompleted { item } if item.item_type == "error" => {
            vec![AgentEvent::Failed { message: item.message.unwrap_or_else(|| "error sin mensaje".to_string()) }]
        }
        // Every other `item.completed` shape (agent text, shell commands,
        // file changes) is the known gap from the module doc — skipped, not
        // guessed at.
        RawEvent::ItemCompleted { .. } | RawEvent::TurnStarted {} | RawEvent::Unknown => Vec::new(),
    }
}

/// The subset of Codex's `exec --json` shapes this parser understands.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { #[allow(dead_code)] thread_id: String },
    #[serde(rename = "turn.started")]
    TurnStarted {},
    #[serde(rename = "turn.completed")]
    TurnCompleted {},
    #[serde(rename = "turn.failed")]
    TurnFailed { error: CodexError },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: Item },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct CodexError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct Item {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    message: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    const REAL_CAPTURE: &str = include_str!("../tests/fixtures/codex_exec_sample.jsonl");

    #[test]
    fn parses_every_line_of_the_real_captured_session_without_panicking() {
        for line in REAL_CAPTURE.lines() {
            let _ = parse_line(line);
        }
    }

    #[test]
    fn thread_started_becomes_started() {
        let line = REAL_CAPTURE.lines().next().expect("fixture has at least one line");
        assert_eq!(parse_line(line), vec![AgentEvent::Started]);
    }

    #[test]
    fn the_error_item_becomes_failed() {
        let line = REAL_CAPTURE
            .lines()
            .find(|l| l.contains("\"item.completed\""))
            .expect("fixture has an item.completed line");
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::Failed { message } if message.contains("gpt-5.6-terra")));
    }

    #[test]
    fn the_top_level_error_becomes_failed() {
        let line = REAL_CAPTURE
            .lines()
            .find(|l| l == &r#"{"type":"error","message":"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-5.6-terra' model requires a newer version of Codex. Please upgrade to the latest app or CLI and try again.\"}}"}"#)
                .or_else(|| REAL_CAPTURE.lines().find(|l| l.starts_with(r#"{"type":"error""#)))
            .expect("fixture has a top-level error line");
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn turn_failed_becomes_failed() {
        let line = REAL_CAPTURE
            .lines()
            .find(|l| l.contains("\"turn.failed\""))
            .expect("fixture has a turn.failed line");
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn turn_started_is_informational_and_yields_no_event() {
        let line = r#"{"type":"turn.started"}"#;
        assert_eq!(parse_line(line), Vec::new());
    }

    #[test]
    fn turn_completed_becomes_completed() {
        let line = r#"{"type":"turn.completed"}"#;
        assert_eq!(parse_line(line), vec![AgentEvent::Completed { summary: None }]);
    }

    #[test]
    fn an_unmapped_item_type_is_skipped_not_guessed_at() {
        // Per the module doc: agent_message/command_execution/file_change
        // items are a known, stated gap until a live capture exists.
        let line = r#"{"type":"item.completed","item":{"id":"x","type":"agent_message","text":"hola"}}"#;
        assert_eq!(parse_line(line), Vec::new());
    }

    #[test]
    fn garbage_input_yields_no_events_instead_of_panicking() {
        assert_eq!(parse_line("no es json"), Vec::new());
        assert_eq!(parse_line(""), Vec::new());
    }

    proptest::proptest! {
        #[test]
        fn parse_line_never_panics_on_arbitrary_input(line in ".*") {
            let _ = parse_line(&line);
        }
    }
}
