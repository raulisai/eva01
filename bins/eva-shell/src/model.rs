//! What the shell knows and what it should be showing: the state machine of
//! `docs/PLAN.md` §3.3 point 4 — `IDLE → ESCUCHANDO → PENSANDO → EJECUTANDO
//! → ✓/✗` — with its watchdog. Pure logic over explicit timestamps, so every
//! rule (a hung transcription must not leave the overlay on "Pensando…"
//! forever; a dictation started while an agent works must not be hidden by
//! it; a worker that died must not leave a stale prompt) is a unit test.
//!
//! Requests are tracked one by one, not as a single global state, because
//! they overlap: an agent task runs for minutes while the user keeps
//! dictating. A request that turns into a background task (`TaskStarted`)
//! stops being an overlay state and becomes a line in the tray.

use eva_ipc::{TaskInfo, TaskState, WorkerState, WorkerToShell};
use eva_macos::{OverlayContent, Tone};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// A recording the key never released (a lost key-up) is stopped after this.
pub const LISTENING_LIMIT: Duration = Duration::from_secs(120);
/// Transcription and formatting take a couple of seconds; past this the
/// request is stuck (`docs/PLAN.md` §3.3 point 4's watchdog).
pub const THINKING_LIMIT: Duration = Duration::from_secs(45);
/// A quick action (open an app, paste) that has not finished by now never will.
pub const EXECUTING_LIMIT: Duration = Duration::from_secs(90);
const OK_NOTICE: Duration = Duration::from_millis(1_400);
const ERROR_NOTICE: Duration = Duration::from_millis(4_500);
const TASK_RESULT_NOTICE: Duration = Duration::from_millis(5_000);

/// Something the shell must send to the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start capturing.
    StartRecording(Uuid),
    /// Stop capturing and process what was said.
    StopRecording(Uuid),
    /// Abandon a recording or stop waiting on a request.
    Cancel(Uuid),
    /// The user's answer to a confirmation.
    Confirm {
        /// The question answered.
        id: Uuid,
        /// Yes or no.
        approved: bool,
    },
    /// Cancel every background task.
    CancelAllTasks,
    /// Ask for the task list.
    ListTasks(Uuid),
}

/// A system notification the shell should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The title.
    pub title: String,
    /// The body.
    pub body: String,
}

/// Which icon the tray shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    /// Nothing happening.
    Idle,
    /// Recording.
    Listening,
    /// Working: transcribing, executing, or an agent running.
    Busy,
    /// Something needs attention: an error, or the worker restarting.
    Attention,
}

/// What the tray should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    /// The icon.
    pub icon: TrayIcon,
    /// The first, disabled line of the menu.
    pub status: String,
    /// Agent tasks running now, oldest first.
    pub running: Vec<TaskInfo>,
    /// The most recent finished tasks.
    pub recent: Vec<TaskInfo>,
}

/// The labels shown for the two confirmation keys, e.g. `⌘⏎` and `⌘⎋`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLabels {
    /// The "yes" key.
    pub confirm: String,
    /// The "no" key.
    pub cancel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Listening,
    Thinking,
    Executing,
}

struct Tracked {
    phase: Phase,
    since: Instant,
}

struct Confirmation {
    id: Uuid,
    title: String,
    detail: String,
    deadline: Instant,
}

struct Notice {
    text: String,
    tone: Tone,
    until: Instant,
}

/// The shell's state.
pub struct ShellModel {
    keys: KeyLabels,
    ready: bool,
    restarting: bool,
    recording: Option<Uuid>,
    requests: HashMap<Uuid, Tracked>,
    tasks: HashMap<Uuid, (TaskInfo, Instant)>,
    recent: Vec<TaskInfo>,
    confirmation: Option<Confirmation>,
    notice: Option<Notice>,
    notifications: Vec<Notification>,
}

