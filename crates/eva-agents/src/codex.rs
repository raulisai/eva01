//! The Codex provider: runs `codex exec --json` and normalizes its JSONL to
//! [`AgentEvent`]s.
//!
//! `parse_line` is verified two ways. The thread/turn/error lines are
//! checked against a real, captured session, saved at
//! `tests/fixtures/codex_exec_sample.jsonl` (produced by literally running
//! `codex exec "di hola en una frase corta" --json --skip-git-repo-check`
//! on this machine on 2026-09-22, with stderr captured separately so the
//! fixture is exactly what `--json` puts on stdout). That run failed — this
//! account's configured model needs a newer CLI — which is useful in its own
//! way: it exercises `thread.started`, a top-level `error`, `item.completed`
//! with an error item, and `turn.failed` from one real run.
//!
//! The non-error `item.completed` shapes (the agent's text, shell commands,
//! file changes, MCP calls, web searches) could not be captured live —
//! no run on this machine ever reaches a successful turn — so their field
//! names were read off the installed binary itself instead: the `codex`
//! executable's own string table carries the serde vocabulary of its exec
//! events (`agent_message`, `command_execution`, `file_change`,
//! `mcp_tool_call`, `web_search`, `todo_list`, `aggregated_output`,
//! `exit_code`, `changes` with `add`/`delete`/`update`, `path`, `server`,
//! `arguments`, `query`, statuses `in_progress`/`completed`/`failed`). Every
//! field of those shapes is optional here, so a shape that differs in detail
//! from this reading degrades to fewer events instead of a failed run.
//!
//! Resuming: `codex exec resume <SESSION_ID> <PROMPT>` takes the session id
//! Codex itself assigned (`thread.started`'s `thread_id` — EVA cannot choose
//! it), which is why [`AgentEvent::SessionAssigned`] exists. `resume` accepts
//! neither `-C` nor `-s` (checked with `codex exec resume --help`), so the
//! working directory is the child's own cwd and the sandbox goes through
//! `-c sandbox_mode=…`.

use crate::event::AgentEvent;
use crate::provider::{AgentError, AgentProvider, AgentTask, McpInjection, ProviderStatus, RunningAgent};
use crate::stream::{detect_cli, launch};
use async_trait::async_trait;
use serde::Deserialize;
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
        detect_cli(BINARY, &["login", "status"]).await
    }

    async fn execute(&self, task: &AgentTask, events: UnboundedSender<AgentEvent>) -> Result<RunningAgent, AgentError> {
        launch("codex", build_command(task), events, parse_line)
    }
}

/// The exact command line for `task` — separate from
/// [`CodexProvider::execute`] so the argument order (which differs between a
/// fresh run and a resume) is unit-testable without spawning anything.
fn build_command(task: &AgentTask) -> Command {
    let mut command = Command::new(BINARY);
    command.arg("exec");
    if task.resume_session_id.is_some() {
        command.arg("resume");
    }
    command.arg("--json").arg("--skip-git-repo-check");

    if task.resume_session_id.is_none() {
        command.arg("-C").arg(&task.project_dir).arg("-s").arg("workspace-write");
    } else {
        command.arg("-c").arg("sandbox_mode=\"workspace-write\"");
    }

    if let Some(mcp) = &task.mcp {
        for override_arg in mcp_overrides(mcp) {
            command.arg("-c").arg(override_arg);
        }
    }

    // `--` so a prompt that starts with a dash cannot be read as a flag.
    command.arg("--");
    if let Some(resume_id) = task.resume_session_id {
        command.arg(resume_id.to_string());
    }
    command.arg(&task.prompt).current_dir(&task.project_dir);
    command
}

/// The `-c key=value` overrides that register EVA's MCP server for this one
/// invocation only — Codex's counterpart of Claude's `--mcp-config`, and just
/// as reversible: nothing is written to `~/.codex/config.toml`. Values are
/// TOML; a JSON string is a valid TOML basic string for every path or token
/// this can carry.
fn mcp_overrides(mcp: &McpInjection) -> Vec<String> {
    let name = McpInjection::SERVER_NAME;
    let args = mcp.args.iter().map(|a| toml_string(a)).collect::<Vec<_>>().join(",");
    let env = mcp.env.iter().map(|(k, v)| format!("{k}={}", toml_string(v))).collect::<Vec<_>>().join(",");
    vec![
        format!("mcp_servers.{name}.command={}", toml_string(&mcp.command.to_string_lossy())),
        format!("mcp_servers.{name}.args=[{args}]"),
        format!("mcp_servers.{name}.env={{{env}}}"),
    ]
}

fn toml_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

