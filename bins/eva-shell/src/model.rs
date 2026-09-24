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
use eva_macos::{Activity, Choice, Icon, OverlayContent, Tone};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// A recording the key never released (a lost key-up) is stopped after this.
pub const LISTENING_LIMIT: Duration = Duration::from_secs(120);
/// Transcription and formatting of a short dictation take a couple of
/// seconds; past this the request is stuck (`docs/PLAN.md` §3.3 point 4's
/// watchdog). A long one takes longer — measured, a 70 s dictation took
/// ~15 s — so every second recorded adds [`THINKING_PER_RECORDED`] to it:
/// a fixed 45 s could cancel a two-minute dictation on a slower Mac and lose it.
pub const THINKING_LIMIT: Duration = Duration::from_secs(45);
/// Extra time to think, per second recorded (half a second each).
const THINKING_PER_RECORDED: f64 = 0.5;
/// A quick action (open an app, paste) that has not finished by now never will.
pub const EXECUTING_LIMIT: Duration = Duration::from_secs(90);
/// How often a ready worker is asked whether its command loop still answers.
pub const PING_EVERY: Duration = Duration::from_secs(15);
/// How long an answer may take before the worker counts as stuck. A process
/// that is alive but whose loop never reads again (a blocking system call
/// that never returns) is invisible to the supervisor, which only notices
/// exits; this is what notices it.
pub const PONG_WITHIN: Duration = Duration::from_secs(10);
const OK_NOTICE: Duration = Duration::from_millis(1_400);
const ERROR_NOTICE: Duration = Duration::from_millis(4_500);
/// "Copiado · pégalo con ⌘V" stays long enough to move to where it goes.
const CLIPBOARD_NOTICE: Duration = Duration::from_millis(4_000);
/// "Ejecutando en Claude Code" stays this long: the task itself runs on.
const TASK_START_NOTICE: Duration = Duration::from_millis(2_500);
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
    /// Flag the last dictation as wrong, keeping it for the eval corpus.
    FlagLastDictation(Uuid),
    /// Ask the worker how it is doing, to tell the user what is missing.
    CheckHealth(Uuid),
    /// Check that the worker's command loop still answers.
    Ping(Uuid),
    /// The worker stopped answering: kill it, so the supervisor starts a new one.
    RestartWorker,
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
    /// How long the recording behind this request lasted (zero for a request
    /// that did not start as a recording).
    recorded: Duration,
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
    icon: Icon,
    until: Instant,
}

/// What a command is doing, in the words and picture of the island: "Abriendo
/// Spotify" with its icon while it runs, "Spotify abierto" when it is done.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Doing {
    now: String,
    done: String,
    icon: Icon,
}

impl Doing {
    /// What the command `intent_json` (an `eva_intent::Intent`, as sent by the
    /// worker) does, if it is something worth saying: dictation, questions and
    /// the like are not.
    fn from_intent(intent_json: &serde_json::Value) -> Option<Doing> {
        let text = |key: &str| intent_json.get(key).and_then(serde_json::Value::as_str).map(str::to_string);
        let doing = |now: String, done: String, icon: Icon| Some(Doing { now, done, icon });
        match intent_json.get("kind")?.as_str()? {
            "open_app" => {
                let app = text("app")?;
                doing(format!("Abriendo {app}"), format!("{app} abierto"), Icon::App(app))
            }
            "close_app" => {
                let app = text("app")?;
                doing(format!("Cerrando {app}"), format!("{app} cerrado"), Icon::App(app))
            }
            "open_url" => {
                let site = site_of(&text("url")?);
                doing(format!("Abriendo {site}"), format!("{site} abierto"), Icon::Symbol("globe"))
            }
            "web_search" => {
                let query = short(&text("query")?, 32);
                doing(format!("Buscando {query}"), "Búsqueda lista".to_string(), Icon::Symbol("magnifyingglass"))
            }
            "custom" => {
                let phrase = short(&text("phrase")?, 32);
                doing(format!("Ejecutando {phrase}"), format!("{phrase} listo"), Icon::Symbol("bolt.fill"))
            }
            "edit_selection" => {
                doing("Reescribiendo".to_string(), "Texto reescrito".to_string(), Icon::Symbol("pencil"))
            }
            "agent_task" | "continue_agent_task" => {
                let now = match text("provider") {
                    Some(provider) => format!("Ejecutando en {}", provider_name(&provider)),
                    None => "Enviando al agente".to_string(),
                };
                doing(now, "Tarea enviada".to_string(), Icon::Symbol("sparkles"))
            }
            "dictation" => doing("Escribiendo".to_string(), "Listo".to_string(), Icon::Symbol("text.cursor")),
            _ => None,
        }
    }
}