impl ShellModel {
    /// A model before the worker has said it is ready.
    pub fn new(keys: KeyLabels) -> ShellModel {
        ShellModel {
            keys,
            ready: false,
            restarting: false,
            recording: None,
            requests: HashMap::new(),
            tasks: HashMap::new(),
            recent: Vec::new(),
            confirmation: None,
            notice: None,
            notifications: Vec::new(),
        }
    }

    // ---- input from the user ----

    /// The dictation key went down. Starts a recording if the worker is up
    /// and nothing else has the user's attention.
    pub fn press(&mut self, now: Instant, request_id: Uuid) -> Vec<Command> {
        if self.confirmation.is_some() || self.recording.is_some() {
            return Vec::new();
        }
        if !self.ready {
            let text = if self.restarting { "EVA se está reiniciando…" } else { "EVA se está iniciando…" };
            self.set_notice(now, text, Tone::Neutral, ERROR_NOTICE);
            return Vec::new();
        }
        self.notice = None;
        self.recording = Some(request_id);
        self.requests.insert(request_id, Tracked { phase: Phase::Listening, since: now });
        vec![Command::StartRecording(request_id)]
    }

    /// The dictation key came up. Stops the recording, if one is running.
    pub fn release(&mut self, now: Instant) -> Vec<Command> {
        let Some(id) = self.recording.take() else { return Vec::new() };
        self.requests.insert(id, Tracked { phase: Phase::Thinking, since: now });
        vec![Command::StopRecording(id)]
    }

    /// The "no" key: answers a pending confirmation with no, or abandons the
    /// recording in progress.
    pub fn cancel_key(&mut self) -> Vec<Command> {
        if let Some(confirmation) = self.confirmation.take() {
            return vec![Command::Confirm { id: confirmation.id, approved: false }];
        }
        if let Some(id) = self.recording.take() {
            self.requests.remove(&id);
            return vec![Command::Cancel(id)];
        }
        Vec::new()
    }

    /// The "yes" key: answers a pending confirmation with yes.
    pub fn confirm_key(&mut self) -> Vec<Command> {
        match self.confirmation.take() {
            Some(confirmation) => vec![Command::Confirm { id: confirmation.id, approved: true }],
            None => Vec::new(),
        }
    }

    /// Whether the "yes"/"no" keys should be listening right now.
    pub fn wants_confirmation_keys(&self) -> bool {
        self.confirmation.is_some()
    }

    /// Whether the "no" key should be listening right now (to cancel a
    /// recording or refuse a prompt) — registered only then, so EVA does not
    /// hold a system-wide shortcut it does not need.
    pub fn wants_cancel_key(&self) -> bool {
        self.confirmation.is_some() || self.recording.is_some()
    }

    // ---- input from the worker ----

    /// The worker died and is being restarted: everything it was doing is
    /// gone with it, so nothing may keep waiting on it.
    pub fn worker_restarting(&mut self) {
        self.ready = false;
        self.restarting = true;
        self.recording = None;
        self.requests.clear();
        self.tasks.clear();
        self.confirmation = None;
        self.notice = None;
    }

