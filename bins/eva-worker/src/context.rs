//! Everything a command needs to be handled, built once at startup and
//! shared by reference: the store, the config, the gateway and the two
//! services acting through it, the agent registry, the STT model, and the
//! outgoing event channel. `main.rs` builds one; tests build many through
//! the test kit.

use crate::confirm::ConfirmationBroker;
use crate::harvest::Harvest;
use crate::orphans::AgentLedger;
use crate::tasks::TaskRegistry;
use eva_agents::{AgentRegistry, McpInjection};
use eva_audio::{AudioSource, CaptureHandle, SpeechToText};
use eva_config::{Config, ProjectIndex, Resolution};
use eva_gateway::Gateway;
use eva_intent::AppIndex;
use eva_ipc::{WorkerState, WorkerToShell};
use eva_mcp::{ConfiguredProjects, Desktop, LocalService, ProjectSource};
use eva_store::Store;
use eva_text::Formatter;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use uuid::Uuid;

/// The channel every part of the worker reports through. Cloning is cheap;
/// events reach `eva-shell` (or the CLI) the moment they are sent, from any
/// task — which is what lets an agent stream its progress for minutes while
/// the worker keeps taking commands.
#[derive(Clone)]
pub struct Events(UnboundedSender<WorkerToShell>);

impl Events {
    /// A new channel: the sender side to hand around, the receiver to drain.
    pub fn channel() -> (Events, UnboundedReceiver<WorkerToShell>) {
        let (tx, rx) = unbounded_channel();
        (Events(tx), rx)
    }

    /// Sends `event`. A closed channel means the process is shutting down
    /// and nobody is listening, which is not an error worth surfacing.
    pub fn emit(&self, event: WorkerToShell) {
        let _ = self.0.send(event);
    }

    /// Reports the state of `request_id`.
    pub fn state(&self, request_id: Uuid, state: WorkerState) {
        self.emit(WorkerToShell::StateChanged { state, request_id: Some(request_id) });
    }

    /// Reports a recoverable problem with `request_id`.
    pub fn error(&self, request_id: Uuid, message: impl Into<String>) {
        self.emit(WorkerToShell::Error { request_id: Some(request_id), message: message.into(), recoverable: true });
    }

    /// Reports a problem and ends the request as failed.
    pub fn fail(&self, request_id: Uuid, message: impl Into<String>) {
        self.error(request_id, message);
        self.state(request_id, WorkerState::Done(false));
    }
}

/// The audio capture and speech-to-text pieces, present only when a model
/// was successfully configured and loaded at startup (`docs/PLAN.md` §3.3
/// point 5: "el modelo STT no carga → overlay en rojo + notificación clara
/// al primer intento de grabar", never a silent no-op).
pub struct AudioContext {
    /// Captures the microphone, resampled to 16 kHz mono.
    pub source: Arc<dyn AudioSource>,
    /// Transcribes a finished recording.
    pub stt: Arc<dyn SpeechToText>,
    /// A human-readable identifier for the loaded model, for `eva doctor`.
    pub model_id: String,
}

/// The buffer and capture handle for a recording in progress, between a
/// `StartRecording` and its matching `StopRecording`.
pub(crate) struct RecordingSession {
    pub(crate) request_id: Uuid,
    pub(crate) handle: Box<dyn CaptureHandle>,
    pub(crate) buffer: Arc<Mutex<Vec<f32>>>,
}

/// What [`WorkerContext::new`] is built from.
pub struct WorkerDeps {
    /// The history/dictionary/settings/audit database.
    pub store: Store,
    /// `config.toml`, already loaded.
    pub config: Config,
    /// Resolves spoken app names to canonical ones.
    pub app_index: AppIndex,
    /// The configured wake word ("Adán" by default).
    pub wake_word: String,
    /// The working directory: where a task runs when nothing says otherwise.
    pub base_dir: PathBuf,
    /// Opens/closes apps, opens URLs, pastes text, reads the active window.
    pub desktop: Arc<dyn Desktop>,
    /// Codex/Claude Code, in priority order.
    pub agents: AgentRegistry,
    /// `None` until a real STT model is configured and loaded.
    pub audio: Option<AudioContext>,
    /// The context-aware formatting pass.
    pub formatter: Arc<dyn Formatter>,
    /// Where events go.
    pub events: Events,
    /// How agents reach this worker's gateway, or `None` if the socket is
    /// not up (agents then run without EVA's tools).
    pub mcp: Option<McpInjection>,
    /// Where each task's git worktree is created.
    pub worktrees_dir: PathBuf,
    /// A short name for the formatter in use, for the health report.
    pub formatter_name: String,
    /// Problems found loading `config.toml`, for the health report.
    pub config_warnings: Vec<String>,
    /// Where dictations the user flags as wrong are kept.
    pub harvest_dir: PathBuf,
    /// Where the running agents' processes are written down.
    pub agent_ledger_dir: PathBuf,
}

