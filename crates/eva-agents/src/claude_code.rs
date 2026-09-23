//! The Claude Code provider: runs `claude -p --output-format stream-json
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
//!
//! Resuming: `claude --resume <id>` continues that session in place. It must
//! **not** be combined with `--session-id` — the real CLI rejects the pair
//! ("--session-id can only be used with --continue or --resume if
//! --fork-session is also specified", found by running it, not from its
//! docs) — so a resumed task passes only `--resume`, and a fresh one passes
//! only `--session-id` with the id EVA assigned.

use crate::event::AgentEvent;
use crate::provider::{AgentError, AgentProvider, AgentTask, McpInjection, ProviderStatus, RunningAgent};
use crate::stream::{detect_cli, launch};
use async_trait::async_trait;
use serde::Deserialize;
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
        // `claude doctor` exits non-zero when there is no usable
        // authentication; a zero exit is treated as "has an active session".
        detect_cli(BINARY, &["doctor"]).await
    }

    async fn execute(&self, task: &AgentTask, events: UnboundedSender<AgentEvent>) -> Result<RunningAgent, AgentError> {
        launch("claude_code", build_command(task), events, parse_line)
    }
}

/// The exact command line for `task`. Separate from [`ClaudeCodeProvider::execute`]
/// so the flag combinations — the part that broke against the real CLI —
/// are unit-testable without spawning anything.
fn build_command(task: &AgentTask) -> Command {
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
        .current_dir(&task.project_dir);

    match task.resume_session_id {
        Some(resume_id) => command.arg("--resume").arg(resume_id.to_string()),
        None => command.arg("--session-id").arg(task.session_id.to_string()),
    };

    if let Some(mcp) = &task.mcp {
        // Added on top of whatever MCP servers the user already has (no
        // `--strict-mcp-config`): EVA's tools are an addition to the
        // agent's world, not a replacement of it. `mcp__eva` allows every
        // tool of that server — without it, `-p` mode has nobody to answer
        // the permission prompt and the agent's calls would just be denied.
        command
            .arg("--mcp-config")
            .arg(mcp_config_json(mcp))
            .arg("--allowedTools")
            .arg(format!("mcp__{}", McpInjection::SERVER_NAME));
    }

    command
}