    /// An event from the worker. Returns anything to send back.
    pub fn worker_event(&mut self, now: Instant, event: WorkerToShell) -> Vec<Command> {
        match event {
            WorkerToShell::Ready => {
                self.ready = true;
                self.restarting = false;
                return vec![Command::ListTasks(Uuid::new_v4())];
            }
            WorkerToShell::StateChanged { state, request_id: Some(id) } => self.state_changed(now, id, state),
            WorkerToShell::Error { request_id, message, .. } => {
                if let Some(id) = request_id {
                    self.forget(id);
                }
                self.set_notice(now, &format!("✗ {}", short(&message, 90)), Tone::Error, ERROR_NOTICE);
            }
            WorkerToShell::TaskStarted { request_id, provider, prompt } => {
                self.forget(request_id);
                let info =
                    TaskInfo { request_id, provider, prompt, state: TaskState::Running, summary: None, age_secs: 0 };
                self.tasks.insert(request_id, (info, now));
            }
            WorkerToShell::TaskFinished { request_id, success, summary } => {
                self.tasks.remove(&request_id);
                self.forget(request_id);
                let (text, tone) = task_notice(success, &summary);
                self.set_notice(now, &text, tone, TASK_RESULT_NOTICE);
                return vec![Command::ListTasks(Uuid::new_v4())];
            }
            WorkerToShell::TaskList { tasks, .. } => {
                self.recent = tasks.into_iter().filter(|t| t.state != TaskState::Running).take(5).collect();
            }
            WorkerToShell::ConfirmationRequested { confirmation_id, title, detail, timeout_secs } => {
                self.confirmation = Some(Confirmation {
                    id: confirmation_id,
                    title,
                    detail,
                    deadline: now + Duration::from_secs(timeout_secs),
                });
            }
            WorkerToShell::ConfirmationClosed { confirmation_id } => {
                if self.confirmation.as_ref().is_some_and(|c| c.id == confirmation_id) {
                    self.confirmation = None;
                }
            }
            // Informational for the overlay: the worker pastes text itself,
            // and these are for logs and the CLI.
            WorkerToShell::StateChanged { request_id: None, .. }
            | WorkerToShell::Transcript { .. }
            | WorkerToShell::IntentRecognized { .. }
            | WorkerToShell::AgentEvent { .. }
            | WorkerToShell::Health { .. }
            | WorkerToShell::CustomWords { .. }
            | WorkerToShell::Ack { .. } => {}
        }
        Vec::new()
    }

    fn state_changed(&mut self, now: Instant, id: Uuid, state: WorkerState) {
        if self.tasks.contains_key(&id) {
            return; // a background task: shown in the tray, not the overlay
        }
        match state {
            WorkerState::Listening => {
                self.requests.insert(id, Tracked { phase: Phase::Listening, since: now });
            }
            WorkerState::Thinking => {
                self.requests.insert(id, Tracked { phase: Phase::Thinking, since: now });
            }
            WorkerState::Executing => {
                self.requests.insert(id, Tracked { phase: Phase::Executing, since: now });
            }
            WorkerState::Idle => self.forget(id),
            WorkerState::Done(success) => {
                self.forget(id);
                // Never over a message that already says more: an `Error`
                // (or a task's summary) arrives just before its `Done`.
                if self.notice.is_none() {
                    if success {
                        self.set_notice(now, "✓ Listo", Tone::Ok, OK_NOTICE);
                    } else {
                        self.set_notice(now, "✗ Algo falló", Tone::Error, ERROR_NOTICE);
                    }
                }
            }
        }
    }

    /// Stops tracking `id`, including as the recording in progress.
    fn forget(&mut self, id: Uuid) {
        self.requests.remove(&id);
        if self.recording == Some(id) {
            self.recording = None;
        }
    }

    // ---- time ----