/// The worker's shared state.
pub struct WorkerContext {
    /// The history/dictionary/settings/audit database.
    pub store: Store,
    /// `config.toml`.
    pub config: Config,
    /// Resolves spoken app names to canonical ones.
    pub app_index: AppIndex,
    /// The configured wake word.
    pub wake_word: String,
    /// The working directory.
    pub base_dir: PathBuf,
    /// Opens/closes apps, opens URLs, pastes text, reads the active window.
    pub desktop: Arc<dyn Desktop>,
    /// Codex/Claude Code, in priority order.
    pub agents: AgentRegistry,
    /// `None` until a real STT model is configured and loaded — see
    /// [`AudioContext`]'s doc for why `StartRecording` reports a real error
    /// rather than pretending to record when this is `None`.
    pub audio: Option<AudioContext>,
    /// The context-aware formatting pass. Every call still goes through
    /// `eva_text::clean_styled`'s own fallback, so even a formatter that
    /// starts failing mid-session degrades instead of losing the transcript.
    pub formatter: Arc<dyn Formatter>,
    /// Where events go.
    pub events: Events,
    /// Asks the user, through the shell, and waits for a click or hotkey.
    pub broker: Arc<ConfirmationBroker>,
    /// Rules on every action.
    pub gateway: Arc<Gateway>,
    /// The user's voice, acting through the gateway.
    pub voice: LocalService,
    /// An agent, acting through the gateway — what the gateway socket serves.
    pub agent_service: Arc<LocalService>,
    /// The projects EVA knows about.
    pub projects: Arc<ConfiguredProjects>,
    /// How agents reach this worker's gateway.
    pub mcp: Option<McpInjection>,
    /// Where each task's git worktree is created.
    pub worktrees_dir: PathBuf,
    /// A short name for the formatter in use, for the health report.
    pub formatter_name: String,
    /// Problems found loading `config.toml`, for the health report.
    pub config_warnings: Vec<String>,
    /// The agent tasks running in the background.
    pub tasks: TaskRegistry,
    /// The last dictation, held so the user can flag it as wrong.
    pub harvest: Harvest,
    /// The running agents' processes, for the next worker if this one dies.
    pub agent_ledger: AgentLedger,
    recording: Mutex<Option<RecordingSession>>,
    jobs: JobTracker,
}

impl WorkerContext {
    /// Assembles the context, wiring the gateway to the confirmation broker
    /// and both services to the gateway.
    pub fn new(deps: WorkerDeps) -> WorkerContext {
        let broker = Arc::new(ConfirmationBroker::new(deps.events.clone()));
        let gateway = Arc::new(Gateway::new(deps.config.gateway.clone(), deps.store.clone(), broker.clone()));
        let projects = Arc::new(ConfiguredProjects::from_config(&deps.config, deps.base_dir.clone()));
        let confirm_timeout = Duration::from_secs(deps.config.gateway.confirm_timeout_secs());

        let service = |origin| {
            LocalService::new(
                origin,
                gateway.clone(),
                deps.desktop.clone(),
                broker.clone(),
                projects.clone() as Arc<dyn ProjectSource>,
                confirm_timeout,
            )
        };
        let voice = service(eva_config::Origin::Voice);
        let agent_service = Arc::new(service(eva_config::Origin::Agent));

        WorkerContext {
            store: deps.store,
            config: deps.config,
            app_index: deps.app_index,
            wake_word: deps.wake_word,
            base_dir: deps.base_dir,
            desktop: deps.desktop,
            agents: deps.agents,
            audio: deps.audio,
            formatter: deps.formatter,
            events: deps.events,
            broker,
            gateway,
            voice,
            agent_service,
            projects,
            mcp: deps.mcp,
            worktrees_dir: deps.worktrees_dir,
            formatter_name: deps.formatter_name,
            config_warnings: deps.config_warnings,
            tasks: TaskRegistry::default(),
            harvest: Harvest::new(deps.harvest_dir),
            agent_ledger: AgentLedger::new(deps.agent_ledger_dir),
            recording: Mutex::new(None),
            jobs: JobTracker::new(),
        }
    }

    /// The index of known projects.
    pub fn project_index(&self) -> &ProjectIndex {
        self.projects.index()
    }

