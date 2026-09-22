//! The Claude Code provider: spawns `claude -p --output-format stream-json
//! --verbose` and normalizes its JSONL to [`AgentEvent`]s.
//!
//! [`parse_line`] is verified against a real, captured session, saved at
//! `tests/fixtures/claude_stream_sample.jsonl` (produced by literally running
//! `claude -p "Di hola en una frase corta" --output-format stream-json
//! --verbose --model claude-haiku-4-5-20251001` on 2026-09-22 and redirecting
//! its stdout — not hand-written JSON). That capture happened to fail
//! authentication (a nested `claude` invocation from inside another Claude
//! Code session does not inherit the parent's session), which is a bit of
//! luck: it means the fixture covers both the `system`/`assistant` shapes
//! *and* the `type: "result", is_error: true` failure shape, from one real
//! run. The `tool_use` content-block shape below is **not** independently
//! re-verified against a live captured tool call in this fixture — it
//! follows the standard Anthropic Messages API content-block schema that
//! `assistant.message.content` is documented to wrap (the same schema the
//! `text` blocks in the fixture already match exactly), so it is
//! well-founded, just not from this specific capture.

use crate::event::AgentEvent;
use crate::provider::{AgentError, AgentOutcome, AgentProvider, AgentTask, ProviderStatus, RunningAgent};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// The Claude Code CLI binary name, resolved via `PATH`.
const BINARY: &str = "claude";

/// Talks to the `claude` CLI in non-interactive, streaming mode.
pub struct ClaudeCodeProvider;

#[async_trait]
impl AgentProvider for ClaudeCodeProvider {
    fn id(&self) -> &'static str {
        "claude_code"
    }

    async fn detect(&self) -> ProviderStatus {
        let version = match Command::new(BINARY).arg("--version").output().await {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
            Ok(_) => None,
            Err(_) => return ProviderStatus::NotInstalled,
        };

        // `claude doctor` exits non-zero when there is no usable
        // authentication; a zero exit is treated as "has an active session".
        let has_session = Command::new(BINARY)
            .arg("doctor")
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
            .arg("-p")
            .arg(&task.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--add-dir")
            .arg(&task.project_dir)
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("--session-id")
            .arg(task.session_id.to_string())
            .current_dir(&task.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()); // protocol lives on stdout only; see the module doc's capture notes

        if let Some(resume_id) = task.resume_session_id {
            command.arg("--resume").arg(resume_id.to_string());
        }

        let mut child = command.spawn().map_err(|source| AgentError::Spawn { provider: "claude_code", source })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Io { provider: "claude_code", source: std::io::Error::other("no stdout pipe") })?;

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

/// Parses one line of `claude --output-format stream-json` output into zero
/// or more [`AgentEvent`]s. A line that is not recognized JSON, or is JSON
/// but not a shape this function knows about, yields no events rather than
/// an error — an unrecognized line should never take down the read loop.
fn parse_line(line: &str) -> Vec<AgentEvent> {
    let Ok(value) = serde_json::from_str::<RawEvent>(line) else {
        return Vec::new();
    };

    match value {
        RawEvent::System { .. } => vec![AgentEvent::Started],
        RawEvent::Assistant { message } => message
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(AgentEvent::Message { text }),
                ContentBlock::ToolUse { name, input } => Some(AgentEvent::ToolCall {
                    name,
                    summary: summarize_tool_input(&input),
                }),
                ContentBlock::Other => None,
            })
            .collect(),
        RawEvent::Result { is_error, result } => {
            if is_error {
                vec![AgentEvent::Failed { message: result.unwrap_or_else(|| "el agente reportó un error".to_string()) }]
            } else {
                vec![AgentEvent::Completed { summary: result }]
            }
        }
        RawEvent::Unknown => Vec::new(),
    }
}

fn summarize_tool_input(input: &serde_json::Value) -> String {
    let rendered = input.to_string();
    const MAX_LEN: usize = 120;
    if rendered.chars().count() > MAX_LEN {
        let truncated: String = rendered.chars().take(MAX_LEN).collect();
        format!("{truncated}…")
    } else {
        rendered
    }
}

/// The subset of Claude Code's `stream-json` shapes this parser understands,
/// matched loosely (`#[serde(other)]` catches everything else as `Unknown`
/// instead of failing to deserialize the whole line).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawEvent {
    System {},
    Assistant { message: AssistantMessage },
    Result {
        is_error: bool,
        result: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    ToolUse { name: String, #[serde(default)] input: serde_json::Value },
    #[serde(other)]
    Other,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    const REAL_CAPTURE: &str = include_str!("../tests/fixtures/claude_stream_sample.jsonl");

    #[test]
    fn parses_every_line_of_the_real_captured_session_without_panicking() {
        for line in REAL_CAPTURE.lines() {
            let _ = parse_line(line);
        }
    }

    #[test]
    fn the_system_init_line_becomes_started() {
        let first_line = REAL_CAPTURE.lines().next().expect("fixture has at least one line");
        assert_eq!(parse_line(first_line), vec![AgentEvent::Started]);
    }

    #[test]
    fn the_assistant_message_line_yields_its_text_content() {
        let assistant_line = REAL_CAPTURE
            .lines()
            .find(|l| l.contains("\"type\":\"assistant\""))
            .expect("fixture has an assistant line");
        let events = parse_line(assistant_line);
        assert_eq!(
            events,
            vec![AgentEvent::Message {
                text: "Failed to authenticate: OAuth session expired and could not be refreshed".to_string()
            }]
        );
    }

    #[test]
    fn the_terminal_result_line_becomes_failed_because_is_error_was_true() {
        let result_line = REAL_CAPTURE
            .lines()
            .find(|l| l.contains("\"type\":\"result\""))
            .expect("fixture has a result line");
        let events = parse_line(result_line);
        assert_eq!(
            events,
            vec![AgentEvent::Failed {
                message: "Failed to authenticate: OAuth session expired and could not be refreshed".to_string()
            }]
        );
    }

    #[test]
    fn a_successful_result_line_becomes_completed() {
        let line = r#"{"type":"result","is_error":false,"result":"3 archivos cambiados"}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::Completed { summary: Some("3 archivos cambiados".to_string()) }]
        );
    }

    #[test]
    fn a_tool_use_content_block_becomes_a_tool_call_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"npm test"}}]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::ToolCall { name, .. } if name == "Bash"));
    }

    #[test]
    fn a_message_with_both_text_and_a_tool_call_yields_both_events() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"voy a correr los tests"},
            {"type":"tool_use","name":"Bash","input":{"command":"npm test"}}
        ]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn garbage_input_yields_no_events_instead_of_panicking() {
        assert_eq!(parse_line("esto no es json"), Vec::new());
        assert_eq!(parse_line(""), Vec::new());
        assert_eq!(parse_line("{\"type\": \"something_unrecognized\"}"), Vec::new());
    }

    proptest::proptest! {
        #[test]
        fn parse_line_never_panics_on_arbitrary_input(line in ".*") {
            let _ = parse_line(&line);
        }
    }
}