    /// Advances time: expires notices and prompts, and fires the watchdog on
    /// anything stuck. Returns what to send the worker.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        if self.notice.as_ref().is_some_and(|n| now >= n.until) {
            self.notice = None;
        }
        if self.confirmation.as_ref().is_some_and(|c| now >= c.deadline) {
            self.confirmation = None;
        }

        let mut commands = Vec::new();
        let stuck: Vec<(Uuid, Phase)> = self
            .requests
            .iter()
            .filter(|(_, t)| now.duration_since(t.since) >= limit_of(t.phase))
            .map(|(id, t)| (*id, t.phase))
            .collect();

        for (id, phase) in stuck {
            match phase {
                Phase::Listening => {
                    // The key-up never arrived. Stop as if it had, so what
                    // was said is not lost.
                    self.recording = None;
                    self.requests.insert(id, Tracked { phase: Phase::Thinking, since: now });
                    commands.push(Command::StopRecording(id));
                    self.notifications.push(Notification {
                        title: "EVA01".to_string(),
                        body: "La grabación llevaba dos minutos abierta; la detuve.".to_string(),
                    });
                }
                Phase::Thinking | Phase::Executing => {
                    self.forget(id);
                    commands.push(Command::Cancel(id));
                    self.set_notice(now, "✗ EVA se quedó trabada; la detuve", Tone::Error, ERROR_NOTICE);
                    self.notifications.push(Notification {
                        title: "EVA01".to_string(),
                        body: "Una petición se quedó sin responder y la cancelé.".to_string(),
                    });
                }
            }
        }
        commands
    }

    fn set_notice(&mut self, now: Instant, text: &str, tone: Tone, lasts: Duration) {
        self.notice = Some(Notice { text: text.to_string(), tone, until: now + lasts });
    }

    /// Notifications the shell should show since the last call.
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }

    // ---- output ----

    /// What the overlay should show, if anything. A question outranks
    /// everything (it is blocking something), then what the user is doing
    /// right now, then how the last thing went.
    pub fn overlay(&self) -> Option<OverlayContent> {
        let show = |text: &str, tone| Some(OverlayContent { text: text.to_string(), tone });

        if self.restarting {
            return show("↻ Reiniciando EVA…", Tone::Neutral);
        }
        if let Some(confirmation) = &self.confirmation {
            let text = format!(
                "{}\n{}\n{} sí  ·  {} no",
                short(&confirmation.title, 70),
                short(&confirmation.detail, 70),
                self.keys.confirm,
                self.keys.cancel
            );
            return show(&text, Tone::Ask);
        }
        if self.requests.values().any(|t| t.phase == Phase::Listening) {
            return show("● Escuchando…", Tone::Neutral);
        }
        if self.requests.values().any(|t| t.phase == Phase::Thinking) {
            return show("◌ Pensando…", Tone::Neutral);
        }
        if self.requests.values().any(|t| t.phase == Phase::Executing) {
            return show("▶ Ejecutando…", Tone::Neutral);
        }
        self.notice.as_ref().map(|n| OverlayContent { text: n.text.clone(), tone: n.tone })
    }

    /// What the tray should show, as of `now` (task ages are relative to it).
    pub fn tray(&self, now: Instant) -> TrayView {
        let mut running: Vec<TaskInfo> = self
            .tasks
            .values()
            .map(|(info, started)| TaskInfo { age_secs: now.duration_since(*started).as_secs(), ..info.clone() })
            .collect();
        running.sort_by_key(|t| std::cmp::Reverse(t.age_secs));

        let busy = !self.requests.is_empty() || !running.is_empty();
        let icon = if self.restarting || self.notice.as_ref().is_some_and(|n| n.tone == Tone::Error) {
            TrayIcon::Attention
        } else if self.recording.is_some() {
            TrayIcon::Listening
        } else if busy {
            TrayIcon::Busy
        } else {
            TrayIcon::Idle
        };

        let status = if self.restarting {
            "Reiniciando…".to_string()
        } else if !self.ready {
            "Iniciando…".to_string()
        } else if self.recording.is_some() {
            "Escuchando…".to_string()
        } else if !running.is_empty() {
            format!("{} tarea(s) en curso", running.len())
        } else {
            "Listo".to_string()
        };

        TrayView { icon, status, running, recent: self.recent.clone() }
    }
}

fn limit_of(phase: Phase) -> Duration {
    match phase {
        Phase::Listening => LISTENING_LIMIT,
        Phase::Thinking => THINKING_LIMIT,
        Phase::Executing => EXECUTING_LIMIT,
    }
}

/// The overlay line for a finished task.
fn task_notice(success: bool, summary: &str) -> (String, Tone) {
    if summary == "cancelada" {
        ("Tarea cancelada".to_string(), Tone::Neutral)
    } else if success {
        (format!("✓ Tarea lista: {}", short(summary, 70)), Tone::Ok)
    } else {
        (format!("✗ La tarea falló: {}", short(summary, 70)), Tone::Error)
    }
}

