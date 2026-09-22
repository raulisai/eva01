#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! Wire protocol between `eva-shell` and `eva-worker`.
//!
//! `eva-shell` is the process the user sees (tray icon, global hotkey,
//! overlay). It never touches the microphone, the STT engine, or any code
//! that can crash. `eva-worker` is a child process that does that work.
//! They talk over the worker's stdin/stdout, one JSON object per line
//! (JSON-lines): the shell writes [`ShellToWorker`] lines to the worker's
//! stdin, the worker writes [`WorkerToShell`] lines to its stdout.
//!
//! This crate only defines the messages and the line framing helpers. It has
//! no knowledge of how either side is implemented, so `eva-shell` and
//! `eva-worker` can change independently as long as they agree on this file.
//! See `docs/PLAN.md` §3.3 for why the two processes are split at all.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A command sent from `eva-shell` to `eva-worker`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellToWorker {
    /// The hotkey was pressed and held: start capturing audio.
    StartRecording {
        /// Correlates this recording with the [`WorkerToShell`] events it produces.
        request_id: uuid::Uuid,
    },
    /// The hotkey was released: stop capturing and begin processing.
    StopRecording {
        /// Must match the `request_id` of the [`ShellToWorker::StartRecording`] it closes.
        request_id: uuid::Uuid,
    },
    /// The user cancelled the in-flight request (e.g. pressed Escape).
    Cancel {
        /// The request being cancelled.
        request_id: uuid::Uuid,
    },
    /// Run the intent parser directly on a string, bypassing audio entirely.
    /// This is what `eva intent "abre brave"` uses on the CLI, and it is the
    /// same code path production dictation uses after transcription — so a
    /// text fixture test exercises real production logic, not a stand-in.
    RunIntentText {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
        /// The text to parse as if it had just been transcribed.
        text: String,
    },
    /// Ask the worker to report its health (models loaded, DB status, etc.).
    HealthCheck {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
    },
    /// Ask the worker to shut down cleanly before the shell terminates it.
    Shutdown,
}

/// An event sent from `eva-worker` to `eva-shell`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerToShell {
    /// The worker finished initializing (models loaded, DB open) and is ready
    /// to accept [`ShellToWorker`] commands.
    Ready,
    /// The worker's overall state changed; the shell renders this in the overlay.
    StateChanged {
        /// The new state.
        state: WorkerState,
        /// The request this state change belongs to, if any.
        request_id: Option<uuid::Uuid>,
    },
    /// A transcript was produced and cleaned.
    Transcript {
        /// The request this transcript belongs to.
        request_id: uuid::Uuid,
        /// The raw text straight out of the STT engine.
        raw: String,
        /// The text after filler-word removal and formatting.
        cleaned: String,
    },
    /// The intent parser classified the (possibly wake-word-prefixed) text.
    IntentRecognized {
        /// The request this intent belongs to.
        request_id: uuid::Uuid,
        /// The recognized intent, JSON-encoded by `eva-intent`.
        intent_json: serde_json::Value,
    },
    /// An agent (Codex, Claude Code, …) reported progress on a dispatched task.
    AgentEvent {
        /// The request this event belongs to.
        request_id: uuid::Uuid,
        /// The agent event, JSON-encoded by `eva-agents`.
        event_json: serde_json::Value,
    },
    /// Something went wrong. `recoverable = true` means the worker is still
    /// usable and the shell should just surface the message; `false` means
    /// the worker is about to exit and the shell should expect a restart.
    Error {
        /// The request this error belongs to, if any.
        request_id: Option<uuid::Uuid>,
        /// A human-readable message, safe to show in the overlay.
        message: String,
        /// Whether the worker can keep serving other requests.
        recoverable: bool,
    },
    /// Response to [`ShellToWorker::HealthCheck`].
    Health {
        /// Correlates with the health-check request.
        request_id: uuid::Uuid,
        /// Machine-readable health snapshot.
        report: HealthReport,
    },
}

