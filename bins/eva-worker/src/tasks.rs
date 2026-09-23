//! Agent tasks: dispatching a spoken request to Codex or Claude Code, keeping
//! it running in the background while the worker keeps serving other
//! commands, and everything that surrounds it — the throwaway worktree, the
//! session bookkeeping that "Adán, continúa" depends on, falling back to
//! the next agent when one fails before doing anything, cancelling, the time
//! limit, and telling the user how it went (`docs/PLAN.md` fase 6 and 7).

use crate::context::WorkerContext;
use eva_agents::{AgentEvent, AgentOutcome, AgentProvider, AgentTask, Workspace, Worktree};
use eva_config::{ActionKind, Origin};
use eva_gateway::{Action, Verdict};
use eva_ipc::{TaskInfo, TaskState, WorkerToShell};
use eva_store::NewTask;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use uuid::Uuid;

/// The tasks running right now, so they can be listed and cancelled.
#[derive(Default)]
pub struct TaskRegistry {
    tasks: Mutex<HashMap<Uuid, Entry>>,
}

struct Entry {
    provider: String,
    prompt: String,
    started: Instant,
    cancel: Option<oneshot::Sender<()>>,
}

impl TaskRegistry {
    /// Records that `id` is now running on `provider`, returning the signal
    /// that fires if it is cancelled. Registering the same id again (a
    /// fallback to the next agent) replaces the provider and the signal but
    /// keeps the original start time.
    pub fn register(&self, id: Uuid, provider: &str, prompt: &str) -> oneshot::Receiver<()> {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut tasks = self.lock();
        let started = tasks.get(&id).map_or_else(Instant::now, |e| e.started);
        tasks.insert(
            id,
            Entry { provider: provider.to_string(), prompt: prompt.to_string(), started, cancel: Some(cancel_tx) },
        );
        cancel_rx
    }

    /// Forgets a finished task.
    pub fn finish(&self, id: Uuid) {
        self.lock().remove(&id);
    }

    /// Whether `id` is a running task.
    #[cfg(test)]
    pub fn contains(&self, id: Uuid) -> bool {
        self.lock().contains_key(&id)
    }

    /// Asks the task to stop. `false` if there is no such running task.
    pub fn cancel(&self, id: Uuid) -> bool {
        let sender = self.lock().get_mut(&id).and_then(|entry| entry.cancel.take());
        sender.is_some_and(|tx| tx.send(()).is_ok())
    }

    /// Asks every running task to stop; returns how many were asked.
    pub fn cancel_all(&self) -> usize {
        let senders: Vec<_> = self.lock().values_mut().filter_map(|entry| entry.cancel.take()).collect();
        senders.into_iter().filter_map(|tx| tx.send(()).ok()).count()
    }