/// Parses one line of `codex exec --json` output into zero or more
/// [`AgentEvent`]s. An unrecognized line yields no events rather than an
/// error.
fn parse_line(line: &str) -> Vec<AgentEvent> {
    let Ok(value) = serde_json::from_str::<RawEvent>(line) else {
        return Vec::new();
    };

    match value {
        RawEvent::ThreadStarted { thread_id } => {
            let mut events = vec![AgentEvent::Started];
            events.extend(thread_id.map(|id| AgentEvent::SessionAssigned { id }));
            events
        }
        RawEvent::TurnCompleted {} => vec![AgentEvent::Completed { summary: None }],
        RawEvent::TurnFailed { error } => vec![AgentEvent::Failed { message: error.message }],
        RawEvent::Error { message } => vec![AgentEvent::Failed { message }],
        RawEvent::ItemCompleted { item } => parse_item(item),
        RawEvent::TurnStarted {} | RawEvent::Unknown => Vec::new(),
    }
}

fn parse_item(item: Item) -> Vec<AgentEvent> {
    match item.item_type.as_str() {
        "error" => {
            vec![AgentEvent::Failed { message: item.message.unwrap_or_else(|| "error sin mensaje".to_string()) }]
        }
        "agent_message" => item.text.map(|text| AgentEvent::Message { text }).into_iter().collect(),
        "command_execution" => item
            .command
            .map(|command| AgentEvent::ToolCall { name: "shell".to_string(), summary: command })
            .into_iter()
            .collect(),
        "file_change" => {
            item.changes.into_iter().filter_map(|c| c.path).map(|path| AgentEvent::FileChanged { path }).collect()
        }
        "mcp_tool_call" => {
            let name = match (&item.server, &item.tool) {
                (Some(server), Some(tool)) => format!("{server}/{tool}"),
                (_, Some(tool)) => tool.clone(),
                _ => "mcp".to_string(),
            };
            vec![AgentEvent::ToolCall { name, summary: String::new() }]
        }
        "web_search" => item
            .query
            .map(|query| AgentEvent::ToolCall { name: "web_search".to_string(), summary: query })
            .into_iter()
            .collect(),
        // `reasoning` and `todo_list` are the agent thinking out loud, not
        // progress worth showing in a task panel; anything unknown is
        // skipped rather than guessed at.
        _ => Vec::new(),
    }
}

/// The subset of Codex's `exec --json` shapes this parser understands.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted {
        #[serde(default)]
        thread_id: Option<String>,
    },
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

/// One `item.completed` payload. Every field beyond `type` is optional: only
/// the item types above read any of them, and a shape that differs in detail
/// must degrade to "no event", never to a parse failure.
#[derive(Debug, Deserialize)]
struct Item {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    changes: Vec<FileChange>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileChange {
    #[serde(default)]
    path: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    const REAL_CAPTURE: &str = include_str!("../tests/fixtures/codex_exec_sample.jsonl");

    fn task(resume: Option<Uuid>, mcp: Option<McpInjection>) -> AgentTask {
        AgentTask {
            prompt: "arregla el login".to_string(),
            project_dir: PathBuf::from("/repos/iam"),
            session_id: Uuid::new_v4(),
            resume_session_id: resume,
            mcp,
        }
    }