/// An agent's id as the person knows it.
fn provider_name(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude_code" | "claude" => "Claude Code",
        other => other,
    }
}

/// The part of `url` worth reading in an island: its host, without `www.`.
fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    short(host.strip_prefix("www.").unwrap_or(host), 32)
}

/// The shell's state.
pub struct ShellModel {
    keys: KeyLabels,
    ready: bool,
    restarting: bool,
    recording: Option<Uuid>,
    requests: HashMap<Uuid, Tracked>,
    /// What each command that is running is doing, by request.
    doing: HashMap<Uuid, Doing>,
    tasks: HashMap<Uuid, (TaskInfo, Instant)>,
    recent: Vec<TaskInfo>,
    confirmation: Option<Confirmation>,
    notice: Option<Notice>,
    /// A command went well and the island asks "¿Algo más?" until this.
    follow_up: Option<Instant>,
    notifications: Vec<Notification>,
    health_asked: bool,
    /// When the last ping went out, and the one still waiting for its pong.
    last_ping: Option<Instant>,
    unanswered_ping: Option<(Uuid, Instant)>,
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
            doing: HashMap::new(),
            tasks: HashMap::new(),
            recent: Vec::new(),
            confirmation: None,
            notice: None,
            follow_up: None,
            notifications: Vec::new(),
            health_asked: false,
            last_ping: None,
            unanswered_ping: None,
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
        self.follow_up = None;
        self.recording = Some(request_id);
        self.track(request_id, Phase::Listening, now);
        vec![Command::StartRecording(request_id)]
    }

    /// The dictation key came up. Stops the recording, if one is running.
    pub fn release(&mut self, now: Instant) -> Vec<Command> {
        let Some(id) = self.recording.take() else { return Vec::new() };
        self.track(id, Phase::Thinking, now);
        vec![Command::StopRecording(id)]
    }

    /// The "no" key: answers a pending confirmation with no, or abandons the
    /// recording in progress.
    pub fn cancel_key(&mut self) -> Vec<Command> {
        if let Some(confirmation) = self.confirmation.take() {
            return vec![Command::Confirm { id: confirmation.id, approved: false }];
        }
        self.abandon_recording()
    }

    /// Drops the recording in progress without a word — the dictation key
    /// turned out to be half of a shortcut (fn+Delete, fn+arrow), or the
    /// user pressed "no".
    pub fn abandon_recording(&mut self) -> Vec<Command> {
        match self.recording.take() {
            Some(id) => {
                self.requests.remove(&id);
                vec![Command::Cancel(id)]
            }
            None => Vec::new(),
        }
    }

    /// The "yes" key: answers a pending confirmation with yes.
    pub fn confirm_key(&mut self) -> Vec<Command> {
        match self.confirmation.take() {
            Some(confirmation) => vec![Command::Confirm { id: confirmation.id, approved: true }],
            None => Vec::new(),
        }
    }

    /// A click on button `index` of the question on the island: the same
    /// answer as its key (`0` is yes, `1` is no).
    pub fn choose(&mut self, index: usize) -> Vec<Command> {
        match index {
            0 => self.confirm_key(),
            1 => self.cancel_key(),
            _ => Vec::new(),
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
        self.last_ping = None;
        self.unanswered_ping = None;
    }

    /// An event from the worker. Returns anything to send back.
    pub fn worker_event(&mut self, now: Instant, event: WorkerToShell) -> Vec<Command> {
        match event {
            WorkerToShell::Ready => {
                self.ready = true;
                self.restarting = false;
                self.last_ping = Some(now);
                let mut commands = vec![Command::ListTasks(Uuid::new_v4())];
                // Once per run: what is missing (no voice model, a typo in
                // the config) is said now, not discovered at the first dictation.
                if !std::mem::replace(&mut self.health_asked, true) {
                    commands.push(Command::CheckHealth(Uuid::new_v4()));
                }
                return commands;
            }
            WorkerToShell::Health { report, .. } => {
                let problems = startup_problems(&report);
                if !problems.is_empty() {
                    self.notifications.push(Notification { title: "EVA01".to_string(), body: problems.join("\n") });
                }
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
                let text = format!("Ejecutando en {}", provider_name(&provider));
                self.notice = Some(Notice {
                    text,
                    tone: Tone::Neutral,
                    icon: Icon::Symbol("sparkles"),
                    until: now + TASK_START_NOTICE,
                });
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
            WorkerToShell::Pong { request_id } => {
                if self.unanswered_ping.is_some_and(|(id, _)| id == request_id) {
                    self.unanswered_ping = None;
                }
            }
            WorkerToShell::Notice { message, .. } => {
                self.notice = Some(Notice {
                    text: message,
                    tone: Tone::Ok,
                    icon: Icon::Symbol("doc.on.clipboard"),
                    until: now + CLIPBOARD_NOTICE,
                });
            }
            WorkerToShell::FollowUp { secs, .. } => {
                // The result ("Spotify abierto") is shown first; the question
                // gets its full time after it.
                self.follow_up = Some(now + OK_NOTICE + Duration::from_secs(secs));
            }
            WorkerToShell::DictationFlagged { message, .. } => {
                self.set_notice(now, &format!("✓ {}", short(&message, 90)), Tone::Ok, TASK_RESULT_NOTICE);
            }
            // Informational for the overlay: the worker pastes text itself,
            // and these are for logs and the CLI.
            WorkerToShell::IntentRecognized { request_id, intent_json } => {
                if let Some(doing) = Doing::from_intent(&intent_json) {
                    self.doing.insert(request_id, doing);
                }
            }
            WorkerToShell::StateChanged { request_id: None, .. }
            | WorkerToShell::Transcript { .. }
            | WorkerToShell::AgentEvent { .. }
            | WorkerToShell::CustomWords { .. }
            | WorkerToShell::Ack { .. } => {}
        }
        Vec::new()
    }

    /// Pings a ready worker every [`PING_EVERY`]; one that has not answered
    /// within [`PONG_WITHIN`] is stuck, and gets restarted.
    fn heartbeat(&mut self, now: Instant) -> Vec<Command> {
        if !self.ready {
            return Vec::new();
        }
        if let Some((_, sent)) = self.unanswered_ping {
            if now.duration_since(sent) < PONG_WITHIN {
                return Vec::new();
            }
            tracing::warn!("eva-worker no respondió al latido; se reinicia");
            self.worker_restarting();
            self.notifications.push(Notification {
                title: "EVA01".to_string(),
                body: "EVA dejó de responder y la reinicié. Si estabas dictando, vuelve a intentarlo.".to_string(),
            });
            return vec![Command::RestartWorker];
        }
        if self.last_ping.is_none_or(|last| now.duration_since(last) >= PING_EVERY) {
            let id = Uuid::new_v4();
            self.last_ping = Some(now);
            self.unanswered_ping = Some((id, now));
            return vec![Command::Ping(id)];
        }
        Vec::new()
    }

    /// Moves `id` into `phase` as of `now`. Leaving the recording notes how
    /// long it lasted, which the rest of the request keeps.
    fn track(&mut self, id: Uuid, phase: Phase, now: Instant) {
        let recorded = match self.requests.get(&id) {
            Some(t) if t.phase == Phase::Listening && phase != Phase::Listening => now.duration_since(t.since),
            Some(t) => t.recorded,
            None => Duration::ZERO,
        };
        self.requests.insert(id, Tracked { phase, since: now, recorded });
    }

    fn state_changed(&mut self, now: Instant, id: Uuid, state: WorkerState) {
        if self.tasks.contains_key(&id) {
            return; // a background task: shown in the tray, not the overlay
        }
        match state {
            WorkerState::Listening => self.track(id, Phase::Listening, now),
            WorkerState::Thinking => self.track(id, Phase::Thinking, now),
            WorkerState::Executing => self.track(id, Phase::Executing, now),
            WorkerState::Idle => self.forget(id),
            WorkerState::Done(success) => {
                let doing = self.doing.remove(&id);
                self.forget(id);
                // Never over a message that already says more: an `Error`
                // (or a task's summary) arrives just before its `Done`.
                if self.notice.is_none() {
                    if let (true, Some(doing)) = (success, doing) {
                        self.notice =
                            Some(Notice { text: doing.done, tone: Tone::Ok, icon: doing.icon, until: now + OK_NOTICE });
                    } else if success {
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
        self.doing.remove(&id);
        if self.recording == Some(id) {
            self.recording = None;
        }
    }

    // ---- time ----

    /// Advances time: expires notices and prompts, and fires the watchdog on
    /// anything stuck. Returns what to send the worker.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        if self.follow_up.is_some_and(|until| now >= until) {
            self.follow_up = None;
        }
        if self.notice.as_ref().is_some_and(|n| now >= n.until) {
            self.notice = None;
        }
        if self.confirmation.as_ref().is_some_and(|c| now >= c.deadline) {
            self.confirmation = None;
        }

        let mut commands = self.heartbeat(now);
        let stuck: Vec<(Uuid, Phase)> = self
            .requests
            .iter()
            .filter(|(_, t)| now.duration_since(t.since) >= limit_of(t))
            .map(|(id, t)| (*id, t.phase))
            .collect();

        for (id, phase) in stuck {
            match phase {
                Phase::Listening => {
                    // The key-up never arrived. Stop as if it had, so what
                    // was said is not lost.
                    self.recording = None;
                    self.track(id, Phase::Thinking, now);
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
        self.notice = Some(Notice { text: text.to_string(), tone, icon: Icon::None, until: now + lasts });
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
        let working = |text: &str, activity| {
            Some(OverlayContent {
                text: text.to_string(),
                tone: Tone::Neutral,
                activity,
                icon: Icon::None,
                choices: Vec::new(),
            })
        };

        if self.restarting {
            return working("Reiniciando EVA", Activity::Thinking);
        }
        if let Some(confirmation) = &self.confirmation {
            let text = format!("{}\n{}", short(&confirmation.title, 70), short(&confirmation.detail, 70));
            let choice = |label: &str, shortcut: &str, primary| Choice {
                label: label.to_string(),
                shortcut: shortcut.to_string(),
                primary,
            };
            return Some(OverlayContent {
                text,
                tone: Tone::Ask,
                activity: Activity::None,
                icon: Icon::None,
                choices: vec![choice("Sí", &self.keys.confirm, true), choice("No", &self.keys.cancel, false)],
            });
        }
        if self.requests.values().any(|t| t.phase == Phase::Listening) {
            return working("Escuchando", Activity::Listening);
        }
        if let Some((id, _)) = self.requests.iter().find(|(_, t)| t.phase == Phase::Thinking) {
            // Once the words are understood, say what they turned out to be.
            return match self.doing.get(id) {
                Some(doing) => Some(OverlayContent {
                    text: doing.now.clone(),
                    tone: Tone::Neutral,
                    activity: Activity::Thinking,
                    icon: doing.icon.clone(),
                    choices: Vec::new(),
                }),
                None => working("Pensando", Activity::Thinking),
            };
        }
        if let Some((id, _)) = self.requests.iter().find(|(_, t)| t.phase == Phase::Executing) {
            return match self.doing.get(id) {
                Some(doing) => Some(OverlayContent {
                    text: doing.now.clone(),
                    tone: Tone::Neutral,
                    activity: Activity::Executing,
                    icon: doing.icon.clone(),
                    choices: Vec::new(),
                }),
                None => working("Ejecutando", Activity::Executing),
            };
        }
        if self.notice.is_none() && self.follow_up.is_some() {
            return Some(OverlayContent {
                text: "¿Algo más?".to_string(),
                tone: Tone::Neutral,
                activity: Activity::None,
                icon: Icon::Symbol("mic"),
                choices: Vec::new(),
            });
        }
        self.notice.as_ref().map(|n| OverlayContent {
            text: n.text.clone(),
            tone: n.tone,
            activity: Activity::None,
            icon: n.icon.clone(),
            choices: Vec::new(),
        })
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

fn limit_of(tracked: &Tracked) -> Duration {
    match tracked.phase {
        Phase::Listening => LISTENING_LIMIT,
        Phase::Thinking => THINKING_LIMIT + tracked.recorded.mul_f64(THINKING_PER_RECORDED),
        Phase::Executing => EXECUTING_LIMIT,
    }
}

/// What is wrong with the worker's setup, in words for the user — empty when
/// all is well.
fn startup_problems(report: &eva_ipc::HealthReport) -> Vec<String> {
    let mut problems = Vec::new();
    if !report.stt_model_loaded {
        problems.push(
            "Falta el modelo de voz, así que no puedo dictar. Instálalo con «eva model install» \
             (el comando está en EVA01.app/Contents/MacOS)."
                .to_string(),
        );
    }
    if !report.store_ok {
        problems.push("La base de datos de EVA01 no abrió: no se guardará el historial.".to_string());
    }
    problems.extend(report.config_warnings.iter().map(|w| format!("Configuración: {w}")));
    problems
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

    /// `model.tick(now)`, with every ping answered at once, as a healthy
    /// worker does — for the tests that are about something else.
    fn tick(model: &mut ShellModel, now: Instant) -> Vec<Command> {
        let mut commands = model.tick(now);
        commands.retain(|command| match command {
            Command::Ping(id) => {
                model.worker_event(now, WorkerToShell::Pong { request_id: *id });
                false
            }
            _ => true,
        });
        commands
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
        assert_eq!(text(&model).as_deref(), Some("Escuchando"));
        assert_eq!(model.tray(t0).icon, TrayIcon::Listening);
    }

    #[test]
    fn releasing_stops_the_recording_and_shows_thinking_until_the_worker_finishes() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);

        assert_eq!(model.release(t0 + secs(2)), vec![Command::StopRecording(id)]);
        assert_eq!(text(&model).as_deref(), Some("Pensando"));

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

        tick(&mut model, t0 + Duration::from_millis(500));
        assert!(model.overlay().is_some());
        tick(&mut model, t0 + secs(2));
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

        assert_eq!(tick(&mut model, t0 + secs(30)), Vec::new(), "still within the limit");
        assert_eq!(tick(&mut model, t0 + THINKING_LIMIT), vec![Command::Cancel(id)]);

        assert_eq!(text(&model).as_deref(), Some("✗ EVA se quedó trabada; la detuve"));
        assert_eq!(model.take_notifications().len(), 1, "the user must be told, not left guessing");
        tick(&mut model, t0 + THINKING_LIMIT + secs(10));
        assert_eq!(model.overlay(), None, "and then it returns to idle");
    }

    #[test]
    fn a_recording_whose_key_up_was_lost_is_stopped_after_the_limit() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);

        assert_eq!(tick(&mut model, t0 + LISTENING_LIMIT), vec![Command::StopRecording(id)]);
        assert_eq!(text(&model).as_deref(), Some("Pensando"), "what was said is still processed");
        assert_eq!(model.release(t0 + LISTENING_LIMIT + secs(1)), Vec::new(), "the late key-up has nothing left to do");
    }

    fn ping_in(commands: &[Command]) -> Option<Uuid> {
        commands.iter().find_map(|c| match c {
            Command::Ping(id) => Some(*id),
            _ => None,
        })
    }

    #[test]
    fn a_ready_worker_is_pinged_regularly_and_an_answer_keeps_it() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        assert_eq!(ping_in(&model.tick(t0 + secs(1))), None, "not right after it said it is ready");

        let ping = ping_in(&model.tick(t0 + PING_EVERY)).expect("a ping is due");
        model.worker_event(t0 + PING_EVERY + secs(1), WorkerToShell::Pong { request_id: ping });

        let later = t0 + PING_EVERY + PONG_WITHIN + secs(1);
        assert!(!model.tick(later).contains(&Command::RestartWorker), "it answered");
        assert!(ping_in(&model.tick(t0 + PING_EVERY * 2)).is_some(), "and is asked again later");
    }

    #[test]
    fn a_worker_that_stops_answering_is_restarted_and_the_user_is_told() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0 + PING_EVERY, id);
        assert!(ping_in(&model.tick(t0 + PING_EVERY)).is_some());

        let commands = model.tick(t0 + PING_EVERY + PONG_WITHIN);

        assert!(commands.contains(&Command::RestartWorker), "{commands:?}");
        assert_eq!(model.take_notifications().len(), 1);
        assert_eq!(text(&model).as_deref(), Some("Reiniciando EVA"));
        assert!(!model.wants_cancel_key(), "the recording died with it");
        assert!(model.tick(t0 + PING_EVERY * 3).is_empty(), "nothing more until the new worker is ready");
    }

    #[test]
    fn a_late_or_stray_pong_changes_nothing() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let ping = ping_in(&model.tick(t0 + PING_EVERY)).expect("ping");
        model.worker_event(t0 + PING_EVERY, WorkerToShell::Pong { request_id: Uuid::new_v4() });
        assert!(
            model.tick(t0 + PING_EVERY + PONG_WITHIN).contains(&Command::RestartWorker),
            "a stray pong is not an answer"
        );
        model.worker_event(t0 + PING_EVERY + PONG_WITHIN, WorkerToShell::Pong { request_id: ping });
        assert!(model.tick(t0 + PING_EVERY * 4).is_empty());
    }

    #[test]
    fn a_shortcut_with_the_dictation_key_abandons_the_recording_quietly() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);

        assert_eq!(model.abandon_recording(), vec![Command::Cancel(id)]);
        assert_eq!(model.overlay(), None, "nothing to say: it was fn+Delete");
        assert_eq!(model.release(t0 + secs(1)), Vec::new(), "the key-up afterwards has nothing to stop");
        assert_eq!(model.abandon_recording(), Vec::new());
    }

    #[test]
    fn a_long_dictation_gets_longer_to_be_transcribed_before_the_watchdog_gives_up() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        let released = t0 + secs(100);
        model.release(released);
        // The worker confirms the phase: the recorded time must survive it.
        model.worker_event(released, state(id, WorkerState::Thinking));

        assert_eq!(
            tick(&mut model, released + THINKING_LIMIT + secs(1)),
            Vec::new(),
            "100 s of speech needs more than 45 s"
        );
        assert_eq!(tick(&mut model, released + THINKING_LIMIT + secs(51)), vec![Command::Cancel(id)], "45 s + 50 s");
    }

    #[test]
    fn a_recording_stopped_by_the_watchdog_also_gets_the_longer_limit() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.press(t0, id);
        tick(&mut model, t0 + LISTENING_LIMIT);

        assert_eq!(tick(&mut model, t0 + LISTENING_LIMIT + THINKING_LIMIT + secs(30)), Vec::new());
        assert_eq!(
            tick(&mut model, t0 + LISTENING_LIMIT + THINKING_LIMIT + secs(61)),
            vec![Command::Cancel(id)],
            "45 s + half of the 120 s recorded"
        );
    }

    #[test]
    fn a_stuck_quick_action_is_also_caught() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Executing));
        assert_eq!(tick(&mut model, t0 + EXECUTING_LIMIT), vec![Command::Cancel(id)]);
    }

    #[test]
    fn each_state_change_restarts_that_requests_clock() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Thinking));
        model.worker_event(t0 + secs(40), state(id, WorkerState::Executing));
        assert_eq!(
            tick(&mut model, t0 + secs(50)),
            Vec::new(),
            "progress was made at 40s; 45s from the start is not stuck"
        );
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

        assert_eq!(tick(&mut model, t0 + secs(3_600)), Vec::new(), "an agent may legitimately run for an hour");
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
        assert_eq!(text(&model).as_deref(), Some("Ejecutando en Codex"), "it says where the task went…");
        tick(&mut model, t0 + TASK_START_NOTICE + secs(1));
        assert_eq!(model.overlay(), None, "…and then a background task alone shows nothing on the overlay");
        let t0 = t0 + TASK_START_NOTICE + secs(1);
        assert_eq!(model.tray(t0).icon, TrayIcon::Busy);
        assert_eq!(model.tray(t0).status, "1 tarea(s) en curso");

        let dictation = Uuid::new_v4();
        model.press(t0, dictation);
        assert_eq!(text(&model).as_deref(), Some("Escuchando"));
        model.release(t0);
        model.worker_event(t0, state(dictation, WorkerState::Done(true)));
        tick(&mut model, t0 + secs(5));
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

    fn health(model_loaded: bool, warnings: &[&str]) -> WorkerToShell {
        WorkerToShell::Health {
            request_id: Uuid::new_v4(),
            report: eva_ipc::HealthReport {
                stt_model_loaded: model_loaded,
                stt_model_id: model_loaded.then(|| "canary".to_string()),
                store_ok: true,
                agents: Vec::new(),
                formatter: "reglas".into(),
                config_warnings: warnings.iter().map(|w| w.to_string()).collect(),
                gateway_socket: None,
                project_count: 0,
            },
        }
    }

    #[test]
    fn the_health_of_the_worker_is_asked_once_not_after_every_restart() {
        let t0 = Instant::now();
        let mut model = ShellModel::new(keys());
        let first = model.worker_event(t0, WorkerToShell::Ready);
        assert!(first.iter().any(|c| matches!(c, Command::CheckHealth(_))));

        model.worker_restarting();
        let after_restart = model.worker_event(t0, WorkerToShell::Ready);
        assert!(!after_restart.iter().any(|c| matches!(c, Command::CheckHealth(_))));
    }

    #[test]
    fn a_missing_voice_model_and_config_typos_are_announced_at_startup() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(t0, health(false, &["hotkey.dictaton: clave desconocida"]));

        let notifications = model.take_notifications();
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].body.contains("eva model install"), "{}", notifications[0].body);
        assert!(notifications[0].body.contains("hotkey.dictaton"), "{}", notifications[0].body);
    }

    #[test]
    fn a_healthy_worker_says_nothing() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(t0, health(true, &[]));
        assert!(model.take_notifications().is_empty());
    }

    #[test]
    fn flagging_a_dictation_confirms_in_green_and_says_where_it_went() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(
            t0,
            WorkerToShell::DictationFlagged { request_id: Uuid::new_v4(), message: "Guardado para el corpus".into() },
        );
        let overlay = model.overlay().unwrap();
        assert_eq!((overlay.text.as_str(), overlay.tone), ("✓ Guardado para el corpus", Tone::Ok));
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
        assert_eq!(overlay.text, "Cerrar la aplicación Spotify\nLo pide un agente");
        let buttons: Vec<_> =
            overlay.choices.iter().map(|c| (c.label.as_str(), c.shortcut.as_str(), c.primary)).collect();
        assert_eq!(buttons, vec![("Sí", "⌘⏎", true), ("No", "⌘⎋", false)], "each button shows its shortcut");
        assert!(model.wants_confirmation_keys());
    }

    #[test]
    fn clicking_a_button_answers_like_its_key() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        let id = ask(&mut model, t0);
        assert_eq!(model.choose(0), vec![Command::Confirm { id, approved: true }]);

        let id = ask(&mut model, t0);
        assert_eq!(model.choose(1), vec![Command::Confirm { id, approved: false }]);
        assert_eq!(model.choose(0), Vec::new(), "nothing left to answer");
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
        tick(&mut model, t0 + secs(31));
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

        assert_eq!(text(&model).as_deref(), Some("Reiniciando EVA"));
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
        assert_eq!(text(&model).as_deref(), Some("Reiniciando EVA"));
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

    #[test]
    fn a_command_that_opens_an_app_says_so_with_the_apps_icon_and_then_that_it_is_open() {
        let mut model = ShellModel::new(keys());
        let t0 = Instant::now();
        let id = Uuid::new_v4();
        model.worker_event(
            t0,
            WorkerToShell::IntentRecognized {
                request_id: id,
                intent_json: serde_json::json!({ "kind": "open_app", "app": "Spotify" }),
            },
        );
        model.worker_event(t0, state(id, WorkerState::Executing));

        let running = model.overlay().unwrap();
        assert_eq!(
            (running.text.as_str(), running.activity, running.icon),
            ("Abriendo Spotify", Activity::Executing, Icon::App("Spotify".to_string()))
        );

        model.worker_event(t0, state(id, WorkerState::Done(true)));
        let done = model.overlay().unwrap();
        assert_eq!(
            (done.text.as_str(), done.tone, done.icon),
            ("Spotify abierto", Tone::Ok, Icon::App("Spotify".to_string()))
        );
    }

    #[test]
    fn what_each_kind_of_command_says_it_is_doing() {
        let says = |json: serde_json::Value| Doing::from_intent(&json).map(|d| (d.now, d.icon));
        assert_eq!(
            says(serde_json::json!({ "kind": "open_url", "url": "https://www.github.com/foo?x=1" })),
            Some(("Abriendo github.com".to_string(), Icon::Symbol("globe")))
        );
        assert_eq!(
            says(serde_json::json!({ "kind": "close_app", "app": "Notes" })),
            Some(("Cerrando Notes".to_string(), Icon::App("Notes".to_string())))
        );
        assert_eq!(says(serde_json::json!({ "kind": "hazlo_raro" })), None);
        assert_eq!(says(serde_json::json!({ "kind": "confirm_app", "app": "Spotify", "heard": "spotifi" })), None);
    }

    #[test]
    fn a_command_with_nothing_to_say_still_shows_the_plain_executing_and_listo() {
        let mut model = ShellModel::new(keys());
        let t0 = Instant::now();
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Executing));
        assert_eq!(model.overlay().unwrap().text, "Ejecutando");
        model.worker_event(t0, state(id, WorkerState::Done(true)));
        assert_eq!(model.overlay().unwrap().text, "✓ Listo");
    }

    #[test]
    fn a_dictation_says_escribiendo_and_an_agent_task_says_which_agent() {
        let mut model = ShellModel::new(keys());
        let t0 = Instant::now();
        let id = Uuid::new_v4();
        model.worker_event(t0, state(id, WorkerState::Thinking));
        assert_eq!(model.overlay().unwrap().text, "Pensando", "before it is understood");

        model.worker_event(
            t0,
            WorkerToShell::IntentRecognized { request_id: id, intent_json: serde_json::json!({ "kind": "dictation" }) },
        );
        let typing = model.overlay().unwrap();
        assert_eq!((typing.text.as_str(), typing.icon), ("Escribiendo", Icon::Symbol("text.cursor")));

        model.worker_event(t0, state(id, WorkerState::Done(true)));
        let task = Uuid::new_v4();
        model.worker_event(
            t0,
            WorkerToShell::TaskStarted { request_id: task, provider: "claude_code".into(), prompt: "x".into() },
        );
        assert_eq!(model.overlay().unwrap().text, "Ejecutando en Claude Code");
        assert_eq!(
            Doing::from_intent(&serde_json::json!({ "kind": "agent_task", "prompt": "x", "provider": "codex" }))
                .unwrap()
                .now,
            "Ejecutando en Codex"
        );
    }

    #[test]
    fn a_dictation_copied_for_lack_of_a_place_to_paste_says_so_in_green_with_the_clipboard() {
        let t0 = Instant::now();
        let mut model = ShellModel::new(keys());
        let id = Uuid::new_v4();
        model
            .worker_event(t0, WorkerToShell::Notice { request_id: id, message: "Copiado · pégalo con ⌘V".to_string() });
        model.worker_event(t0, state(id, WorkerState::Done(true)));

        let overlay = model.overlay().unwrap();
        assert_eq!(
            (overlay.text.as_str(), overlay.tone, overlay.icon),
            ("Copiado · pégalo con ⌘V", Tone::Ok, Icon::Symbol("doc.on.clipboard")),
            "the Done that follows must not replace it with a bare ✓ Listo"
        );
    }

    #[test]
    fn after_a_command_the_island_asks_for_something_more_and_stops_asking() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(t0, WorkerToShell::FollowUp { request_id: Uuid::new_v4(), secs: 5 });
        let asking = model.overlay().unwrap();
        assert_eq!((asking.text.as_str(), asking.icon), ("¿Algo más?", Icon::Symbol("mic")));

        // The result the command left is shown first; the question does not outlast its five seconds.
        tick(&mut model, t0 + secs(3));
        assert!(model.overlay().is_some(), "still asking");
        tick(&mut model, t0 + secs(8));
        assert_eq!(model.overlay(), None, "and then it stops");
    }

    #[test]
    fn a_new_recording_takes_the_island_and_ends_the_question() {
        let t0 = Instant::now();
        let mut model = ready_model(t0);
        model.worker_event(t0, WorkerToShell::FollowUp { request_id: Uuid::new_v4(), secs: 5 });
        model.press(t0 + secs(1), Uuid::new_v4());
        assert_ne!(model.overlay().unwrap().text, "¿Algo más?");
        model.abandon_recording();
        assert_eq!(model.overlay(), None, "the question does not come back after the user spoke");
    }
}