fn mcp_config_json(mcp: &McpInjection) -> String {
    let env: serde_json::Map<String, serde_json::Value> =
        mcp.env.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    serde_json::json!({
        "mcpServers": {
            McpInjection::SERVER_NAME: {
                "command": mcp.command,
                "args": mcp.args,
                "env": env,
            }
        }
    })
    .to_string()
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
        RawEvent::System { session_id } => {
            let mut events = vec![AgentEvent::Started];
            events.extend(session_id.map(|id| AgentEvent::SessionAssigned { id }));
            events
        }
        RawEvent::Assistant { message } => message
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(AgentEvent::Message { text }),
                ContentBlock::ToolUse { name, input } => {
                    Some(AgentEvent::ToolCall { name, summary: summarize_tool_input(&input) })
                }
                ContentBlock::Other => None,
            })
            .collect(),
        RawEvent::Result { is_error, result } => {
            if is_error {
                vec![AgentEvent::Failed {
                    message: result.unwrap_or_else(|| "el agente reportó un error".to_string())
                }]
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
    System {
        #[serde(default)]
        session_id: Option<String>,
    },
    Assistant {
        message: AssistantMessage,
    },
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
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    const REAL_CAPTURE: &str = include_str!("../tests/fixtures/claude_stream_sample.jsonl");

    fn task(resume: Option<Uuid>, mcp: Option<McpInjection>) -> AgentTask {
        AgentTask {
            prompt: "arregla el login".to_string(),
            project_dir: PathBuf::from("/repos/iam"),
            session_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("uuid"),
            resume_session_id: resume,
            mcp,
        }
    }

    fn args_of(command: &Command) -> Vec<String> {
        command.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_fresh_task_passes_the_session_id_eva_assigned() {
        let args = args_of(&build_command(&task(None, None)));
        let position = args.iter().position(|a| a == "--session-id").expect("must pass --session-id");
        assert_eq!(args[position + 1], "11111111-1111-1111-1111-111111111111");
        assert!(!args.contains(&"--resume".to_string()));
    }

    #[test]
    fn a_resumed_task_passes_only_resume_never_session_id_together() {
        // The real CLI rejects `--resume` + `--session-id` without
        // `--fork-session`; this is the regression test for that.
        let resume = Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("uuid");
        let args = args_of(&build_command(&task(Some(resume), None)));
        let position = args.iter().position(|a| a == "--resume").expect("must pass --resume");
        assert_eq!(args[position + 1], "22222222-2222-2222-2222-222222222222");
        assert!(!args.contains(&"--session-id".to_string()), "combining them makes the CLI refuse to start");
    }

    #[test]
    fn the_prompt_comes_right_after_dash_p_so_variadic_flags_cannot_swallow_it() {
        let args = args_of(&build_command(&task(None, None)));
        assert_eq!(&args[..2], ["-p", "arregla el login"]);
    }

    #[test]
    fn without_mcp_no_mcp_flags_are_passed() {
        let args = args_of(&build_command(&task(None, None)));
        assert!(!args.iter().any(|a| a.contains("mcp")));
    }

    #[test]
    fn mcp_is_injected_per_invocation_and_its_tools_pre_approved() {
        let mcp = McpInjection {
            command: PathBuf::from("/Apps/EVA01.app/Contents/MacOS/eva-mcp"),
            args: vec!["--gateway".into(), "/tmp/eva.sock".into()],
            env: vec![("EVA_GATEWAY_TOKEN".into(), "abc".into())],
        };
        let args = args_of(&build_command(&task(None, Some(mcp))));

        let position = args.iter().position(|a| a == "--mcp-config").expect("must pass --mcp-config");
        let config: serde_json::Value = serde_json::from_str(&args[position + 1]).expect("must be valid JSON");
        assert_eq!(config["mcpServers"]["eva"]["command"], "/Apps/EVA01.app/Contents/MacOS/eva-mcp");
        assert_eq!(config["mcpServers"]["eva"]["args"], serde_json::json!(["--gateway", "/tmp/eva.sock"]));
        assert_eq!(config["mcpServers"]["eva"]["env"]["EVA_GATEWAY_TOKEN"], "abc");

        let allowed = args.iter().position(|a| a == "--allowedTools").expect("must pre-approve the tools");
        assert_eq!(args[allowed + 1], "mcp__eva");
        assert!(!args.contains(&"--strict-mcp-config".to_string()), "the user's own MCP servers stay available");
    }

    #[test]
    fn parses_every_line_of_the_real_captured_session_without_panicking() {
        for line in REAL_CAPTURE.lines() {
            let _ = parse_line(line);
        }
    }

    #[test]
    fn the_system_init_line_becomes_started_and_reports_the_session_id() {
        let first_line = REAL_CAPTURE.lines().next().expect("fixture has at least one line");
        assert_eq!(
            parse_line(first_line),
            vec![
                AgentEvent::Started,
                AgentEvent::SessionAssigned { id: "606e00e1-fe88-45b2-9157-8c8c6b6b63fd".to_string() }
            ]
        );
    }

    #[test]
    fn the_assistant_message_line_yields_its_text_content() {
        let assistant_line =
            REAL_CAPTURE.lines().find(|l| l.contains("\"type\":\"assistant\"")).expect("fixture has an assistant line");
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
        let result_line =
            REAL_CAPTURE.lines().find(|l| l.contains("\"type\":\"result\"")).expect("fixture has a result line");
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
        assert_eq!(parse_line(line), vec![AgentEvent::Completed { summary: Some("3 archivos cambiados".to_string()) }]);
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
