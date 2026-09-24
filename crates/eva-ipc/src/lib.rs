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

pub mod rpc;

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
    /// Say what a string would be understood as — dictation, or which
    /// command — without doing any of it. The safe cousin of
    /// [`ShellToWorker::RunIntentText`], and what `eva intent` sends by default.
    InterpretText {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
        /// The text to classify as if it had just been transcribed.
        text: String,
    },
    /// Ask the worker to report its health (models loaded, DB status, etc.).
    HealthCheck {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
    },
    /// Adds a word to the persisted personal dictionary
    /// (`docs/PLAN.md` fase 3) — closing the gap where `eva-store` could
    /// hold custom words but nothing in the protocol could ever add one.
    AddCustomWord {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
        /// The word to add, in its preferred display form (e.g. `"García"`).
        word: String,
    },
    /// Removes a word from the persisted personal dictionary.
    RemoveCustomWord {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
        /// The word to remove.
        word: String,
    },
    /// Lists every word currently in the persisted personal dictionary.
    ListCustomWords {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
    },
    /// Sets the wake word ("Adán" by default), persisted for future runs.
    SetWakeWord {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
        /// The new wake word.
        word: String,
    },
    /// Cancels every agent task currently running in the background
    /// (the tray's "Cancelar tareas" item).
    CancelAllTasks,
    /// Asks for the agent tasks currently running in the background.
    ListTasks {
        /// Correlates this request with its [`WorkerToShell::TaskList`] response.
        request_id: uuid::Uuid,
    },
    /// The user's answer to a [`WorkerToShell::ConfirmationRequested`] —
    /// given by clicking or pressing a hotkey, never by voice
    /// (`docs/PLAN.md` §6).
    ConfirmationResponse {
        /// The confirmation being answered.
        confirmation_id: uuid::Uuid,
        /// `true` to allow the action, `false` to refuse it.
        approved: bool,
    },
    /// The user says the last dictation came out wrong (the flag hotkey or
    /// the tray item). The worker keeps its audio and text as a case for the
    /// eval corpus and answers with [`WorkerToShell::DictationFlagged`].
    FlagLastDictation {
        /// Correlates this request with its response.
        request_id: uuid::Uuid,
    },
    /// "Are you still there?" — answered at once by [`WorkerToShell::Pong`]
    /// from the worker's command loop itself, so an answer proves the loop is
    /// not stuck, not merely that the process exists.
    Ping {
        /// Correlates with the answer.
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
    /// Response to [`ShellToWorker::ListCustomWords`], and also sent after
    /// [`ShellToWorker::AddCustomWord`]/[`ShellToWorker::RemoveCustomWord`]
    /// so the caller never has to issue a second request just to see the
    /// list reflect its own change.
    CustomWords {
        /// Correlates with the request that produced this list.
        request_id: uuid::Uuid,
        /// Every word currently in the persisted personal dictionary.
        words: Vec<String>,
    },
    /// A simple acknowledgement for commands with nothing more specific to
    /// report (e.g. [`ShellToWorker::SetWakeWord`]).
    Ack {
        /// Correlates with the request being acknowledged.
        request_id: uuid::Uuid,
    },
    /// An agent task was accepted and is now running in the background —
    /// the worker keeps serving other commands (dictation included) while
    /// it works (`docs/PLAN.md` fase 7).
    TaskStarted {
        /// The request that started the task; also its id everywhere else.
        request_id: uuid::Uuid,
        /// Which agent is running it (`"codex"`, `"claude_code"`).
        provider: String,
        /// What the agent was asked to do.
        prompt: String,
    },
    /// A background agent task ended, however it ended.
    TaskFinished {
        /// The task's id, as in [`WorkerToShell::TaskStarted`].
        request_id: uuid::Uuid,
        /// `true` only if the agent reported success.
        success: bool,
        /// A short, speakable summary (the agent's own, or the failure reason).
        summary: String,
    },
    /// Response to [`ShellToWorker::ListTasks`]: the tasks still running
    /// first, then the most recent finished ones.
    TaskList {
        /// Correlates with the request.
        request_id: uuid::Uuid,
        /// Running tasks, then recent history, newest first within each.
        tasks: Vec<TaskInfo>,
    },
    /// The gateway needs a human decision before it lets an action run
    /// (`docs/PLAN.md` fase 5). The shell must show it and answer with
    /// [`ShellToWorker::ConfirmationResponse`]; the worker treats silence
    /// past `timeout_secs` as a refusal.
    ConfirmationRequested {
        /// Identifies this question in the answer.
        confirmation_id: uuid::Uuid,
        /// A one-line description of the action, e.g. `Abrir file:///etc/hosts`.
        title: String,
        /// Why confirmation is needed, shown under the title.
        detail: String,
        /// How long the worker waits before assuming "no".
        timeout_secs: u64,
    },
    /// A [`WorkerToShell::ConfirmationRequested`] is no longer waiting for an
    /// answer — it timed out, or was answered elsewhere. The shell takes its
    /// prompt down.
    ConfirmationClosed {
        /// The confirmation that ended.
        confirmation_id: uuid::Uuid,
    },
    /// Response to [`ShellToWorker::Ping`].
    Pong {
        /// Correlates with the ping.
        request_id: uuid::Uuid,
    },
    /// Response to [`ShellToWorker::FlagLastDictation`] when it worked.
    DictationFlagged {
        /// Correlates with the request.
        request_id: uuid::Uuid,
        /// What was kept and where, for the user.
        message: String,
    },
}

