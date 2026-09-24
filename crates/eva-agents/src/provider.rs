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
    /// EVA's own MCP server, injected for this one invocation — never
    /// written to the user's global CLI configuration (`docs/PLAN.md`
    /// fase 8: "reversible, aislado"). `None` runs the agent without it.
    pub mcp: Option<McpInjection>,
}

/// How to launch EVA's MCP server for one agent invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInjection {
    /// Absolute path of the `eva-mcp` binary.
    pub command: PathBuf,
    /// Arguments for it (the gateway socket to talk to).
    pub args: Vec<String>,
    /// Environment for it (the gateway's per-run token).
    pub env: Vec<(String, String)>,
}

impl McpInjection {
    /// The MCP server name agents see the tools under (`mcp__eva__open_url`).
    pub const SERVER_NAME: &'static str = "eva";
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

    /// The agent's process id — also the id of its process group, which the
    /// CLI leads (`stream::launch`). `None` once it has been reaped. What a
    /// worker records so that, if it dies, the next one can stop an agent
    /// that would otherwise keep running with nobody watching it.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Waits for the task to finish on its own and returns its outcome.
    pub async fn wait(self) -> AgentOutcome {
        outcome_of(self.output_task.await)
    }

    /// Waits for the task to finish, but cancels it (see [`Self::cancel`]) if
    /// `cancel` resolves first — how a background task stays stoppable from
    /// the tray while it runs for minutes.
    pub async fn wait_or_cancel(mut self, cancel: impl std::future::Future<Output = ()>) -> AgentOutcome {
        tokio::select! {
            joined = &mut self.output_task => outcome_of(joined),
            () = cancel => self.cancel().await,
        }
    }

