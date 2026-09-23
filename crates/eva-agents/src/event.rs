//! The normalized event vocabulary from `docs/PLAN.md` fase 6: whatever
//! `codex exec --json` or `claude -p --output-format stream-json` actually
//! emit gets translated into one of these, so the overlay and the task panel
//! never need to know which agent is running.

use serde::Serialize;

/// A normalized progress event from a running agent task.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent's process started and produced its first output.
    Started,
    /// The CLI reported the id of the session it is running in. Codex
    /// assigns its own (`thread.started`'s `thread_id`) — EVA cannot choose
    /// it the way it can for Claude (`--session-id`) — so this is what
    /// "continúa" must save to resume the right conversation later.
    SessionAssigned {
        /// The session id, as the CLI reports it.
        id: String,
    },
    /// A plain-text message from the agent (its reasoning or response text).
    Message {
        /// The message text.
        text: String,
    },
    /// The agent invoked a tool (ran a shell command, edited a file, …).
    ToolCall {
        /// The tool's name, as the underlying CLI reports it.
        name: String,
        /// A short, human-readable summary of the call's input, for the panel.
        summary: String,
    },
    /// A file was created or modified as a result of a tool call.
    FileChanged {
        /// The path that changed, as reported by the agent.
        path: String,
    },
    /// The agent is waiting for a human decision before continuing (e.g. a
    /// sandbox escalation request). Per `docs/PLAN.md` §6, approvals are
    /// surfaced by the overlay for a click or hotkey — never voice.
    ApprovalRequired {
        /// What the agent wants to do that needs approval.
        description: String,
    },
    /// The agent finished successfully.
    Completed {
        /// The agent's final summary message, if it gave one.
        summary: Option<String>,
    },
    /// The agent's process exited with an error, or its output could not be
    /// understood.
    Failed {
        /// A human-readable description of what went wrong.
        message: String,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn every_variant_serializes_to_a_tagged_json_shape() {
        let cases = vec![
            (AgentEvent::Started, serde_json::json!({"kind": "started"})),
            (
                AgentEvent::SessionAssigned { id: "01a0c7b4".into() },
                serde_json::json!({"kind": "session_assigned", "id": "01a0c7b4"}),
            ),
            (AgentEvent::Message { text: "hola".into() }, serde_json::json!({"kind": "message", "text": "hola"})),
            (
                AgentEvent::ToolCall { name: "Bash".into(), summary: "npm test".into() },
                serde_json::json!({"kind": "tool_call", "name": "Bash", "summary": "npm test"}),
            ),
            (
                AgentEvent::FileChanged { path: "src/main.rs".into() },
                serde_json::json!({"kind": "file_changed", "path": "src/main.rs"}),
            ),
            (
                AgentEvent::ApprovalRequired { description: "escribir fuera del proyecto".into() },
                serde_json::json!({"kind": "approval_required", "description": "escribir fuera del proyecto"}),
            ),
            (
                AgentEvent::Completed { summary: Some("3 archivos".into()) },
                serde_json::json!({"kind": "completed", "summary": "3 archivos"}),
            ),
            (
                AgentEvent::Failed { message: "no such file".into() },
                serde_json::json!({"kind": "failed", "message": "no such file"}),
            ),
        ];

        for (event, expected) in cases {
            let json = serde_json::to_value(&event).expect("AgentEvent must always serialize");
            assert_eq!(json, expected, "mismatch for {event:?}");
        }
    }
}