    fn args_of(command: &Command) -> Vec<String> {
        command.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_fresh_task_runs_exec_in_the_project_with_a_workspace_write_sandbox() {
        let args = args_of(&build_command(&task(None, None)));
        assert_eq!(args[0], "exec");
        assert!(!args.contains(&"resume".to_string()));
        let c = args.iter().position(|a| a == "-C").expect("must pass -C");
        assert_eq!(args[c + 1], "/repos/iam");
        let s = args.iter().position(|a| a == "-s").expect("must pass -s");
        assert_eq!(args[s + 1], "workspace-write");
        assert_eq!(args.last().map(String::as_str), Some("arregla el login"));
    }

    #[test]
    fn a_resumed_task_puts_the_subcommand_first_and_the_id_before_the_prompt() {
        // `codex exec "prompt" … resume <id>` is not the CLI's grammar; the
        // real one is `codex exec resume <SESSION_ID> <PROMPT>`.
        let id = Uuid::parse_str("01a0c7b4-347f-7dd3-8108-1649df00c5a6").expect("uuid");
        let args = args_of(&build_command(&task(Some(id), None)));
        assert_eq!(&args[..2], ["exec", "resume"]);
        let tail = &args[args.len() - 3..];
        assert_eq!(tail, ["--", "01a0c7b4-347f-7dd3-8108-1649df00c5a6", "arregla el login"]);
    }

    #[test]
    fn a_resumed_task_avoids_the_flags_resume_does_not_accept() {
        let args = args_of(&build_command(&task(Some(Uuid::new_v4()), None)));
        assert!(!args.contains(&"-C".to_string()), "`codex exec resume` has no -C");
        assert!(!args.contains(&"-s".to_string()), "`codex exec resume` has no -s");
        assert!(args.contains(&"sandbox_mode=\"workspace-write\"".to_string()));
    }

    #[test]
    fn a_prompt_starting_with_a_dash_cannot_be_read_as_a_flag() {
        let mut t = task(None, None);
        t.prompt = "--help".to_string();
        let args = args_of(&build_command(&t));
        let dashes = args.iter().position(|a| a == "--").expect("must separate positionals");
        assert_eq!(args[dashes + 1], "--help");
    }

    #[test]
    fn mcp_is_registered_through_per_invocation_config_overrides() {
        let mcp = McpInjection {
            command: PathBuf::from("/Apps/EVA01.app/Contents/MacOS/eva-mcp"),
            args: vec!["--gateway".into(), "/tmp/eva.sock".into()],
            env: vec![("EVA_GATEWAY_TOKEN".into(), "abc".into())],
        };
        let overrides = mcp_overrides(&mcp);
        assert_eq!(overrides[0], r#"mcp_servers.eva.command="/Apps/EVA01.app/Contents/MacOS/eva-mcp""#);
        assert_eq!(overrides[1], r#"mcp_servers.eva.args=["--gateway","/tmp/eva.sock"]"#);
        assert_eq!(overrides[2], r#"mcp_servers.eva.env={EVA_GATEWAY_TOKEN="abc"}"#);

        let args = args_of(&build_command(&task(None, Some(mcp))));
        assert_eq!(args.iter().filter(|a| *a == "-c").count(), 3);
    }

    #[test]
    fn paths_with_quotes_or_backslashes_stay_valid_toml() {
        assert_eq!(toml_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn parses_every_line_of_the_real_captured_session_without_panicking() {
        for line in REAL_CAPTURE.lines() {
            let _ = parse_line(line);
        }
    }

    #[test]
    fn thread_started_becomes_started_and_reports_codexs_own_session_id() {
        let line = REAL_CAPTURE.lines().next().expect("fixture has at least one line");
        assert_eq!(
            parse_line(line),
            vec![
                AgentEvent::Started,
                AgentEvent::SessionAssigned { id: "01a0c7b4-347f-7dd3-8108-1649df00c5a6".to_string() }
            ]
        );
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
            .find(|l| l.starts_with(r#"{"type":"error""#))
            .expect("fixture has a top-level error line");
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn turn_failed_becomes_failed() {
        let line =
            REAL_CAPTURE.lines().find(|l| l.contains("\"turn.failed\"")).expect("fixture has a turn.failed line");
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn turn_started_is_informational_and_yields_no_event() {
        assert_eq!(parse_line(r#"{"type":"turn.started"}"#), Vec::new());
    }

    #[test]
    fn turn_completed_becomes_completed_and_the_stream_fills_in_the_summary() {
        assert_eq!(parse_line(r#"{"type":"turn.completed"}"#), vec![AgentEvent::Completed { summary: None }]);
    }

    #[test]
    fn an_agent_message_item_becomes_a_message() {
        let line = r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"listo, agregué los tests"}}"#;
        assert_eq!(parse_line(line), vec![AgentEvent::Message { text: "listo, agregué los tests".to_string() }]);
    }

    #[test]
    fn a_command_execution_item_becomes_a_shell_tool_call() {
        let line = r#"{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"npm test","aggregated_output":"ok","exit_code":0,"status":"completed"}}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::ToolCall { name: "shell".to_string(), summary: "npm test".to_string() }]
        );
    }

    #[test]
    fn a_file_change_item_yields_one_event_per_changed_path() {
        let line = r#"{"type":"item.completed","item":{"id":"item_3","type":"file_change","changes":[{"path":"src/a.rs","kind":"update"},{"path":"src/b.rs","kind":"add"}],"status":"completed"}}"#;
        assert_eq!(
            parse_line(line),
            vec![
                AgentEvent::FileChanged { path: "src/a.rs".to_string() },
                AgentEvent::FileChanged { path: "src/b.rs".to_string() }
            ]
        );
    }

    #[test]
    fn an_mcp_tool_call_item_names_the_server_and_tool() {
        let line = r#"{"type":"item.completed","item":{"id":"i","type":"mcp_tool_call","server":"eva","tool":"open_url","status":"completed"}}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::ToolCall { name: "eva/open_url".to_string(), summary: String::new() }]
        );
    }

    #[test]
    fn reasoning_and_unknown_item_types_are_skipped_not_guessed_at() {
        let reasoning = r#"{"type":"item.completed","item":{"id":"x","type":"reasoning","text":"pensando"}}"#;
        let unknown = r#"{"type":"item.completed","item":{"id":"x","type":"algo_nuevo"}}"#;
        assert_eq!(parse_line(reasoning), Vec::new());
        assert_eq!(parse_line(unknown), Vec::new());
    }

    #[test]
    fn an_item_missing_its_expected_fields_degrades_to_no_event() {
        let line = r#"{"type":"item.completed","item":{"id":"x","type":"command_execution"}}"#;
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