    /// Cancels the running task: sends `SIGTERM`, waits briefly, and sends
    /// `SIGKILL` if the process has not exited — the "stop con
    /// SIGTERM→SIGKILL" from `docs/PLAN.md` fase 6.
    pub async fn cancel(mut self) -> AgentOutcome {
        if let Some(pid) = self.child.id() {
            send_signal(pid, Signal::Term);
        }

        // Deliberately not `.is_ok()` on the outer `Result` alone: that only
        // tells you the timeout didn't elapse, not that the process actually
        // exited — `Ok(Err(io_error))` (the wait call itself failing, fast,
        // for some other reason) would satisfy `.is_ok()` too and skip the
        // SIGKILL escalation for a process nobody confirmed is gone. Found
        // by the test below actually exercising this path, not by inspection.
        let wait_result = tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await;
        let exited_gracefully = matches!(wait_result, Ok(Ok(_)));

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

fn outcome_of(joined: Result<AgentOutcome, tokio::task::JoinError>) -> AgentOutcome {
    match joined {
        Ok(outcome) => outcome,
        Err(join_error) => {
            AgentOutcome::Failed { message: format!("la tarea que leía la salida del agente falló: {join_error}") }
        }
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
    //
    // The whole process group goes first (`-pid`): agent CLIs are launched
    // as their own group leader (`stream::launch`), and the node/shell
    // subprocesses they spawn would otherwise outlive a cancelled task. A
    // negative pid only names a group this child actually leads — a pid
    // cannot be reused while its group exists — so this can never signal
    // anything unrelated; for a child that is not a group leader it is a
    // harmless `ESRCH`, and the plain `pid` signal right after still lands.
    unsafe {
        libc::kill(-(pid as libc::pid_t), sig);
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
    async fn execute(&self, task: &AgentTask, events: UnboundedSender<AgentEvent>) -> Result<RunningAgent, AgentError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::time::Duration;

    /// A stand-in for the real stdout-reading task: it never resolves on
    /// its own, exactly like a real one blocked reading from a child that
    /// is still running — the only way it ends is `cancel()` aborting it.
    fn pending_output_task() -> tokio::task::JoinHandle<AgentOutcome> {
        tokio::spawn(async {
            std::future::pending::<()>().await;
            #[allow(clippy::panic)] // unreachable; see docs/ENGINEERING.md #2's inline-comment escape hatch
            {
                panic!("this task is never supposed to resolve on its own")
            }
        })
    }

    #[tokio::test]
    async fn cancel_stops_a_well_behaved_process_quickly_via_sigterm_alone() {
        let child = tokio::process::Command::new("sleep")
            .arg("100")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawning `sleep` must succeed on any Unix test runner");
        let running = RunningAgent::new(child, pending_output_task());

        let start = std::time::Instant::now();
        let outcome = running.cancel().await;
        let elapsed = start.elapsed();

        assert_eq!(outcome, AgentOutcome::Cancelled);
        assert!(
            elapsed < Duration::from_secs(1),
            "a process that honors SIGTERM must not pay the SIGKILL escalation's grace period; took {elapsed:?}"
        );
    }

    // Known, accepted trade-off found while writing this test: `sh -c
    // "trap '' TERM; sleep 100"` forks `sleep` as a real grandchild rather
    // than exec-replacing into it, so SIGKILLing the shell leaves `sleep`
    // orphaned to run out its own 100s lifetime. The `Stdio::null()` calls
    // below stop that orphan from holding open the file descriptors this
    // very test's own output is written through — without them, the
    // orphan silently kept the pipe to `cargo test`'s caller open for the
    // full 100s, making the test *look* hung long after it had actually
    // passed. Harmless once isolated like this (no shared file descriptors,
    // self-terminating), so left as-is rather than adding process-group
    // management just to avoid a background `sleep` nobody observes.
    #[tokio::test]
    async fn cancel_escalates_to_sigkill_when_the_process_ignores_sigterm() {
        // A shell that explicitly traps (ignores) SIGTERM — the real-world
        // case the escalation exists for: an agent CLI or a subprocess it
        // spawned that does not exit cleanly on the first signal.
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 100")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawning `sh` must succeed on any Unix test runner");

        // The shell needs a moment to actually execute `trap '' TERM` before
        // it can ignore anything — sending SIGTERM immediately after spawn
        // races the shell's own startup and can catch it before the trap is
        // installed, which made this test flaky against real timing rather
        // than against the behavior it means to verify.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let running = RunningAgent::new(child, pending_output_task());

        let start = std::time::Instant::now();
        let outcome = running.cancel().await;
        let elapsed = start.elapsed();

        assert_eq!(outcome, AgentOutcome::Cancelled);
        assert!(
            elapsed >= Duration::from_secs(3),
            "must actually wait out the full grace period before escalating, not skip straight to SIGKILL; took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn cancel_on_an_already_exited_process_does_not_hang_or_error() {
        let mut child = tokio::process::Command::new("true").spawn().expect("spawning `true` must succeed");
        // Let it exit on its own before cancel() ever touches it — cancelling
        // something already gone must be a graceful no-op, not a hang on a
        // signal to a pid that no longer exists.
        let _ = child.wait().await;
        let running = RunningAgent::new(child, pending_output_task());

        let outcome = running.cancel().await;
        assert_eq!(outcome, AgentOutcome::Cancelled);
    }

    #[tokio::test]
    async fn wait_or_cancel_returns_the_natural_outcome_when_the_task_finishes_first() {
        let child = tokio::process::Command::new("true").spawn().expect("spawning `true` must succeed");
        let output_task = tokio::spawn(async { AgentOutcome::Completed { summary: Some("listo".to_string()) } });
        let running = RunningAgent::new(child, output_task);

        let outcome = running.wait_or_cancel(std::future::pending()).await;
        assert_eq!(outcome, AgentOutcome::Completed { summary: Some("listo".to_string()) });
    }

    #[tokio::test]
    async fn wait_or_cancel_cancels_a_running_process_when_the_signal_fires() {
        let child = tokio::process::Command::new("sleep")
            .arg("100")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawning `sleep` must succeed");
        let running = RunningAgent::new(child, pending_output_task());

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let waiting = tokio::spawn(running.wait_or_cancel(async move {
            let _ = cancel_rx.await;
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_tx.send(()).expect("the waiter is still listening");

        let outcome = waiting.await.expect("the waiting task must not panic");
        assert_eq!(outcome, AgentOutcome::Cancelled);
    }

    #[tokio::test]
    async fn wait_returns_the_output_tasks_outcome_once_the_process_exits() {
        let child = tokio::process::Command::new("true").spawn().expect("spawning `true` must succeed");
        let output_task = tokio::spawn(async { AgentOutcome::Completed { summary: Some("listo".to_string()) } });
        let running = RunningAgent::new(child, output_task);

        let outcome = running.wait().await;
        assert_eq!(outcome, AgentOutcome::Completed { summary: Some("listo".to_string()) });
    }

    #[tokio::test]
    async fn wait_reports_a_failed_outcome_if_the_reader_task_itself_panics() {
        let child = tokio::process::Command::new("true").spawn().expect("spawning `true` must succeed");
        #[allow(clippy::panic)]
        // deliberately simulating a reader-task crash, to prove `wait()` degrades instead of propagating it
        let output_task = tokio::spawn(async { panic!("simulated reader-task crash") });
        let running = RunningAgent::new(child, output_task);

        let outcome = running.wait().await;
        assert!(
            matches!(outcome, AgentOutcome::Failed { .. }),
            "a panicked reader task must degrade to Failed, not propagate the panic"
        );
    }

    #[test]
    fn provider_status_is_active_is_true_only_for_active() {
        assert!(ProviderStatus::Active { version: "1.0".to_string() }.is_active());
        assert!(!ProviderStatus::NotInstalled.is_active());
        assert!(!ProviderStatus::InstalledNoSession { version: None }.is_active());
    }
}