    /// The running tasks, oldest first.
    pub fn running(&self) -> Vec<TaskInfo> {
        let mut running: Vec<_> = self
            .lock()
            .iter()
            .map(|(id, e)| TaskInfo {
                request_id: *id,
                provider: e.provider.clone(),
                prompt: e.prompt.clone(),
                state: TaskState::Running,
                summary: None,
                age_secs: e.started.elapsed().as_secs(),
            })
            .collect();
        running.sort_by_key(|t| std::cmp::Reverse(t.age_secs));
        running
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Entry>> {
        #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
        self.tasks.lock().unwrap()
    }
}

/// An earlier agent session to continue instead of starting a new one.
pub struct Resume {
    /// The CLI's own id for the session.
    pub session_id: Uuid,
    /// The agent that ran it — the only one that can resume it.
    pub provider_id: String,
    /// Where it ran (a worktree, when one was used).
    pub work_dir: Option<PathBuf>,
}

/// A task to run.
pub struct TaskRequest {
    /// The request that asked for it; also the task's id everywhere.
    pub request_id: Uuid,
    /// The parsed voice command, for the audit trail.
    pub intent_json: serde_json::Value,
    /// What to ask the agent to do.
    pub prompt: String,
    /// The agent the user named ("usa Claude y…"), if any.
    pub forced_provider: Option<String>,
    /// The project the task is about.
    pub project_dir: PathBuf,
    /// The session to continue, for "Adán, continúa".
    pub resume: Option<Resume>,
}

/// "Adán, <tarea>": resolves the active project and runs a new task there.
pub async fn start_new(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    intent_json: serde_json::Value,
    prompt: String,
    forced_provider: Option<String>,
) {
    let (project_dir, why) = ctx.resolve_project().await;
    tracing::info!(project = %project_dir.display(), ?why, "proyecto de la tarea");
    run(ctx, TaskRequest { request_id, intent_json, prompt, forced_provider, project_dir, resume: None }).await;
}

/// "Adán, continúa": resumes the last session run in the active project, on
/// the same agent that ran it (never priority order: resuming on the wrong
/// CLI would not find the session at all). No prior session is a clear,
/// immediate error, not a silent fallback to starting something new —
/// "continúa" said nothing to start fresh with.
pub async fn continue_last(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    intent_json: serde_json::Value,
    extra_prompt: String,
) {
    let (project_dir, _why) = ctx.resolve_project().await;
    let key = project_dir.to_string_lossy().into_owned();

    let last = match ctx.store.get_last_session(&key) {
        Ok(Some(session)) => session,
        Ok(None) => {
            let project = project_dir.file_name().map_or_else(|| key.clone(), |n| n.to_string_lossy().into_owned());
            ctx.events
                .fail(request_id, format!("no hay ninguna tarea de agente que continuar en el proyecto {project}"));
            return;
        }
        Err(e) => {
            ctx.events.fail(request_id, e.to_string());
            return;
        }
    };

    let prompt = if extra_prompt.trim().is_empty() { "continúa".to_string() } else { extra_prompt };
    let resume = Resume {
        session_id: last.session_id,
        provider_id: last.provider_id,
        work_dir: last.work_dir.map(PathBuf::from),
    };
    run(ctx, TaskRequest { request_id, intent_json, prompt, forced_provider: None, project_dir, resume: Some(resume) })
        .await;
}

/// Runs one task from authorization to the final announcement. Returns when
/// the task is over; callers run this inside [`WorkerContext::spawn_job`], so
/// the command loop is never held.
pub async fn run(ctx: &Arc<WorkerContext>, req: TaskRequest) {
    let id = req.request_id;
    let events = &ctx.events;

    if req.prompt.trim().is_empty() {
        events.fail(id, "no dijiste qué debo encargarle al agente");
        return;
    }

    let action =
        Action::new(ActionKind::AgentTask, Origin::Voice, req.prompt.as_str()).with_intent(req.intent_json.clone());
    let ticket = match ctx.gateway.authorize(&action).await {
        Verdict::Allowed(ticket) => ticket,
        Verdict::Refused { reason } => {
            events.fail(id, reason);
            return;
        }
    };

    let forced = req.forced_provider.as_deref().or(req.resume.as_ref().map(|r| r.provider_id.as_str()));
    let candidates = match ctx.agents.candidates(forced).await {
        Ok(candidates) => candidates,
        Err(e) => {
            ctx.gateway.record_result(&ticket, &format!("sin agente disponible: {e}"));
            events.fail(id, e.to_string());
            return;
        }
    };

    let prepared = prepare_workspace(ctx, &req).await;
    let project_key = req.project_dir.to_string_lossy().into_owned();
    let work_dir_text = prepared.work_dir.to_string_lossy().into_owned();
    let first = candidates[0];

    let new_task = NewTask {
        id,
        provider_id: first.id(),
        prompt: &req.prompt,
        project_dir: &project_key,
        work_dir: Some(&work_dir_text),
        branch: prepared.worktree.as_ref().map(|w| w.branch.as_str()),
    };
    if let Err(e) = ctx.store.start_task(new_task) {
        tracing::warn!("no se pudo registrar la tarea en el historial: {e}");
    }
    let mut outcome = AgentOutcome::Failed { message: "no se pudo iniciar ningún agente".to_string() };
    let mut last_provider = first.id();
    for (index, provider) in candidates.iter().enumerate() {
        let is_last = index + 1 == candidates.len();
        last_provider = provider.id();
        let cancel_rx = ctx.tasks.register(id, provider.id(), &req.prompt);
        if index > 0 {
            if let Err(e) = ctx.store.set_task_provider(id, provider.id()) {
                tracing::warn!("no se pudo actualizar el agente de la tarea: {e}");
            }
        }
        events.emit(WorkerToShell::TaskStarted {
            request_id: id,
            provider: provider.id().to_string(),
            prompt: req.prompt.clone(),
        });

        let attempt = run_one(ctx, *provider, &req, &prepared, &project_key, cancel_rx).await;
        let (attempt_outcome, did_work) = match attempt {
            Attempt::NotStarted(message) => (AgentOutcome::Failed { message }, false),
            Attempt::Ran { outcome, did_work } => (outcome, did_work),
        };

        // An agent that failed before doing anything (an outdated CLI, no
        // login, a model it no longer accepts) must not make the command
        // fail while a working one sits unused — but never when the user
        // named the agent, and never once it has touched the project.
        let failed_early = matches!(attempt_outcome, AgentOutcome::Failed { .. }) && !did_work;
        outcome = attempt_outcome;
        if failed_early && !is_last && forced.is_none() {
            let next = candidates[index + 1].id();
            let reason =
                if let AgentOutcome::Failed { message } = &outcome { short(message, 120) } else { String::new() };
            emit_notice(ctx, id, &format!("{} no pudo arrancar ({reason}); pruebo con {next}", provider.id()));
            continue;
        }
        break;
    }

    finish(ctx, &req, &prepared, &ticket, last_provider, outcome).await;
}

/// Where a task runs: the worktree made for it, the directory its session
/// lives in (when resuming), or the project itself.
struct Prepared {
    work_dir: PathBuf,
    worktree: Option<Worktree>,
}

async fn prepare_workspace(ctx: &WorkerContext, req: &TaskRequest) -> Prepared {
    if let Some(resume) = &req.resume {
        // Both CLIs key a session to its working directory, so it must be
        // resumed from wherever it started — and a resumed task neither gets
        // a new worktree nor cleans the old one up.
        let work_dir = resume.work_dir.clone().filter(|d| d.is_dir()).unwrap_or_else(|| req.project_dir.clone());
        return Prepared { work_dir, worktree: None };
    }
    if !ctx.config.agents.worktree {
        return Prepared { work_dir: req.project_dir.clone(), worktree: None };
    }
    match eva_agents::worktree::prepare(&req.project_dir, req.request_id, &ctx.worktrees_dir).await {
        Workspace::Worktree(worktree) => Prepared { work_dir: worktree.work_dir.clone(), worktree: Some(worktree) },
        Workspace::InPlace { reason } => {
            tracing::info!(%reason, "la tarea corre en el proyecto mismo, sin worktree");
            Prepared { work_dir: req.project_dir.clone(), worktree: None }
        }
    }
}

enum Attempt {
    /// The process could not even be started.
    NotStarted(String),
    /// It ran; `did_work` is whether it touched anything (a tool call or a
    /// file change) before it ended.
    Ran { outcome: AgentOutcome, did_work: bool },
}

async fn run_one(
    ctx: &Arc<WorkerContext>,
    provider: &dyn AgentProvider,
    req: &TaskRequest,
    prepared: &Prepared,
    project_key: &str,
    cancel_rx: oneshot::Receiver<()>,
) -> Attempt {
    let task = AgentTask {
        prompt: req.prompt.clone(),
        project_dir: prepared.work_dir.clone(),
        session_id: req.request_id,
        resume_session_id: req.resume.as_ref().map(|r| r.session_id),
        mcp: ctx.mcp.clone(),
    };

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let running = match provider.execute(&task, tx).await {
        Ok(running) => running,
        Err(e) => return Attempt::NotStarted(e.to_string()),
    };

    // Recorded as soon as the provider accepted the task — a "continúa" in
    // this project should find it even if the task itself later fails, the
    // same way a real conversation stays resumable after an error. Claude's
    // session id is the one EVA assigned; Codex assigns its own, which
    // arrives later as `SessionAssigned`.
    match (&req.resume, provider.id()) {
        (Some(resume), id) => save_session(ctx, project_key, id, resume.session_id, &prepared.work_dir),
        (None, "claude_code") => save_session(ctx, project_key, "claude_code", req.request_id, &prepared.work_dir),
        (None, _) => {}
    }

    let forwarder = tokio::spawn(forward_events(
        ctx.clone(),
        req.request_id,
        provider.id(),
        project_key.to_string(),
        prepared.work_dir.clone(),
        rx,
    ));

    let timed_out = Arc::new(AtomicBool::new(false));
    let limit_secs = ctx.config.feedback.task_timeout_secs;
    let stop = {
        let timed_out = Arc::clone(&timed_out);
        async move {
            tokio::select! {
                _ = cancel_rx => {}
                () = time_limit(limit_secs) => timed_out.store(true, Ordering::SeqCst),
            }
        }
    };
    let outcome = running.wait_or_cancel(stop).await;
    let did_work = forwarder.await.unwrap_or(false);

    let outcome = match outcome {
        AgentOutcome::Cancelled if timed_out.load(Ordering::SeqCst) => {
            AgentOutcome::Failed { message: format!("se agotó el tiempo límite de {} minutos", limit_secs / 60) }
        }
        other => other,
    };
    Attempt::Ran { outcome, did_work }
}

/// Resolves after `secs`; never, for `0` ("no limit").
async fn time_limit(secs: u64) {
    if secs == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
}

/// Relays an agent's events to the shell and watches for the session id.
/// Returns whether the agent did any work.
async fn forward_events(
    ctx: Arc<WorkerContext>,
    request_id: Uuid,
    provider_id: &'static str,
    project_key: String,
    work_dir: PathBuf,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) -> bool {
    let mut did_work = false;
    while let Some(event) = rx.recv().await {
        match &event {
            AgentEvent::ToolCall { .. } | AgentEvent::FileChanged { .. } => did_work = true,
            AgentEvent::SessionAssigned { id } => {
                if let Ok(session_id) = Uuid::parse_str(id) {
                    save_session(&ctx, &project_key, provider_id, session_id, &work_dir);
                }
            }
            _ => {}
        }
        let event_json = serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
        ctx.events.emit(WorkerToShell::AgentEvent { request_id, event_json });
    }
    did_work
}

fn save_session(ctx: &WorkerContext, project_key: &str, provider_id: &str, session_id: Uuid, work_dir: &Path) {
    if let Err(e) = ctx.store.save_last_session(project_key, provider_id, session_id, Some(&work_dir.to_string_lossy()))
    {
        tracing::warn!("no se pudo guardar la sesión del agente para 'continúa': {e}");
    }
}

/// A one-off line of progress in the task's event stream ("Codex no pudo
/// arrancar; pruebo con claude_code").
fn emit_notice(ctx: &WorkerContext, request_id: Uuid, text: &str) {
    let event_json = serde_json::json!({"kind": "message", "text": text});
    ctx.events.emit(WorkerToShell::AgentEvent { request_id, event_json });
}

/// Bookkeeping, worktree cleanup and the announcement, once the outcome is known.
async fn finish(
    ctx: &Arc<WorkerContext>,
    req: &TaskRequest,
    prepared: &Prepared,
    ticket: &eva_gateway::Ticket,
    provider_id: &str,
    outcome: AgentOutcome,
) {
    let id = req.request_id;
    let (success, mut summary, cancelled) = match &outcome {
        AgentOutcome::Completed { summary } => (true, summary.clone().unwrap_or_else(|| "listo".to_string()), false),
        AgentOutcome::Failed { message } => (false, message.clone(), false),
        AgentOutcome::Cancelled => (false, "cancelada".to_string(), true),
    };

    if let Some(worktree) = &prepared.worktree {
        if !worktree.remove_if_untouched().await {
            summary = format!("{summary} — cambios en la rama {} ({})", worktree.branch, worktree.root.display());
        }
    }

    if let Err(e) = ctx.store.finish_task(id, success, &summary) {
        tracing::warn!("no se pudo cerrar la tarea en el historial: {e}");
    }
    ctx.gateway.record_result(ticket, &format!("{provider_id}: {summary}"));
    ctx.tasks.finish(id);

    ctx.events.emit(WorkerToShell::TaskFinished { request_id: id, success, summary: summary.clone() });
    ctx.events.state(id, eva_ipc::WorkerState::Done(success));

    if !cancelled {
        announce(ctx, success, &summary).await;
    }
}

/// Tells the user how a task went: a notification and a short spoken line,
/// each optional in the config (`feedback`). Never fails the task.
async fn announce(ctx: &WorkerContext, success: bool, summary: &str) {
    let feedback = &ctx.config.feedback;
    let (title, spoken) = if success {
        ("EVA01: tarea lista", format!("Terminé. {}", short(first_sentence(summary), 160)))
    } else {
        ("EVA01: la tarea falló", format!("La tarea falló. {}", short(first_sentence(summary), 160)))
    };
    let body = short(summary, 240);
    let (notify, speak) = (feedback.notify_task_results, feedback.speak_task_results);

    let desktop = Arc::clone(&ctx.desktop);
    let _ = tokio::task::spawn_blocking(move || {
        if notify {
            if let Err(e) = desktop.notify(title, &body) {
                tracing::warn!("no se pudo mostrar la notificación de la tarea: {e}");
            }
        }
        if speak {
            if let Err(e) = desktop.speak(&spoken) {
                tracing::warn!("no se pudo hablar el resultado de la tarea: {e}");
            }
        }
    })
    .await;
}

/// The first sentence of `text`, or all of it.
fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    text.find(['.', '\n']).map_or(text, |end| &text[..end])
}