/// Where an agent task stands.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Still working.
    Running,
    /// Finished and succeeded.
    Succeeded,
    /// Finished and failed, was cancelled, or was interrupted.
    Failed,
}

/// One agent task, as reported by [`WorkerToShell::TaskList`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskInfo {
    /// The task's id.
    pub request_id: uuid::Uuid,
    /// Which agent runs (or ran) it.
    pub provider: String,
    /// What it was asked to do.
    pub prompt: String,
    /// Where it stands.
    pub state: TaskState,
    /// The agent's summary or the failure reason, once finished.
    pub summary: Option<String>,
    /// Seconds since it started.
    pub age_secs: u64,
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
/// and printed by `eva doctor` (`docs/PLAN.md` §3.4).
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
    /// Which formatter is in use: `"apple_intelligence"`, `"reglas"`, or a
    /// composite naming both.
    pub formatter: String,
    /// Problems found in `config.toml`, in words fit to show.
    pub config_warnings: Vec<String>,
    /// The socket agents' MCP servers use to reach this worker, if it is up.
    pub gateway_socket: Option<String>,
    /// How many projects EVA knows about (`agents.project_roots`).
    pub project_count: usize,
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
    fn shell_to_worker_round_trips_for_every_variant() {
        let request_id = uuid::Uuid::nil();
        let messages = vec![
            ShellToWorker::StartRecording { request_id },
            ShellToWorker::StopRecording { request_id },
            ShellToWorker::Cancel { request_id },
            ShellToWorker::RunIntentText { request_id, text: "Adán, abre brave".into() },
            ShellToWorker::InterpretText { request_id, text: "Adán, abre brave".into() },
            ShellToWorker::HealthCheck { request_id },
            ShellToWorker::AddCustomWord { request_id, word: "García".into() },
            ShellToWorker::RemoveCustomWord { request_id, word: "García".into() },
            ShellToWorker::ListCustomWords { request_id },
            ShellToWorker::SetWakeWord { request_id, word: "Eva".into() },
            ShellToWorker::CancelAllTasks,
            ShellToWorker::ListTasks { request_id },
            ShellToWorker::ConfirmationResponse { confirmation_id: request_id, approved: true },
            ShellToWorker::FlagLastDictation { request_id },
            ShellToWorker::Ping { request_id },
            ShellToWorker::Shutdown,
        ];

        for msg in messages {
            let line = encode_line(&msg).expect("serializing a plain enum never fails");
            let decoded: ShellToWorker = decode_line(&line).expect("valid line must decode");
            assert_eq!(decoded, msg, "round trip failed for {msg:?}");
        }
    }

    #[test]
    fn shell_to_worker_round_trips_through_a_line() {
        let msg = ShellToWorker::StartRecording { request_id: uuid::Uuid::nil() };
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
            WorkerToShell::StateChanged { state: WorkerState::Listening, request_id: Some(request_id) },
            WorkerToShell::Transcript {
                request_id,
                raw: "eh adan abre brave".into(),
                cleaned: "Adán, abre Brave.".into(),
            },
            WorkerToShell::IntentRecognized {
                request_id,
                intent_json: serde_json::json!({"kind": "open_app", "target": "Brave"}),
            },
            WorkerToShell::AgentEvent { request_id, event_json: serde_json::json!({"kind": "started"}) },
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
                    formatter: "apple_intelligence".into(),
                    config_warnings: vec!["agents.priority: no conozco el agente \"x\"".into()],
                    gateway_socket: Some("/tmp/gateway.sock".into()),
                    project_count: 3,
                },
            },
            WorkerToShell::CustomWords { request_id, words: vec!["García".into(), "Núñez".into()] },
            WorkerToShell::Ack { request_id },
            WorkerToShell::TaskStarted { request_id, provider: "codex".into(), prompt: "agrega tests al login".into() },
            WorkerToShell::TaskFinished { request_id, success: true, summary: "3 archivos".into() },
            WorkerToShell::TaskList {
                request_id,
                tasks: vec![TaskInfo {
                    request_id,
                    provider: "claude_code".into(),
                    prompt: "arregla el build".into(),
                    state: TaskState::Succeeded,
                    summary: Some("3 archivos".into()),
                    age_secs: 42,
                }],
            },
            WorkerToShell::ConfirmationRequested {
                confirmation_id: request_id,
                title: "Abrir file:///etc/hosts".into(),
                detail: "un esquema que no es web".into(),
                timeout_secs: 30,
            },
            WorkerToShell::ConfirmationClosed { confirmation_id: request_id },
            WorkerToShell::DictationFlagged { request_id, message: "guardado".into() },
            WorkerToShell::Pong { request_id },
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