    /// Runs `job` in the background, tracked so [`WorkerContext::wait_idle`]
    /// (graceful shutdown, tests) can wait for it. Everything that takes
    /// real time — transcription, a dictation's formatting, an agent task —
    /// runs like this, which is what keeps the command loop answering
    /// `Cancel`, `ConfirmationResponse` and a new `StartRecording` meanwhile.
    pub fn spawn_job(&self, job: impl Future<Output = ()> + Send + 'static) {
        let guard = self.jobs.start();
        tokio::spawn(async move {
            job.await;
            drop(guard);
        });
    }

    /// Resolves once no background job is running.
    pub async fn wait_idle(&self) {
        self.jobs.wait_idle().await;
    }

    /// The title of the focused window, read off the main path's thread.
    pub async fn active_window_title(&self) -> Option<String> {
        self.active_window().await.and_then(|w| w.window_title)
    }

    /// The frontmost app, read on a blocking thread (the Accessibility
    /// queries behind it are synchronous and bounded by a timeout).
    pub async fn active_window(&self) -> Option<eva_macos::RunningAppInfo> {
        let desktop = Arc::clone(&self.desktop);
        tokio::task::spawn_blocking(move || desktop.active_window()).await.unwrap_or(None)
    }

    /// Where a voice task should run, and why: the project the focused
    /// window names, else `agents.default_project`, else the project the
    /// last task ran in, else the working directory.
    pub async fn resolve_project(&self) -> (PathBuf, Resolution) {
        let title = self.active_window_title().await;
        let most_recent = self
            .store
            .recent_tasks(1)
            .ok()
            .and_then(|tasks| tasks.into_iter().next())
            .map(|t| PathBuf::from(t.project_dir));
        self.projects.index().resolve_active(
            title.as_deref(),
            self.projects.default_project(),
            most_recent.as_deref(),
            &self.base_dir,
        )
    }

    pub(crate) fn recording(&self) -> std::sync::MutexGuard<'_, Option<RecordingSession>> {
        #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
        self.recording.lock().unwrap()
    }
}

/// Counts background jobs and lets a caller wait for zero.
struct JobTracker {
    running: watch::Sender<usize>,
}

/// Held by a running job; dropping it (on completion or panic) counts the
/// job as finished.
struct JobGuard(watch::Sender<usize>);

impl JobTracker {
    fn new() -> JobTracker {
        JobTracker { running: watch::channel(0).0 }
    }

    fn start(&self) -> JobGuard {
        self.running.send_modify(|n| *n += 1);
        JobGuard(self.running.clone())
    }

    async fn wait_idle(&self) {
        let mut rx = self.running.subscribe();
        let _ = rx.wait_for(|n| *n == 0).await;
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_idle_returns_immediately_with_no_jobs() {
        let tracker = JobTracker::new();
        tokio::time::timeout(Duration::from_millis(200), tracker.wait_idle()).await.expect("must not wait");
    }

    #[tokio::test]
    async fn wait_idle_waits_for_a_running_job_and_only_that_long() {
        let tracker = Arc::new(JobTracker::new());
        let guard = tracker.start();
        let waiter = {
            let tracker = Arc::clone(&tracker);
            tokio::spawn(async move { tracker.wait_idle().await })
        };

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "a job is still running");

        drop(guard);
        tokio::time::timeout(Duration::from_millis(500), waiter).await.expect("must finish").expect("no panic");
    }

    #[tokio::test]
    async fn a_job_that_panics_still_counts_as_finished() {
        let tracker = Arc::new(JobTracker::new());
        let guard = tracker.start();
        let handle = tokio::spawn(async move {
            let _guard = guard;
            #[allow(clippy::panic)]
            {
                panic!("simulated job crash")
            }
        });
        let _ = handle.await;
        tokio::time::timeout(Duration::from_millis(500), tracker.wait_idle())
            .await
            .expect("panic must release the guard");
    }

    #[test]
    fn events_state_and_fail_emit_the_expected_shapes() {
        let (events, mut rx) = Events::channel();
        let id = Uuid::new_v4();
        events.fail(id, "roto");
        assert!(
            matches!(rx.try_recv(), Ok(WorkerToShell::Error { message, recoverable: true, .. }) if message == "roto")
        );
        assert!(matches!(rx.try_recv(), Ok(WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[test]
    fn emitting_on_a_closed_channel_is_silent() {
        let (events, rx) = Events::channel();
        drop(rx);
        events.emit(WorkerToShell::Ready); // must not panic
    }
}
