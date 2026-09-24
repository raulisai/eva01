//! A worker wired to test doubles: a mock desktop, an in-memory store, a
//! deterministic rules-only formatter (the real Apple Intelligence bridge
//! depends on this machine's own state — see `eva-text`'s own real-model
//! tests for that coverage), and a channel to read every event from.

use crate::context::{AudioContext, Events, WorkerContext, WorkerDeps};
use crate::handler::handle;
use async_trait::async_trait;
use eva_agents::{AgentEvent, AgentProvider, AgentRegistry, AgentTask, McpInjection, ProviderStatus, RunningAgent};
use eva_config::Config;
use eva_intent::{AppEntry, AppIndex};
use eva_ipc::{ShellToWorker, WorkerToShell};
use eva_mcp::desktop::mock::MockDesktop;
use eva_store::Store;
use eva_text::{FormatError, Formatter, RuleOnlyFormatter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;

/// A worker under test.
pub struct Rig {
    /// The worker's shared state.
    pub ctx: Arc<WorkerContext>,
    /// The mock desktop the worker acts through.
    pub desktop: Arc<MockDesktop>,
    rx: UnboundedReceiver<WorkerToShell>,
    _dir: tempfile::TempDir,
}

/// Configures a [`Rig`].
pub struct RigBuilder {
    desktop: MockDesktop,
    agents: AgentRegistry,
    audio: Option<AudioContext>,
    formatter: Arc<dyn Formatter>,
    config: Config,
    config_warnings: Vec<String>,
    base_dir: Option<PathBuf>,
    mcp: Option<McpInjection>,
    apps: Option<crate::apps::AppCatalog>,
}

impl Rig {
    /// A rig with every default: rules-only formatting, no agents, no STT.
    pub fn new() -> Rig {
        Rig::builder().build()
    }

    /// A rig to configure.
    pub fn builder() -> RigBuilder {
        let mut config = Config::default();
        // Quiet by default so `desktop.calls()` shows only what a test is
        // about; tests of the announcements turn these back on. No worktree
        // either: tests of worktrees make a real repository for it.
        config.feedback.speak_task_results = false;
        config.feedback.notify_task_results = false;
        config.agents.worktree = false;
        config.agents.project_roots = Vec::new();
        RigBuilder {
            desktop: MockDesktop::new(),
            agents: AgentRegistry::new(Vec::new()),
            audio: None,
            formatter: Arc::new(RuleOnlyFormatter),
            config,
            config_warnings: Vec::new(),
            base_dir: None,
            mcp: None,
            apps: None,
        }
    }

    /// Sends `command` and returns every event produced by the time all the
    /// background work it started has finished.
    pub async fn run(&mut self, command: ShellToWorker) -> Vec<WorkerToShell> {
        handle(&self.ctx, command, None);
        self.ctx.wait_idle().await;
        self.drain()
    }

    /// Like [`Rig::run`], answering every confirmation the way the shell's
    /// overlay would: through the real `ConfirmationResponse` command.
    pub async fn run_answering(&mut self, command: ShellToWorker, approve: bool) -> Vec<WorkerToShell> {
        handle(&self.ctx, command, None);
        let mut events = Vec::new();
        let ctx = Arc::clone(&self.ctx);
        let idle = ctx.wait_idle();
        tokio::pin!(idle);
        loop {
            tokio::select! {
                Some(event) = self.rx.recv() => {
                    if let WorkerToShell::ConfirmationRequested { confirmation_id, .. } = &event {
                        handle(&ctx, ShellToWorker::ConfirmationResponse { confirmation_id: *confirmation_id, approved: approve }, None);
                    }
                    events.push(event);
                }
                () = &mut idle => break,
            }
        }
        events.extend(self.drain());
        events
    }

    /// Every event already waiting.
    pub fn drain(&mut self) -> Vec<WorkerToShell> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// The next event, waiting up to two seconds for it.
    pub async fn next_event(&mut self) -> Option<WorkerToShell> {
        tokio::time::timeout(std::time::Duration::from_secs(2), self.rx.recv()).await.ok().flatten()
    }

    /// Waits until an event matching `wanted` arrives (or two seconds pass),
    /// returning everything seen up to and including it.
    pub async fn until(&mut self, wanted: impl Fn(&WorkerToShell) -> bool) -> Vec<WorkerToShell> {
        let mut seen = Vec::new();
        while let Some(event) = self.next_event().await {
            let done = wanted(&event);
            seen.push(event);
            if done {
                break;
            }
        }
        seen
    }
}

impl RigBuilder {
    /// The mock desktop to act through.
    #[must_use]
    pub fn desktop(mut self, desktop: MockDesktop) -> Self {
        self.desktop = desktop;
        self
    }

    /// The agents available.
    #[must_use]
    pub fn agents(mut self, agents: AgentRegistry) -> Self {
        self.agents = agents;
        self
    }

    /// A loaded STT model and microphone.
    #[must_use]
    pub fn audio(mut self, audio: AudioContext) -> Self {
        self.audio = Some(audio);
        self
    }

    /// The formatter.
    #[must_use]
    pub fn formatter(mut self, formatter: impl Formatter + 'static) -> Self {
        self.formatter = Arc::new(formatter);
        self
    }

    /// Adjusts the config.
    #[must_use]
    pub fn configure(mut self, change: impl FnOnce(&mut Config)) -> Self {
        change(&mut self.config);
        self
    }

    /// Config warnings to report in the health check.
    #[must_use]
    pub fn config_warnings(mut self, warnings: Vec<String>) -> Self {
        self.config_warnings = warnings;
        self
    }

    /// The directory tasks run in when nothing says otherwise.
    #[must_use]
    pub fn base_dir(mut self, dir: PathBuf) -> Self {
        self.base_dir = Some(dir);
        self
    }

    /// The installed apps, instead of the default (only Brave).
    #[must_use]
    pub fn apps(mut self, apps: crate::apps::AppCatalog) -> Self {
        self.apps = Some(apps);
        self
    }

    /// How agents would reach the gateway.
    #[must_use]
    pub fn mcp(mut self, mcp: McpInjection) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Builds the worker.
    pub fn build(self) -> Rig {
        let dir = tempfile::tempdir().expect("a temp dir");
        let desktop = Arc::new(self.desktop);
        let (events, rx) = Events::channel();
        let ctx = WorkerContext::new(WorkerDeps {
            store: Store::open_in_memory().expect("in-memory store must open"),
            config: self.config,
            app_index: self.apps.unwrap_or_else(|| {
                crate::apps::AppCatalog::fixed(AppIndex::new(vec![
                    AppEntry::new("Brave Browser").with_aliases(["brave"])
                ]))
            }),
            wake_word: "Adán".to_string(),
            base_dir: self.base_dir.unwrap_or_else(|| dir.path().to_path_buf()),
            desktop: desktop.clone(),
            agents: self.agents,
            audio: self.audio,
            formatter: self.formatter,
            events,
            mcp: self.mcp,
            worktrees_dir: dir.path().join("worktrees"),
            formatter_name: "reglas".to_string(),
            config_warnings: self.config_warnings,
            harvest_dir: dir.path().join("harvest"),
            agent_ledger_dir: dir.path().join("agents"),
            worker_process: crate::orphans::Process::current(),
        });
        Rig { ctx: Arc::new(ctx), desktop, rx, _dir: dir }
    }
}

/// A [`Formatter`] that can rewrite (like Apple Intelligence) with a fixed
/// answer, remembering what it was asked.
pub struct Rewriter {
    answer: String,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

impl Rewriter {
    /// A formatter whose `rewrite` always returns `answer`.
    pub fn returning(answer: &str) -> Rewriter {
        Rewriter { answer: answer.to_string(), seen: Arc::new(Mutex::new(Vec::new())) }
    }

    /// The `(text, instruction)` pairs it has been asked to rewrite.
    pub fn seen(&self) -> Arc<Mutex<Vec<(String, String)>>> {
        Arc::clone(&self.seen)
    }
}

impl Formatter for Rewriter {
    fn format(&self, text: &str) -> Result<String, FormatError> {
        RuleOnlyFormatter.format(text)
    }

    fn rewrite(&self, text: &str, instruction: &str) -> Result<String, FormatError> {
        #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
        self.seen.lock().unwrap().push((text.to_string(), instruction.to_string()));
        Ok(self.answer.clone())
    }
}

/// Lets a test keep a handle on a mock provider after the registry has taken
/// ownership of the box — the registry needs to own its providers, but the
/// test needs to inspect what they received.
pub struct SharedProvider(pub Arc<eva_agents::mock::MockProvider>);

#[async_trait]
impl AgentProvider for SharedProvider {
    fn id(&self) -> &'static str {
        self.0.id()
    }

    async fn detect(&self) -> ProviderStatus {
        self.0.detect().await
    }

    async fn execute(
        &self,
        task: &AgentTask,
        events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<RunningAgent, eva_agents::AgentError> {
        self.0.execute(task, events).await
    }
}

/// A registry over shared mock providers, in the order given.
pub fn registry_of(providers: &[&Arc<eva_agents::mock::MockProvider>]) -> AgentRegistry {
    AgentRegistry::new(
        providers.iter().map(|p| Box::new(SharedProvider(Arc::clone(p))) as Box<dyn AgentProvider>).collect(),
    )
}
