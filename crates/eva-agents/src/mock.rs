//! A test double for [`AgentProvider`], per `docs/ENGINEERING.md` #5: this is
//! how dispatch logic gets tested without spawning a real CLI or spending
//! real API quota. Exposed as a normal (non-`#[cfg(test)]`) module because
//! `eva-worker`'s own test suite will need it too, not just this crate's.

use crate::event::AgentEvent;
use crate::provider::{AgentError, AgentOutcome, AgentProvider, AgentTask, ProviderStatus, RunningAgent};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// A scripted, in-memory [`AgentProvider`] for tests.
pub struct MockProvider {
    id: &'static str,
    status: ProviderStatus,
    /// The events (and final outcome) to emit on the *next* call to
    /// [`AgentProvider::execute`]. Consumed in order across calls, so a test
    /// can script a sequence of different runs from the same mock.
    scripts: Mutex<Vec<(Vec<AgentEvent>, AgentOutcome)>>,
    executions: AtomicUsize,
    /// Every [`AgentTask`] this mock has actually received, in order — lets
    /// a test assert on what was asked for (a resumed session id, the right
    /// project directory, …), not just that *something* was called.
    received_tasks: Mutex<Vec<AgentTask>>,
    /// When set, `execute` starts a run that never finishes on its own — the
    /// shape of a real agent mid-task, for tests of cancellation and of
    /// things that must keep working while an agent runs.
    hangs: bool,
}

impl MockProvider {
    /// Builds a mock that reports `status` and, on each successive
    /// `execute` call, emits the next `(events, outcome)` pair from
    /// `scripts` in order.
    pub fn new(id: &'static str, status: ProviderStatus, scripts: Vec<(Vec<AgentEvent>, AgentOutcome)>) -> Self {
        MockProvider {
            id,
            status,
            scripts: Mutex::new(scripts),
            executions: AtomicUsize::new(0),
            received_tasks: Mutex::new(Vec::new()),
            hangs: false,
        }
    }

    /// A provider whose runs never finish until cancelled: after emitting
    /// [`AgentEvent::Started`] they sit there, like a real agent working.
    pub fn never_finishes(id: &'static str) -> Self {
        MockProvider {
            hangs: true,
            ..MockProvider::new(id, ProviderStatus::Active { version: "mock".to_string() }, Vec::new())
        }
    }

    /// Every task received so far, in order.
    pub fn received_tasks(&self) -> Vec<AgentTask> {
        #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
        self.received_tasks.lock().unwrap().clone()
    }

    /// A convenience constructor for the common case: always `Active`, and
    /// every call to `execute` emits the same single completion.
    pub fn always_completes(id: &'static str, summary: &str) -> Self {
        MockProvider::new(
            id,
            ProviderStatus::Active { version: "mock".to_string() },
            vec![(Vec::new(), AgentOutcome::Completed { summary: Some(summary.to_string()) })],
        )
    }

    /// How many times [`AgentProvider::execute`] has been called on this mock.
    pub fn execution_count(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AgentProvider for MockProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    async fn detect(&self) -> ProviderStatus {
        self.status.clone()
    }

    async fn execute(&self, task: &AgentTask, events: UnboundedSender<AgentEvent>) -> Result<RunningAgent, AgentError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
        self.received_tasks.lock().unwrap().push(task.clone());

        if self.hangs {
            let _ = events.send(AgentEvent::Started);
            let child = tokio::process::Command::new("sleep")
                .arg("300")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|source| AgentError::Spawn { provider: self.id, source })?;
            let output_task = tokio::spawn(async move {
                std::future::pending::<()>().await;
                AgentOutcome::Cancelled
            });
            return Ok(RunningAgent::new(child, output_task));
        }

        #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
        let mut scripts = self.scripts.lock().unwrap();
        let (script_events, outcome) = if scripts.is_empty() {
            (Vec::new(), AgentOutcome::Completed { summary: None })
        } else {
            scripts.remove(0)
        };
        drop(scripts);

        for event in script_events {
            let _ = events.send(event);
        }

        // A mock has no real child process to hand back, so it spawns a
        // trivial one (`true`, universally available on Unix and present on
        // macOS/Linux CI images) purely so `RunningAgent` has something to
        // hold — its exit code is never inspected, only the scripted
        // `outcome` above is what `RunningAgent::wait` returns.
        let child = tokio::process::Command::new("true")
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(|source| AgentError::Spawn { provider: self.id, source })?;

        let output_task = tokio::spawn(async move { outcome });
        Ok(RunningAgent::new(child, output_task))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn dummy_task() -> AgentTask {
        AgentTask {
            prompt: "test".to_string(),
            project_dir: std::env::temp_dir(),
            session_id: uuid::Uuid::new_v4(),
            resume_session_id: None,
            mcp: None,
        }
    }

    #[tokio::test]
    async fn always_completes_reports_active_and_completes() {
        let mock = MockProvider::always_completes("mock", "listo");
        assert_eq!(mock.detect().await, ProviderStatus::Active { version: "mock".to_string() });

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let running = mock.execute(&dummy_task(), tx).await.expect("mock execute must succeed");
        let outcome = running.wait().await;
        assert_eq!(outcome, AgentOutcome::Completed { summary: Some("listo".to_string()) });
        assert_eq!(mock.execution_count(), 1);
    }

    #[tokio::test]
    async fn scripts_are_consumed_in_order_across_calls() {
        let mock = MockProvider::new(
            "mock",
            ProviderStatus::Active { version: "mock".to_string() },
            vec![
                (Vec::new(), AgentOutcome::Completed { summary: Some("primero".to_string()) }),
                (Vec::new(), AgentOutcome::Failed { message: "segundo".to_string() }),
            ],
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let first = mock.execute(&dummy_task(), tx.clone()).await.expect("must succeed").wait().await;
        assert_eq!(first, AgentOutcome::Completed { summary: Some("primero".to_string()) });

        let second = mock.execute(&dummy_task(), tx).await.expect("must succeed").wait().await;
        assert_eq!(second, AgentOutcome::Failed { message: "segundo".to_string() });
    }

    #[tokio::test]
    async fn emitted_events_are_received_before_the_outcome() {
        let mock = MockProvider::new(
            "mock",
            ProviderStatus::Active { version: "mock".to_string() },
            vec![(
                vec![AgentEvent::Started, AgentEvent::Message { text: "hola".into() }],
                AgentOutcome::Completed { summary: None },
            )],
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let running = mock.execute(&dummy_task(), tx).await.expect("must succeed");

        assert_eq!(rx.recv().await, Some(AgentEvent::Started));
        assert_eq!(rx.recv().await, Some(AgentEvent::Message { text: "hola".into() }));

        running.wait().await;
    }
}