/// `text` cut to `max` characters with an ellipsis, on one line.
fn short(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// The running tasks followed by the most recent finished ones, for the
/// shell's task list and `eva tasks`.
pub fn snapshot(ctx: &WorkerContext) -> Vec<TaskInfo> {
    let mut tasks = ctx.tasks.running();
    let running: std::collections::HashSet<Uuid> = tasks.iter().map(|t| t.request_id).collect();
    for record in ctx.store.recent_tasks(10).unwrap_or_default() {
        if running.contains(&record.id) {
            continue;
        }
        let age = (chrono_now() - record.started_at).num_seconds().max(0) as u64;
        tasks.push(TaskInfo {
            request_id: record.id,
            provider: record.provider_id,
            prompt: record.prompt,
            state: if record.success == Some(true) { TaskState::Succeeded } else { TaskState::Failed },
            summary: record.summary,
            age_secs: age,
        });
    }
    tasks
}

fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use async_trait::async_trait;
    use eva_agents::AgentRegistry;

    #[test]
    fn registering_lists_a_running_task_and_finishing_removes_it() {
        let registry = TaskRegistry::default();
        let id = Uuid::new_v4();
        let _cancel = registry.register(id, "codex", "agrega tests");

        let running = registry.running();
        assert_eq!(running.len(), 1);
        assert_eq!((running[0].provider.as_str(), running[0].state), ("codex", TaskState::Running));
        assert!(registry.contains(id));

        registry.finish(id);
        assert!(registry.running().is_empty());
        assert!(!registry.contains(id));
    }

    #[test]
    fn cancel_fires_the_tasks_signal_once() {
        let registry = TaskRegistry::default();
        let id = Uuid::new_v4();
        let mut cancel = registry.register(id, "codex", "x");

        assert!(registry.cancel(id));
        assert!(cancel.try_recv().is_ok(), "the task must have been told to stop");
        assert!(!registry.cancel(id), "a second cancel has nothing left to signal");
        assert!(!registry.cancel(Uuid::new_v4()), "an unknown task cannot be cancelled");
    }

    #[test]
    fn cancel_all_signals_every_running_task() {
        let registry = TaskRegistry::default();
        let mut receivers: Vec<_> =
            (0..3).map(|i| registry.register(Uuid::new_v4(), "codex", &format!("t{i}"))).collect();
        assert_eq!(registry.cancel_all(), 3);
        assert!(receivers.iter_mut().all(|rx| rx.try_recv().is_ok()));
        assert_eq!(registry.cancel_all(), 0);
    }

    #[test]
    fn re_registering_for_a_fallback_keeps_the_start_time_and_swaps_the_provider() {
        let registry = TaskRegistry::default();
        let id = Uuid::new_v4();
        let _first = registry.register(id, "codex", "x");
        let _second = registry.register(id, "claude_code", "x");
        let running = registry.running();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].provider, "claude_code");
    }

    use crate::testkit::{registry_of, Rig};
    use eva_agents::mock::MockProvider;
    use eva_agents::{AgentOutcome, ProviderStatus};
    use eva_ipc::{ShellToWorker, WorkerState};
    use eva_mcp::desktop::mock::{Call, MockDesktop};

    fn ask(text: &str) -> (Uuid, ShellToWorker) {
        let request_id = Uuid::new_v4();
        (request_id, ShellToWorker::RunIntentText { request_id, text: text.to_string() })
    }

    fn completes(id: &'static str, summary: &str) -> Arc<MockProvider> {
        Arc::new(MockProvider::always_completes(id, summary))
    }

    fn fails_early(id: &'static str, message: &str) -> Arc<MockProvider> {
        Arc::new(MockProvider::new(
            id,
            ProviderStatus::Active { version: "mock".into() },
            vec![(Vec::new(), AgentOutcome::Failed { message: message.to_string() })],
        ))
    }

    fn finished(events: &[WorkerToShell]) -> Option<(bool, String)> {
        events.iter().find_map(|e| match e {
            WorkerToShell::TaskFinished { success, summary, .. } => Some((*success, summary.clone())),
            _ => None,
        })
    }

    #[tokio::test]
    async fn a_task_with_no_agent_available_says_so() {
        let mut rig = Rig::new();
        let (_, command) = ask("Adán, agrega tests al login");
        let events = rig.run(command).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("disponible"))));
    }

    #[tokio::test]
    async fn a_completed_task_reports_started_then_finished_then_done_and_is_recorded() {
        let codex = completes("codex", "3 archivos cambiados");
        let mut rig = Rig::builder().agents(registry_of(&[&codex])).build();
        let (request_id, command) = ask("Adán, agrega tests");
        let events = rig.run(command).await;

        let started =
            events.iter().position(|e| matches!(e, WorkerToShell::TaskStarted { provider, .. } if provider == "codex"));
        let ended = events.iter().position(|e| matches!(e, WorkerToShell::TaskFinished { .. }));
        assert!(started.expect("TaskStarted") < ended.expect("TaskFinished"));
        assert_eq!(finished(&events), Some((true, "3 archivos cambiados".to_string())));
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));

        let history = rig.ctx.store.recent_tasks(5).expect("history");
        assert_eq!(history[0].id, request_id);
        assert_eq!(history[0].success, Some(true));
        assert!(rig.ctx.tasks.running().is_empty(), "a finished task is no longer listed as running");
    }

    #[tokio::test]
    async fn the_worker_keeps_dictating_and_listing_while_an_agent_works_and_can_cancel_it() {
        let slow = Arc::new(MockProvider::never_finishes("codex"));
        let mut rig = Rig::builder().agents(registry_of(&[&slow])).build();
        let (task_id, command) = ask("Adán, refactoriza todo el módulo");
        crate::handler::handle(&rig.ctx, command, None);
        rig.until(|e| matches!(e, WorkerToShell::TaskStarted { .. })).await;

        // Meanwhile: a dictation goes through end to end.
        let (dictation_id, dictation) = ask("hola mundo");
        crate::handler::handle(&rig.ctx, dictation, None);
        let seen = rig.until(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), request_id: Some(id) } if *id == dictation_id)).await;
        assert!(
            seen.iter().any(|e| matches!(e, WorkerToShell::Transcript { .. })),
            "the dictation must not wait for the agent"
        );
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Hola mundo. ".to_string())]);

        // The task is listed as running…
        crate::handler::handle(&rig.ctx, ShellToWorker::ListTasks { request_id: Uuid::new_v4() }, None);
        let listing = rig.until(|e| matches!(e, WorkerToShell::TaskList { .. })).await;
        let Some(WorkerToShell::TaskList { tasks, .. }) = listing.last() else { panic!("no task list") };
        assert_eq!(tasks.iter().filter(|t| t.state == TaskState::Running).count(), 1);

        // …and cancelling it ends it promptly.
        crate::handler::handle(&rig.ctx, ShellToWorker::Cancel { request_id: task_id }, None);
        let seen = rig.until(|e| matches!(e, WorkerToShell::TaskFinished { .. })).await;
        assert_eq!(finished(&seen), Some((false, "cancelada".to_string())));
        rig.ctx.wait_idle().await;
        assert!(rig.ctx.tasks.running().is_empty());
    }

    #[tokio::test]
    async fn cancel_all_stops_every_running_task() {
        let slow = Arc::new(MockProvider::never_finishes("codex"));
        let mut rig = Rig::builder().agents(registry_of(&[&slow])).build();
        for text in ["Adán, tarea uno", "Adán, tarea dos"] {
            let (_, command) = ask(text);
            crate::handler::handle(&rig.ctx, command, None);
        }
        let mut started = 0;
        while started < 2 {
            if let Some(WorkerToShell::TaskStarted { .. }) = rig.next_event().await {
                started += 1;
            }
        }

        crate::handler::handle(&rig.ctx, ShellToWorker::CancelAllTasks, None);
        rig.ctx.wait_idle().await;
        let events = rig.drain();
        assert_eq!(
            events.iter().filter(|e| matches!(e, WorkerToShell::TaskFinished { success: false, .. })).count(),
            2
        );
    }

    #[tokio::test]
    async fn a_task_that_runs_past_the_time_limit_is_stopped_and_says_so() {
        let slow = Arc::new(MockProvider::never_finishes("codex"));
        let mut rig =
            Rig::builder().agents(registry_of(&[&slow])).configure(|c| c.feedback.task_timeout_secs = 1).build();
        let (_, command) = ask("Adán, refactoriza todo");
        let events = rig.run(command).await;
        let (success, summary) = finished(&events).expect("the timeout must end the task");
        assert!(!success);
        assert!(summary.contains("tiempo límite"), "{summary}");
    }

    #[tokio::test]
    async fn an_agent_that_fails_before_doing_anything_hands_over_to_the_next_one() {
        let broken = fails_early("codex", "requires a newer version of Codex");
        let working = completes("claude_code", "hecho");
        let mut rig = Rig::builder().agents(registry_of(&[&broken, &working])).build();
        let (request_id, command) = ask("Adán, agrega tests");
        let events = rig.run(command).await;

        assert_eq!(finished(&events), Some((true, "hecho".to_string())));
        assert_eq!(working.received_tasks().len(), 1, "the second agent must have taken the task");
        let notice = events.iter().any(|e| {
            matches!(e, WorkerToShell::AgentEvent { event_json, .. }
            if event_json["text"].as_str().is_some_and(|t| t.contains("pruebo con claude_code")))
        });
        assert!(notice, "the user must be told why the first agent was skipped");
        assert_eq!(rig.ctx.store.recent_tasks(1).expect("history")[0].provider_id, "claude_code");
        let providers: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                WorkerToShell::TaskStarted { provider, request_id: id, .. } if *id == request_id => {
                    Some(provider.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(providers, vec!["codex", "claude_code"]);
    }

    #[tokio::test]
    async fn a_named_agent_is_never_silently_replaced_when_it_fails() {
        let broken = fails_early("codex", "requires a newer version");
        let working = completes("claude_code", "hecho");
        let mut rig = Rig::builder().agents(registry_of(&[&broken, &working])).build();
        let (_, command) = ask("Adán, usa codex y agrega tests");
        let events = rig.run(command).await;

        let (success, _) = finished(&events).expect("finished");
        assert!(!success);
        assert!(working.received_tasks().is_empty(), "the user said Codex; Claude must not run");
    }

    #[tokio::test]
    async fn usa_claude_runs_on_claude_even_though_codex_has_priority() {
        let codex = completes("codex", "no debería correr");
        let claude = completes("claude_code", "hecho");
        let mut rig = Rig::builder().agents(registry_of(&[&codex, &claude])).build();
        let (_, command) = ask("Adán, usa claude y arregla el login");
        let events = rig.run(command).await;

        assert_eq!(finished(&events), Some((true, "hecho".to_string())));
        assert!(codex.received_tasks().is_empty());
        assert_eq!(claude.received_tasks()[0].prompt, "arregla el login", "the connector is not part of the task");
    }

    #[tokio::test]
    async fn a_failure_after_the_agent_started_working_does_not_fall_back() {
        // Codex touched the project (a tool call) and then failed: running
        // Claude on a half-changed tree would compound the damage.
        let worked_then_failed = Arc::new(MockProvider::new(
            "codex",
            ProviderStatus::Active { version: "mock".into() },
            vec![(
                vec![AgentEvent::ToolCall { name: "shell".into(), summary: "npm test".into() }],
                AgentOutcome::Failed { message: "se cayó".into() },
            )],
        ));
        let working = completes("claude_code", "hecho");
        let mut rig = Rig::builder().agents(registry_of(&[&worked_then_failed, &working])).build();
        let (_, command) = ask("Adán, agrega tests");
        let events = rig.run(command).await;

        assert_eq!(finished(&events), Some((false, "se cayó".to_string())));
        assert!(working.received_tasks().is_empty());
    }

    #[tokio::test]
    async fn a_bare_wake_word_with_no_task_says_what_is_missing() {
        let codex = completes("codex", "x");
        let mut rig = Rig::builder().agents(registry_of(&[&codex])).build();
        let (_, command) = ask("Adán");
        let events = rig.run(command).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("qué debo"))));
        assert!(codex.received_tasks().is_empty());
    }

    #[tokio::test]
    async fn a_task_the_user_declines_to_confirm_never_starts() {
        let codex = completes("codex", "x");
        let mut rig = Rig::builder()
            .agents(registry_of(&[&codex]))
            .configure(|c| {
                c.gateway.voice.insert(ActionKind::AgentTask, eva_config::Policy::Confirm);
            })
            .build();
        let (_, command) = ask("Adán, agrega tests");
        let events = rig.run_answering(command, false).await;

        assert!(events.iter().any(
            |e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("agrega tests"))
        ));
        assert!(codex.received_tasks().is_empty());
    }

    #[tokio::test]
    async fn agents_are_given_the_mcp_server_when_the_gateway_socket_is_up() {
        let codex = completes("codex", "x");
        let mcp = eva_agents::McpInjection {
            command: PathBuf::from("/Apps/EVA01.app/Contents/MacOS/eva-mcp"),
            args: vec!["--gateway".into(), "/tmp/g.sock".into()],
            env: vec![("EVA_GATEWAY_TOKEN".into(), "t".into())],
        };
        let mut rig = Rig::builder().agents(registry_of(&[&codex])).mcp(mcp.clone()).build();
        let (_, command) = ask("Adán, agrega tests");
        rig.run(command).await;
        assert_eq!(codex.received_tasks()[0].mcp, Some(mcp));
    }

    #[tokio::test]
    async fn the_result_is_announced_by_notification_and_voice_when_enabled() {
        let codex = completes("codex", "Agregué tres tests. Todo pasa.");
        let mut rig = Rig::builder()
            .agents(registry_of(&[&codex]))
            .configure(|c| {
                c.feedback.notify_task_results = true;
                c.feedback.speak_task_results = true;
            })
            .build();
        let (_, command) = ask("Adán, agrega tests");
        rig.run(command).await;

        let calls = rig.desktop.calls();
        assert!(
            calls.contains(&Call::Notify(
                "EVA01: tarea lista".to_string(),
                "Agregué tres tests. Todo pasa.".to_string()
            )),
            "{calls:?}"
        );
        assert!(calls.contains(&Call::Speak("Terminé. Agregué tres tests".to_string())), "{calls:?}");
    }

    #[tokio::test]
    async fn a_cancelled_task_is_not_announced() {
        let slow = Arc::new(MockProvider::never_finishes("codex"));
        let mut rig =
            Rig::builder().agents(registry_of(&[&slow])).configure(|c| c.feedback.speak_task_results = true).build();
        let (task_id, command) = ask("Adán, refactoriza el módulo");
        crate::handler::handle(&rig.ctx, command, None);
        rig.until(|e| matches!(e, WorkerToShell::TaskStarted { .. })).await;
        crate::handler::handle(&rig.ctx, ShellToWorker::Cancel { request_id: task_id }, None);
        rig.until(|e| matches!(e, WorkerToShell::TaskFinished { .. })).await;
        rig.ctx.wait_idle().await;
        assert!(rig.desktop.calls().is_empty(), "the user cancelled it; they do not need to be told");
    }

    // ---- sessions and "continúa" ----

    #[tokio::test]
    async fn claudes_session_is_the_one_eva_assigned_and_is_saved_as_soon_as_the_task_starts() {
        let claude = completes("claude_code", "listo");
        let mut rig = Rig::builder().agents(registry_of(&[&claude])).build();
        let (request_id, command) = ask("Adán, arregla el login");
        rig.run(command).await;

        let saved = rig.ctx.store.get_last_session(&rig.ctx.base_dir.to_string_lossy()).expect("read").expect("saved");
        assert_eq!(saved.provider_id, "claude_code");
        assert_eq!(saved.session_id, request_id, "eva assigns Claude's session id");
    }

    #[tokio::test]
    async fn codexs_own_session_id_is_what_gets_saved_not_one_eva_made_up() {
        let thread = "01a0c7b4-347f-7dd3-8108-1649df00c5a6";
        let codex = Arc::new(MockProvider::new(
            "codex",
            ProviderStatus::Active { version: "mock".into() },
            vec![(
                vec![AgentEvent::SessionAssigned { id: thread.to_string() }],
                AgentOutcome::Completed { summary: None },
            )],
        ));
        let mut rig = Rig::builder().agents(registry_of(&[&codex])).build();
        let (request_id, command) = ask("Adán, arregla el login");
        rig.run(command).await;

        let saved = rig.ctx.store.get_last_session(&rig.ctx.base_dir.to_string_lossy()).expect("read").expect("saved");
        assert_eq!(saved.session_id, Uuid::parse_str(thread).expect("uuid"));
        assert_ne!(saved.session_id, request_id);
    }

    #[tokio::test]
    async fn continua_with_no_prior_session_in_the_project_is_a_clear_error() {
        let mut rig = Rig::new();
        let (_, command) = ask("Adán, continúa");
        let events = rig.run(command).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("continuar"))));
    }

    #[tokio::test]
    async fn continua_resumes_the_same_session_on_the_same_agent_that_ran_it() {
        let codex = Arc::new(MockProvider::new(
            "codex",
            ProviderStatus::Active { version: "mock".into() },
            vec![
                (
                    vec![AgentEvent::SessionAssigned { id: "01a0c7b4-347f-7dd3-8108-1649df00c5a6".into() }],
                    AgentOutcome::Completed { summary: None },
                ),
                (Vec::new(), AgentOutcome::Completed { summary: Some("seguí".into()) }),
            ],
        ));
        let claude = completes("claude_code", "no debería correr");
        let mut rig = Rig::builder().agents(registry_of(&[&codex, &claude])).build();

        let (_, first) = ask("Adán, arregla el login");
        rig.run(first).await;
        let (_, again) = ask("Adán, continúa");
        let events = rig.run(again).await;

        assert_eq!(finished(&events), Some((true, "seguí".to_string())));
        let tasks = codex.received_tasks();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].resume_session_id, None);
        assert_eq!(
            tasks[1].resume_session_id,
            Some(Uuid::parse_str("01a0c7b4-347f-7dd3-8108-1649df00c5a6").expect("uuid"))
        );
        assert_eq!(tasks[1].prompt, "continúa");
        assert!(claude.received_tasks().is_empty(), "resuming on the wrong CLI would not find the session");
    }

    #[tokio::test]
    async fn continua_with_an_extra_instruction_passes_it_as_the_prompt() {
        let claude = completes("claude_code", "listo");
        let mut rig = Rig::builder().agents(registry_of(&[&claude])).build();
        let (first_id, first) = ask("Adán, arregla el login");
        rig.run(first).await;
        let (_, again) = ask("Adán, continúa y agrega también tests");
        rig.run(again).await;

        let tasks = claude.received_tasks();
        assert_eq!(tasks[1].prompt, "y agrega también tests");
        assert_eq!(tasks[1].resume_session_id, Some(first_id));
    }

    // ---- worktrees, with a real git repository ----

    async fn git(dir: &Path, args: &[&str]) {
        let status =
            tokio::process::Command::new("git").arg("-C").arg(dir).args(args).status().await.expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "eva@test"],
            vec!["config", "user.name", "EVA"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git(dir.path(), &args).await;
        }
        std::fs::write(dir.path().join("README.md"), "hola").expect("write");
        git(dir.path(), &["add", "-A"]).await;
        git(dir.path(), &["commit", "-q", "-m", "inicial"]).await;
        dir
    }

    #[tokio::test]
    async fn a_task_runs_in_its_own_worktree_and_an_untouched_one_is_cleaned_up() {
        let project = repo().await;
        let codex = completes("codex", "solo respondí");
        let mut rig = Rig::builder()
            .agents(registry_of(&[&codex]))
            .base_dir(project.path().to_path_buf())
            .configure(|c| c.agents.worktree = true)
            .build();
        let (_, command) = ask("Adán, explícame el proyecto");
        let events = rig.run(command).await;

        let ran_in = codex.received_tasks()[0].project_dir.clone();
        assert_ne!(ran_in, project.path(), "the agent must not run in the user's own tree");
        assert!(ran_in.starts_with(&rig.ctx.worktrees_dir));
        assert_eq!(finished(&events), Some((true, "solo respondí".to_string())), "no branch note when nothing changed");
        assert!(!ran_in.exists(), "an untouched worktree is removed");
    }

    /// An agent that edits the project it is given.
    struct Editor;

    #[async_trait]
    impl AgentProvider for Editor {
        fn id(&self) -> &'static str {
            "codex"
        }
        async fn detect(&self) -> ProviderStatus {
            ProviderStatus::Active { version: "mock".into() }
        }
        async fn execute(
            &self,
            task: &AgentTask,
            events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        ) -> Result<eva_agents::RunningAgent, eva_agents::AgentError> {
            std::fs::write(task.project_dir.join("nuevo.rs"), "fn main() {}").expect("the agent writes a file");
            let _ = events.send(AgentEvent::FileChanged { path: "nuevo.rs".into() });
            MockProvider::always_completes("codex", "agregué nuevo.rs").execute(task, events).await
        }
    }

    #[tokio::test]
    async fn a_worktree_where_the_agent_changed_files_is_kept_and_named_in_the_summary() {
        let project = repo().await;
        let mut rig = Rig::builder()
            .agents(AgentRegistry::new(vec![Box::new(Editor)]))
            .base_dir(project.path().to_path_buf())
            .configure(|c| c.agents.worktree = true)
            .build();
        let (_, command) = ask("Adán, agrega nuevo.rs");
        let events = rig.run(command).await;

        let (success, summary) = finished(&events).expect("finished");
        assert!(success);
        assert!(summary.contains("cambios en la rama eva/"), "{summary}");
        let record = &rig.ctx.store.recent_tasks(1).expect("history")[0];
        let branch = record.branch.clone().expect("the branch is recorded");
        assert!(summary.contains(&branch));
        assert!(
            Path::new(record.work_dir.as_deref().expect("work dir")).join("nuevo.rs").exists(),
            "the work is kept for review"
        );
        assert!(!project.path().join("nuevo.rs").exists(), "the user's own tree was never touched");
    }

    #[tokio::test]
    async fn a_project_that_is_not_a_repo_still_runs_in_place() {
        let project = tempfile::tempdir().expect("tempdir");
        let codex = completes("codex", "hecho");
        let mut rig = Rig::builder()
            .agents(registry_of(&[&codex]))
            .base_dir(project.path().to_path_buf())
            .configure(|c| c.agents.worktree = true)
            .build();
        let (_, command) = ask("Adán, refactoriza el módulo");
        let events = rig.run(command).await;
        assert_eq!(finished(&events), Some((true, "hecho".to_string())));
        assert_eq!(codex.received_tasks()[0].project_dir, project.path());
    }

    #[tokio::test]
    async fn continua_resumes_inside_the_worktree_the_session_started_in() {
        let project = repo().await;
        let claude = completes("claude_code", "listo");
        let mut rig = Rig::builder()
            .agents(AgentRegistry::new(vec![Box::new(crate::testkit::SharedProvider(Arc::clone(&claude)))]))
            .base_dir(project.path().to_path_buf())
            .configure(|c| c.agents.worktree = true)
            .build();
        // A first task that leaves a change behind, so its worktree stays.
        let (_, first) = ask("Adán, arregla el login");
        crate::handler::handle(&rig.ctx, first, None);
        rig.until(|e| matches!(e, WorkerToShell::TaskStarted { .. })).await;
        let first_dir = claude.received_tasks()[0].project_dir.clone();
        std::fs::write(first_dir.join("cambio.txt"), "x").expect("the agent's change");
        rig.ctx.wait_idle().await;
        rig.drain();

        let (_, again) = ask("Adán, continúa");
        rig.run(again).await;

        let second_dir = claude.received_tasks()[1].project_dir.clone();
        assert_eq!(second_dir, first_dir, "a session must be resumed from the directory it lives in");
    }

    #[tokio::test]
    async fn a_desktop_that_cannot_announce_does_not_fail_the_task() {
        let codex = completes("codex", "hecho");
        let mut rig = Rig::builder()
            .agents(registry_of(&[&codex]))
            .desktop(MockDesktop::failing())
            .configure(|c| c.feedback.speak_task_results = true)
            .build();
        let (_, command) = ask("Adán, refactoriza el módulo");
        let events = rig.run(command).await;
        assert_eq!(finished(&events), Some((true, "hecho".to_string())));
    }

    #[tokio::test]
    async fn the_task_list_shows_finished_tasks_from_the_history() {
        let codex = completes("codex", "hecho");
        let mut rig = Rig::builder().agents(registry_of(&[&codex])).build();
        let (_, command) = ask("Adán, refactoriza el módulo");
        rig.run(command).await;

        let events = rig.run(ShellToWorker::ListTasks { request_id: Uuid::new_v4() }).await;
        let Some(WorkerToShell::TaskList { tasks, .. }) =
            events.into_iter().find(|e| matches!(e, WorkerToShell::TaskList { .. }))
        else {
            panic!("no task list")
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].state, tasks[0].summary.as_deref()), (TaskState::Succeeded, Some("hecho")));
    }

    #[test]
    fn first_sentence_and_short_shape_the_spoken_summary() {
        assert_eq!(first_sentence("Agregué 3 tests. Todo pasa."), "Agregué 3 tests");
        assert_eq!(first_sentence("una sola frase"), "una sola frase");
        assert_eq!(first_sentence("línea uno\nlínea dos"), "línea uno");
        assert_eq!(short("a  b\nc", 10), "a b c");
        assert_eq!(short(&"x".repeat(50), 10), format!("{}…", "x".repeat(10)));
    }
}
