//! The `AgentProvider` boundary from `docs/PLAN.md` §3: this is the one
//! trait every CLI-backed agent implements, so `eva-worker` never branches on
//! "if codex … if claude". See `docs/ENGINEERING.md` #5 — this is also the
//! seam a mock implementation sits behind for testing dispatch logic without
//! spawning real processes or spending real API quota.

use crate::event::AgentEvent;
use async_trait::async_trait;
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

/// A task to dispatch to an agent.
#[derive(Debug, Clone)]
pub struct AgentTask {
    /// The prompt — usually an `Intent::AgentTask::prompt` from `eva-intent`.
    pub prompt: String,
    /// The project directory the agent should treat as its working root.
    pub project_dir: PathBuf,
    /// The session id EVA assigns, so "Adán, continúa" can resume the right
    /// run without depending on the CLI's own session bookkeeping to be
    /// discoverable (`docs/PLAN.md` fase 7).
    pub session_id: Uuid,
    /// If set, resume this earlier session instead of starting a new one.
    pub resume_session_id: Option<Uuid>,
}

/// Why an agent run could not be started or did not finish cleanly.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The CLI binary could not be found or started.
    #[error("no se pudo iniciar {provider}: {source}")]
    Spawn {
        /// The provider id ("codex", "claude_code") that failed to spawn.
        provider: &'static str,
        /// The underlying OS error.
        source: std::io::Error,
    },
    /// The CLI is installed but not usable right now (e.g. not logged in).
    #[error("{provider} no está listo: {reason}")]
    NotReady {
        /// The provider id.
        provider: &'static str,
        /// A human-readable reason, suitable for the overlay.
        reason: String,
    },
    /// The child process's stdout could not be read.
    #[error("no se pudo leer la salida de {provider}: {source}")]
    Io {
        /// The provider id.
        provider: &'static str,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// Whether a provider's CLI is usable right now, per `docs/PLAN.md` fase 6's
/// detection design (mirrors `codex doctor` / `claude doctor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStatus {
    /// The CLI binary was not found on this system.
    NotInstalled,
    /// The CLI is installed but has no active session (not logged in).
    InstalledNoSession {
        /// The installed version string, if it could be read.
        version: Option<String>,
    },
    /// The CLI is installed, logged in, and ready to run tasks.
    Active {
        /// The installed version string.
        version: String,
    },
}

impl ProviderStatus {
    /// `true` only for [`ProviderStatus::Active`] — the only state a task
    /// should actually be dispatched in.
    pub fn is_active(&self) -> bool {
        matches!(self, ProviderStatus::Active { .. })
    }
}

/// A handle to a running agent task: the caller keeps this to cancel the
/// task, and awaits it to get the final [`AgentOutcome`].
pub struct RunningAgent {
    child: tokio::process::Child,
    output_task: tokio::task::JoinHandle<AgentOutcome>,
}

impl RunningAgent {
    /// Wraps an already-spawned child and the task reading its output.
    /// Providers use this to build the handle they return from
    /// [`AgentProvider::execute`]; it is not meant to be constructed
    /// directly from outside this crate's provider implementations.
    pub(crate) fn new(child: tokio::process::Child, output_task: tokio::task::JoinHandle<AgentOutcome>) -> Self {
        RunningAgent { child, output_task }
    }

    /// Waits for the task to finish on its own and returns its outcome.
    pub async fn wait(self) -> AgentOutcome {
        match self.output_task.await {
            Ok(outcome) => outcome,
            Err(join_error) => AgentOutcome::Failed {
                message: format!("la tarea que leía la salida del agente falló: {join_error}"),
            },
        }
    }

    /// Cancels the running task: sends `SIGTERM`, waits briefly, and sends
    /// `SIGKILL` if the process has not exited — the "stop con
    /// SIGTERM→SIGKILL" from `docs/PLAN.md` fase 6.
    pub async fn cancel(mut self) -> AgentOutcome {
        if let Some(pid) = self.child.id() {
            send_signal(pid, Signal::Term);
        }

        let exited_gracefully = tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait())
            .await
            .is_ok();

        if !exited_gracefully {
            if let Some(pid) = self.child.id() {
                send_signal(pid, Signal::Kill);
            }
            let _ = self.child.wait().await;
        }

        self.output_task.abort();
        AgentOutcome::Cancelled
    }
}

/// The two signals [`RunningAgent::cancel`] needs. Kept as an enum instead of
/// raw `libc` constants so the call sites read as intent, not magic numbers.
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: Signal) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` is called with a pid this process itself spawned (it
    // came from `tokio::process::Child::id`) and a fixed, valid signal
    // number; there is no memory unsafety here, only the possibility that
    // the process has already exited, which `kill` reports as `ESRCH` and
    // which this function intentionally ignores — cancelling an
    // already-dead process is a no-op, not an error worth surfacing.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: Signal) {
    // Windows process termination is a `docs/PLAN.md` "later" item (fase 10
    // table: Windows support is deferred until needed). Nothing to do here
    // yet; `RunningAgent::cancel` still aborts the reader task and returns
    // `Cancelled` even without an OS-level kill.
}

/// How an agent run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentOutcome {
    /// Finished successfully.
    Completed {
        /// The agent's final summary, if any.
        summary: Option<String>,
    },
    /// Finished with an error.
    Failed {
        /// A human-readable description of the failure.
        message: String,
    },
    /// Cancelled by [`RunningAgent::cancel`] before it finished on its own.
    Cancelled,
}

/// Something that can run agent tasks: Codex, Claude Code, a mock for tests,
/// or whatever CLI shows up next — `eva-worker` only ever talks to this trait.
#[async_trait]
pub trait AgentProvider: Send + Sync {
    /// A short, stable id ("codex", "claude_code"), used in logs, settings,
    /// and the "usa Claude y…" voice override.
    fn id(&self) -> &'static str;

    /// Checks whether this provider's CLI is installed and has an active
    /// session, mirroring `codex doctor` / `claude doctor`.
    async fn detect(&self) -> ProviderStatus;

    /// Starts `task` running. Every [`AgentEvent`] the run produces is sent
    /// on `events` as it happens; the returned [`RunningAgent`] is used to
    /// wait for or cancel the run.
    ///
    /// # Errors
    /// Returns [`AgentError`] if the process could not even be started —
    /// once it starts, failures during the run are reported as an
    /// [`AgentEvent::Failed`] on `events` and an [`AgentOutcome::Failed`]
    /// from [`RunningAgent::wait`], not as an `Err` here.
    async fn execute(
        &self,
        task: &AgentTask,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<RunningAgent, AgentError>;
}