/// `text` on one line, cut to `max` characters with an ellipsis.
pub fn short(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max.saturating_sub(1)).collect::<String>())
    } else {
        flat
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn keys() -> KeyLabels {
        KeyLabels { confirm: "⌘⏎".to_string(), cancel: "⌘⎋".to_string() }
    }

    fn ready_model(now: Instant) -> ShellModel {
        let mut model = ShellModel::new(keys());
        model.worker_event(now, WorkerToShell::Ready);
        model
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn text(model: &ShellModel) -> Option<String> {
        model.overlay().map(|o| o.text)
    }

    fn state(id: Uuid, state: WorkerState) -> WorkerToShell {
        WorkerToShell::StateChanged { state, request_id: Some(id) }
    }

    // ---- the dictation cycle ----

    #[test]
    fn pressing_starts_a_recording_and_shows_listening() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();

        assert_eq!(model.press(t0, id), vec![Command::StartRecording(id)]);
        assert_eq!(text(&model).as_deref(), Some("● Escuchando…"));
        assert_eq!(model.tray(t0).icon, TrayIcon::Listening);
    }

    #[test]
    fn releasing_stops_the_recording_and_shows_thinking_until_the_worker_finishes() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);

        assert_eq!(model.release(t0 + secs(2)), vec![Command::StopRecording(id)]);
        assert_eq!(text(&model).as_deref(), Some("◌ Pensando…"));

        model.worker_event(t0 + secs(3), state(id, WorkerState::Done(true)));
        assert_eq!(text(&model).as_deref(), Some("✓ Listo"));
        assert_eq!(model.overlay().unwrap().tone, Tone::Ok);
    }

    #[test]
    fn the_done_message_goes_away_by_itself_and_the_overlay_hides() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        model.release(t0);
        model.worker_event(t0, state(id, WorkerState::Done(true)));

        model.tick(t0 + Duration::from_millis(500));
        assert!(model.overlay().is_some());
        model.tick(t0 + secs(2));
        assert_eq!(model.overlay(), None, "✓ Listo must not stay on screen forever");
        assert_eq!(model.tray(t0).icon, TrayIcon::Idle);
    }

    #[test]
    fn releasing_with_nothing_recording_does_nothing() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        assert_eq!(model.release(t0), Vec::new());
    }

    #[test]
    fn a_second_press_while_recording_is_ignored() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.press(t0, Uuid::new_v4());
        assert_eq!(model.press(t0, Uuid::new_v4()), Vec::new());
    }

    #[test]
    fn idle_shows_nothing() {
        let t0 = Instant::now();
        let model = ready_model(t0);
        assert_eq!(model.overlay(), None);
        assert_eq!(model.tray(t0).status, "Listo");
    }

    // ---- errors ----

    #[test]
    fn an_error_shows_its_message_and_the_done_that_follows_does_not_replace_it() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        model.release(t0);

        model.worker_event(
            t0,
            WorkerToShell::Error { request_id: Some(id), message: "el modelo no respondió".into(), recoverable: true },
        );
        model.worker_event(t0, state(id, WorkerState::Done(false)));

        let overlay = model.overlay().unwrap();
        assert_eq!(overlay.text, "✗ el modelo no respondió");
        assert_eq!(overlay.tone, Tone::Error);
        assert_eq!(model.tray(t0).icon, TrayIcon::Attention);
    }

    #[test]
    fn a_failure_with_no_message_still_says_something_failed() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Thinking));
        model.worker_event(t0, state(id, WorkerState::Done(false)));
        assert_eq!(text(&model).as_deref(), Some("✗ Algo falló"));
    }

    #[test]
    fn an_error_for_the_recording_ends_it_so_releasing_the_key_sends_nothing() {
        // No STT model configured: the worker refuses `StartRecording` while
        // the user is still holding the key.
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        model.worker_event(
            t0,
            WorkerToShell::Error { request_id: Some(id), message: "no hay modelo".into(), recoverable: true },
        );

        assert_eq!(model.release(t0 + secs(1)), Vec::new(), "there is no recording to stop");
        assert_eq!(text(&model).as_deref(), Some("✗ no hay modelo"));
    }

    #[test]
    fn a_long_error_message_is_cut_to_fit_the_overlay() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(t0, WorkerToShell::Error { request_id: None, message: "x".repeat(300), recoverable: true });
        assert!(text(&model).unwrap().chars().count() < 100);
    }

    // ---- the watchdog ----

    #[test]
    fn a_request_stuck_thinking_is_cancelled_and_the_overlay_recovers() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        model.release(t0);

        assert_eq!(model.tick(t0 + secs(30)), Vec::new(), "still within the limit");
        assert_eq!(model.tick(t0 + THINKING_LIMIT), vec![Command::Cancel(id)]);

        assert_eq!(text(&model).as_deref(), Some("✗ EVA se quedó trabada; la detuve"));
        assert_eq!(model.take_notifications().len(), 1, "the user must be told, not left guessing");
        model.tick(t0 + THINKING_LIMIT + secs(10));
        assert_eq!(model.overlay(), None, "and then it returns to idle");
    }

    #[test]
    fn a_recording_whose_key_up_was_lost_is_stopped_after_the_limit() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);

        assert_eq!(model.tick(t0 + LISTENING_LIMIT), vec![Command::StopRecording(id)]);
        assert_eq!(text(&model).as_deref(), Some("◌ Pensando…"), "what was said is still processed");
        assert_eq!(model.release(t0 + LISTENING_LIMIT + secs(1)), Vec::new(), "the late key-up has nothing left to do");
    }

    #[test]
    fn a_stuck_quick_action_is_also_caught() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Executing));
        assert_eq!(model.tick(t0 + EXECUTING_LIMIT), vec![Command::Cancel(id)]);
    }

    #[test]
    fn each_state_change_restarts_that_requests_clock() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Thinking));
        model.worker_event(t0 + secs(40), state(id, WorkerState::Executing));
        assert_eq!(model.tick(t0 + secs(50)), Vec::new(), "progress was made at 40s; 45s from the start is not stuck");
    }

    #[test]
    fn a_background_task_is_never_a_stuck_request() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Thinking));
        model.worker_event(
            t0,
            WorkerToShell::TaskStarted { request_id: id, provider: "codex".into(), prompt: "x".into() },
        );

        assert_eq!(model.tick(t0 + secs(3_600)), Vec::new(), "an agent may legitimately run for an hour");
    }

    // ---- overlapping work ----

    #[test]
    fn dictating_while_an_agent_works_shows_the_dictation_not_the_agent() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let task = Uuid::new_v4();
        model.worker_event(
            t0,
            WorkerToShell::TaskStarted { request_id: task, provider: "codex".into(), prompt: "refactoriza".into() },
        );
        assert_eq!(model.overlay(), None, "a background task alone shows nothing on the overlay");
        assert_eq!(model.tray(t0).icon, TrayIcon::Busy);
        assert_eq!(model.tray(t0).status, "1 tarea(s) en curso");

        let dictation = Uuid::new_v4();
        model.press(t0, dictation);
        assert_eq!(text(&model).as_deref(), Some("● Escuchando…"));
        model.release(t0);
        model.worker_event(t0, state(dictation, WorkerState::Done(true)));
        model.tick(t0 + secs(5));
        assert_eq!(model.overlay(), None);
        assert_eq!(model.tray(t0).status, "1 tarea(s) en curso", "and the task is still running");
    }

    #[test]
    fn a_running_tasks_age_grows_with_the_clock_and_oldest_comes_first() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let (old, recent) = (Uuid::new_v4(), Uuid::new_v4());
        model.worker_event(
            t0,
            WorkerToShell::TaskStarted { request_id: old, provider: "codex".into(), prompt: "vieja".into() },
        );
        model.worker_event(
            t0 + secs(120),
            WorkerToShell::TaskStarted { request_id: recent, provider: "codex".into(), prompt: "nueva".into() },
        );

        let running = model.tray(t0 + secs(180)).running;
        assert_eq!(
            running.iter().map(|t| (t.prompt.as_str(), t.age_secs)).collect::<Vec<_>>(),
            vec![("vieja", 180), ("nueva", 60)]
        );
    }

    #[test]
    fn a_finished_task_announces_itself_and_refreshes_the_list() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let task = Uuid::new_v4();
        model.worker_event(
            t0,
            WorkerToShell::TaskStarted { request_id: task, provider: "codex".into(), prompt: "x".into() },
        );

        let commands = model.worker_event(
            t0,
            WorkerToShell::TaskFinished { request_id: task, success: true, summary: "3 archivos".into() },
        );
        assert!(matches!(commands.as_slice(), [Command::ListTasks(_)]));
        assert_eq!(text(&model).as_deref(), Some("✓ Tarea lista: 3 archivos"));
        assert!(model.tray(t0).running.is_empty());

        model.worker_event(t0, state(task, WorkerState::Done(true)));
        assert_eq!(
            text(&model).as_deref(),
            Some("✓ Tarea lista: 3 archivos"),
            "the trailing Done must not replace the summary"
        );
    }

    #[test]
    fn a_failed_task_says_so_in_red_and_a_cancelled_one_is_neutral() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(
            t0,
            WorkerToShell::TaskFinished { request_id: Uuid::new_v4(), success: false, summary: "se cayó".into() },
        );
        let overlay = model.overlay().unwrap();
        assert_eq!((overlay.text.as_str(), overlay.tone), ("✗ La tarea falló: se cayó", Tone::Error));

        model.worker_event(
            t0,
            WorkerToShell::TaskFinished { request_id: Uuid::new_v4(), success: false, summary: "cancelada".into() },
        );
        assert_eq!(model.overlay().unwrap().tone, Tone::Neutral);
    }

    #[test]
    fn the_task_list_keeps_the_recent_finished_ones_for_the_menu() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let task = |state| TaskInfo {
            request_id: Uuid::new_v4(),
            provider: "codex".into(),
            prompt: "p".into(),
            state,
            summary: None,
            age_secs: 1,
        };
        model.worker_event(
            t0,
            WorkerToShell::TaskList {
                request_id: Uuid::new_v4(),
                tasks: vec![task(TaskState::Running), task(TaskState::Succeeded), task(TaskState::Failed)],
            },
        );
        assert_eq!(model.tray(t0).recent.len(), 2, "the running one comes from the live view, not the list");
    }

    // ---- confirmations ----

    fn ask(model: &mut ShellModel, now: Instant) -> Uuid {
        let id = Uuid::new_v4();
        model.worker_event(
            now,
            WorkerToShell::ConfirmationRequested {
                confirmation_id: id,
                title: "Cerrar la aplicación Spotify".into(),
                detail: "Lo pide un agente".into(),
                timeout_secs: 30,
            },
        );
        id
    }

    #[test]
    fn a_confirmation_shows_the_question_and_the_keys_and_outranks_everything() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.press(t0, Uuid::new_v4());
        ask(&mut model, t0);

        let overlay = model.overlay().unwrap();
        assert_eq!(overlay.tone, Tone::Ask);
        assert_eq!(overlay.text, "Cerrar la aplicación Spotify\nLo pide un agente\n⌘⏎ sí  ·  ⌘⎋ no");
        assert!(model.wants_confirmation_keys());
    }

    #[test]
    fn the_yes_key_approves_and_the_no_key_refuses() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = ask(&mut model, t0);
        assert_eq!(model.confirm_key(), vec![Command::Confirm { id, approved: true }]);
        assert!(!model.wants_confirmation_keys());

        let id = ask(&mut model, t0);
        assert_eq!(model.cancel_key(), vec![Command::Confirm { id, approved: false }]);
    }

    #[test]
    fn the_confirmation_keys_do_nothing_when_nothing_is_asked() {
        let mut model = ready_model(Instant::now());
        assert_eq!(model.confirm_key(), Vec::new());
        assert_eq!(model.cancel_key(), Vec::new());
    }

    #[test]
    fn a_prompt_the_worker_closed_or_that_timed_out_disappears() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = ask(&mut model, t0);
        model.worker_event(t0, WorkerToShell::ConfirmationClosed { confirmation_id: id });
        assert_eq!(model.overlay(), None);

        ask(&mut model, t0);
        model.tick(t0 + secs(31));
        assert_eq!(model.overlay(), None, "even if the close event never arrives");
    }

    #[test]
    fn a_close_for_a_different_question_leaves_the_current_one() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        ask(&mut model, t0);
        model.worker_event(t0, WorkerToShell::ConfirmationClosed { confirmation_id: Uuid::new_v4() });
        assert!(model.overlay().is_some());
    }

    #[test]
    fn dictation_is_blocked_while_a_question_is_waiting() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        ask(&mut model, t0);
        assert_eq!(model.press(t0, Uuid::new_v4()), Vec::new());
    }

    // ---- cancelling ----

    #[test]
    fn the_no_key_abandons_a_recording_in_progress() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        assert!(model.wants_cancel_key());

        assert_eq!(model.cancel_key(), vec![Command::Cancel(id)]);
        assert_eq!(model.overlay(), None);
        assert_eq!(model.release(t0), Vec::new(), "the release that follows has nothing to stop");
        assert!(!model.wants_cancel_key(), "the shortcut is released the moment it is not needed");
    }

    // ---- the worker dying and coming back ----

    #[test]
    fn a_dead_worker_shows_restarting_and_drops_everything_it_was_doing() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.press(t0, Uuid::new_v4());
        ask(&mut model, t0);
        model.worker_event(
            t0,
            WorkerToShell::TaskStarted { request_id: Uuid::new_v4(), provider: "codex".into(), prompt: "x".into() },
        );

        model.worker_restarting();

        assert_eq!(text(&model).as_deref(), Some("↻ Reiniciando EVA…"));
        assert!(!model.wants_confirmation_keys(), "the question died with the worker");
        assert!(model.tray(t0).running.is_empty());
        assert_eq!(model.tray(t0).icon, TrayIcon::Attention);
    }

    #[test]
    fn a_restarted_worker_that_says_ready_clears_the_overlay_and_asks_for_the_tasks() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_restarting();

        let commands = model.worker_event(t0 + secs(3), WorkerToShell::Ready);

        assert!(matches!(commands.as_slice(), [Command::ListTasks(_)]));
        assert_eq!(model.overlay(), None);
        assert_eq!(model.tray(t0).status, "Listo");
    }

    #[test]
    fn pressing_before_the_worker_is_ready_explains_instead_of_recording() {
        let t0 = Instant::now();
        let mut model = ShellModel::new(keys());
        assert_eq!(model.press(t0, Uuid::new_v4()), Vec::new());
        assert_eq!(text(&model).as_deref(), Some("EVA se está iniciando…"));
        assert_eq!(model.overlay().unwrap().tone, Tone::Neutral, "waiting for startup is not an error");
    }

    #[test]
    fn pressing_while_the_worker_restarts_says_so() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_restarting();
        model.press(t0, Uuid::new_v4());
        // The restart banner outranks the notice.
        assert_eq!(text(&model).as_deref(), Some("↻ Reiniciando EVA…"));
    }

    #[test]
    fn short_flattens_whitespace_and_truncates_with_an_ellipsis() {
        assert_eq!(short("a  b\nc", 20), "a b c");
        assert_eq!(short("abcdefghij", 5), "abcd…");
        assert_eq!(short("", 5), "");
    }

    proptest::proptest! {
        #[test]
        fn short_never_exceeds_the_limit_or_panics(text in ".*", max in 1usize..120) {
            proptest::prop_assert!(short(&text, max).chars().count() <= max);
        }
    }
}