/// The high-level state of a single request, mirrored in the overlay.
///
/// This is the typed state machine from `docs/PLAN.md` §3.3 point 4: it is
/// exhaustively matched wherever it is consumed, so the compiler rejects a
/// missing case instead of a state being silently unhandled at runtime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// Nothing in flight.
    Idle,
    /// Actively capturing audio.
    Listening,
    /// Transcribing and/or formatting, or waiting on an agent.
    Thinking,
    /// An OS action or agent task is running.
    Executing,
    /// The request finished. `true` = success, `false` = failure.
    Done(bool),
}

impl fmt::Display for WorkerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkerState::Idle => write!(f, "idle"),
            WorkerState::Listening => write!(f, "listening"),
            WorkerState::Thinking => write!(f, "thinking"),
            WorkerState::Executing => write!(f, "executing"),
            WorkerState::Done(true) => write!(f, "done_ok"),
            WorkerState::Done(false) => write!(f, "done_err"),
        }
    }
}

/// A snapshot of worker health, returned by [`ShellToWorker::HealthCheck`]
/// and printed by the `eva doctor` command described in `docs/PLAN.md` §3.4.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HealthReport {
    /// Whether the STT model is loaded and ready.
    pub stt_model_loaded: bool,
    /// The id of the loaded STT model, if any.
    pub stt_model_id: Option<String>,
    /// Whether the history/settings database opened and passed its integrity check.
    pub store_ok: bool,
    /// Detected agent CLIs and their status, e.g. `[("codex", "active"), ("claude", "not_installed")]`.
    pub agents: Vec<(String, String)>,
}

/// Encodes a message as a single line of JSON, terminated with `\n`, ready to
/// write to a pipe. Framing this way (rather than raw JSON without a
/// delimiter) is what lets the reader use a plain line-buffered read loop.
///
/// # Errors
/// Returns an error only if `msg` cannot be serialized, which does not happen
/// for the enums in this crate (they contain no non-serializable types) but
/// is still surfaced rather than unwrapped, per this workspace's error policy.
pub fn encode_line<T: Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    Ok(line)
}

/// Decodes a single line of JSON (with or without a trailing newline) back
/// into a message.
///
/// # Errors
/// Returns an error if `line` is not valid JSON or does not match `T`'s shape.
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim_end())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn shell_to_worker_round_trips_through_a_line() {
        let msg = ShellToWorker::StartRecording {
            request_id: uuid::Uuid::nil(),
        };
        let line = encode_line(&msg).expect("serializing a plain enum never fails");
        assert!(line.ends_with('\n'));
        let decoded: ShellToWorker = decode_line(&line).expect("valid line must decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn worker_to_shell_round_trips_for_every_variant() {
        let request_id = uuid::Uuid::nil();
        let messages = vec![
            WorkerToShell::Ready,
            WorkerToShell::StateChanged {
                state: WorkerState::Listening,
                request_id: Some(request_id),
            },
            WorkerToShell::Transcript {
                request_id,
                raw: "eh adan abre brave".into(),
                cleaned: "Adán, abre Brave.".into(),
            },
            WorkerToShell::IntentRecognized {
                request_id,
                intent_json: serde_json::json!({"kind": "open_app", "target": "Brave"}),
            },
            WorkerToShell::AgentEvent {
                request_id,
                event_json: serde_json::json!({"kind": "started"}),
            },
            WorkerToShell::Error {
                request_id: Some(request_id),
                message: "no se encontró el modelo".into(),
                recoverable: true,
            },
            WorkerToShell::Health {
                request_id,
                report: HealthReport {
                    stt_model_loaded: true,
                    stt_model_id: Some("canary-1b-flash".into()),
                    store_ok: true,
                    agents: vec![("codex".into(), "active".into())],
                },
            },
        ];

        for msg in messages {
            let line = encode_line(&msg).expect("serializing a plain enum never fails");
            let decoded: WorkerToShell = decode_line(&line).expect("valid line must decode");
            assert_eq!(decoded, msg, "round trip failed for {msg:?}");
        }
    }

    #[test]
    fn decode_line_rejects_garbage_instead_of_panicking() {
        let result: Result<ShellToWorker, _> = decode_line("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn worker_state_display_is_stable_for_logging_and_the_overlay() {
        assert_eq!(WorkerState::Idle.to_string(), "idle");
        assert_eq!(WorkerState::Done(true).to_string(), "done_ok");
        assert_eq!(WorkerState::Done(false).to_string(), "done_err");
    }
}
