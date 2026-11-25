//! Talking with EVA. Two things live here.
//!
//! **What was just done.** For a few seconds after a command opened something
//! (YouTube, Spotify…) the next thing said leans on it: "ahora busca Naruto"
//! searches YouTube (`eva_intent::context`).
//!
//! **"Modo Eva".** Once a command has been said, EVA stays in conversation
//! (`[conversation] stay_secs`): what is said next needs no wake word and is
//! always for her — a command, a question she answers out loud, or a
//! goodbye — never pasted as dictation. Each exchange renews it; "gracias" or
//! "eso es todo" ends it, and so does silence. She says briefly what she did
//! ("Listo, abrí YouTube"), and, when she has finished speaking, listens for
//! the next thing without the key being held (`listen_after_reply`); that
//! listening stops by itself when the user stops talking.
//!
//! What is heard without the key is held to a higher bar: a free-form task
//! for an agent is asked about first, and dictation is dropped, so a voice in
//! the room cannot start work or type into an app.

use crate::context::WorkerContext;
use eva_intent::context::Recall;
use eva_intent::{AppIndex, InterpretResult, Intent, Interpreted};
use eva_ipc::{WorkerState, WorkerToShell};
use eva_mcp::DesktopService;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long the island asks "¿Algo más?" after a command that opened
/// something, when "modo Eva" is off, and how long a follow-up is understood
/// without the wake word.
pub const FOLLOW_UP: Duration = Duration::from_secs(5);

/// The window is held a moment longer than it is shown: "Spotify abierto"
/// stays on the island first, and the seconds count from after it.
const HELD: Duration = Duration::from_millis(6_500);

/// How many earlier questions and answers the local model is given, so
/// "¿y en Japón?" follows "¿cuál es la capital de Francia?".
const TURNS_KEPT: usize = 4;

/// After EVA stops speaking, a moment before listening, so the microphone
/// does not catch the end of her own voice.
const AFTER_SPEAKING: Duration = if cfg!(test) { Duration::ZERO } else { Duration::from_millis(250) };

/// The state of the conversation with EVA.
#[derive(Default)]
pub struct Conversation {
    /// What was last opened, and until when it can be leaned on.
    open: Mutex<Option<(Recall, Instant)>>,
    /// Until when EVA is in conversation ("modo Eva").
    mode_until: Mutex<Option<Instant>>,
    /// The questions answered in this conversation, oldest first.
    turns: Mutex<Vec<(String, String)>>,
    /// Requests that came from the microphone, and whether without the key.
    heard: Mutex<HashMap<Uuid, bool>>,
    /// How many replies have been spoken: what varies their wording.
    replies: AtomicUsize,
}

impl Conversation {
    /// Opens the window on `recall`, as of `now`.
    fn open_at(&self, recall: Recall, now: Instant) {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        {
            *self.open.lock().unwrap() = Some((recall, now + HELD));
        }
    }

    /// What can be leaned on at `now`, if the window is still open.
    fn current_at(&self, now: Instant) -> Option<Recall> {
        #[allow(clippy::unwrap_used)] // as above
        let mut open = self.open.lock().unwrap();
        match open.as_ref() {
            Some((recall, until)) if now < *until => Some(recall.clone()),
            Some(_) => {
                *open = None;
                None
            }
            None => None,
        }
    }

    /// What can be leaned on right now.
    pub fn current(&self) -> Option<Recall> {
        self.current_at(Instant::now())
    }

    /// Stays in conversation for `secs` from `now`.
    fn stay_at(&self, now: Instant, secs: u64) {
        #[allow(clippy::unwrap_used)] // as above
        {
            *self.mode_until.lock().unwrap() = Some(now + HELD + Duration::from_secs(secs));
        }
    }

    /// Whether EVA is in conversation at `now`. Once it has run out, what was
    /// said in it is forgotten.
    fn in_mode_at(&self, now: Instant) -> bool {
        #[allow(clippy::unwrap_used)] // as above
        let mut until = self.mode_until.lock().unwrap();
        match *until {
            Some(end) if now < end => true,
            Some(_) => {
                *until = None;
                #[allow(clippy::unwrap_used)] // as above
                self.turns.lock().unwrap().clear();
                false
            }
            None => false,
        }
    }

    /// Whether EVA is in conversation now.
    pub fn in_mode(&self) -> bool {
        self.in_mode_at(Instant::now())
    }

    /// Ends the conversation: no mode, nothing to lean on, nothing remembered.
    fn leave(&self) {
        #[allow(clippy::unwrap_used)] // as above
        {
            *self.mode_until.lock().unwrap() = None;
            *self.open.lock().unwrap() = None;
            self.turns.lock().unwrap().clear();
        }
    }

    /// Notes that `request_id` came from the microphone — `hands_free` if the
    /// key was not held. The first note wins (listening marks it at the start).
    pub fn mark_heard(&self, request_id: Uuid, hands_free: bool) {
        #[allow(clippy::unwrap_used)] // as above
        self.heard.lock().unwrap().entry(request_id).or_insert(hands_free);
    }

    /// `Some(hands_free)` if `request_id` came from the microphone.
    pub fn heard(&self, request_id: Uuid) -> Option<bool> {
        #[allow(clippy::unwrap_used)] // as above
        self.heard.lock().unwrap().get(&request_id).copied()
    }

    /// Forgets how `request_id` was heard, once it is over.
    pub fn forget_heard(&self, request_id: Uuid) {
        #[allow(clippy::unwrap_used)] // as above
        self.heard.lock().unwrap().remove(&request_id);
    }

    fn turns(&self) -> Vec<(String, String)> {
        #[allow(clippy::unwrap_used)] // as above
        self.turns.lock().unwrap().clone()
    }

    fn remember_turn(&self, said: &str, answered: &str) {
        #[allow(clippy::unwrap_used)] // as above
        let mut turns = self.turns.lock().unwrap();
        turns.push((said.to_string(), answered.to_string()));
        let excess = turns.len().saturating_sub(TURNS_KEPT);
        turns.drain(..excess);
    }

    /// A number that changes on every reply, to vary its wording.
    fn next_reply(&self) -> usize {
        self.replies.fetch_add(1, Ordering::Relaxed)
    }
}

/// What `text` means: [`eva_intent::interpret_in_context`] with what was just
/// done — and, in conversation, as if the wake word had been said.
pub fn interpret(ctx: &WorkerContext, text: &str, apps: &AppIndex, custom: &[&str], learned: &[String]) -> Interpreted {
    let recall = ctx.conversation.current();
    let in_mode = ctx.config.conversation.stay_secs > 0 && ctx.conversation.in_mode();
    let has_wake = eva_intent::wake::find_wake_word(text, &ctx.wake_word, learned).is_some();
    if in_mode && !has_wake && !text.trim().is_empty() {
        if let Some(intent) = recall.as_ref().and_then(|r| eva_intent::context::follow_up(text, r)) {
            return Interpreted { result: InterpretResult::Command(intent), wake: None };
        }
        let addressed = format!("{}, {}", ctx.wake_word, text.trim());
        let mut interpreted =
            eva_intent::interpret_in_context(&addressed, &ctx.wake_word, apps, custom, learned, recall.as_ref());
        // The wake word was not guessed at: nothing to learn from it.
        interpreted.wake = None;
        return interpreted;
    }
    eva_intent::interpret_in_context(text, &ctx.wake_word, apps, custom, learned, recall.as_ref())
}

/// Runs `intent` as part of the conversation: says what it did, keeps the
/// conversation open, and listens again when it is done speaking.
pub async fn run(ctx: &Arc<WorkerContext>, request_id: Uuid, intent: Intent) {
    let config = &ctx.config.conversation;
    let heard = ctx.conversation.heard(request_id);
    let spoken = heard.is_some();
    let hands_free = heard == Some(true);

    if intent == Intent::EndConversation {
        ctx.conversation.leave();
        ctx.events.emit(WorkerToShell::FollowUp { request_id, secs: 0 });
        let goodbye = pick(&GOODBYES, ctx.conversation.next_reply());
        if config.speak_replies && spoken {
            ctx.events.emit(WorkerToShell::Reply { request_id, text: goodbye.to_string() });
            say(ctx, goodbye, false).await;
        }
        ctx.events.state(request_id, WorkerState::Done(true));
        ctx.conversation.forget_heard(request_id);
        return;
    }

    let is_task = matches!(intent, Intent::AgentTask { .. } | Intent::ContinueAgentTask { .. });
    if is_task && hands_free && matches!(intent, Intent::AgentTask { provider: None, .. }) {
        // Heard without the key: make sure it was meant before starting work.
        let prompt = match &intent {
            Intent::AgentTask { prompt, .. } => prompt.clone(),
            _ => String::new(),
        };
        let yes = ctx.voice.ask_confirmation("¿Se lo paso a un agente?", &prompt).await.unwrap_or(false);
        if !yes {
            ctx.events.state(request_id, WorkerState::Idle);
            ctx.conversation.forget_heard(request_id);
            return;
        }
    }

    // A task runs for minutes: the conversation stays open while it does.
    if is_task {
        keep_open(ctx, request_id, false);
        if config.speak_replies && spoken {
            // Said while the task starts, not before it.
            let ack = pick(&TASK_ACKS, ctx.conversation.next_reply());
            let (speaker, wait) = (Arc::clone(ctx), config.listen_after_reply);
            tokio::spawn(async move {
                say(&speaker, ack, wait).await;
                listen_again(&speaker, request_id, spoken);
            });
        }
        ctx.conversation.forget_heard(request_id);
        crate::commands::run_intent(ctx, request_id, intent).await;
        return;
    }

    // A search typed in place leaves things as they were: the same site, still in front.
    let recall = Recall::of(&intent)
        .or_else(|| matches!(intent, Intent::SearchInSite { .. }).then(|| ctx.conversation.current()).flatten());
    let reply = reply_for(&intent, ctx.conversation.next_reply());
    let answered_itself = matches!(intent, Intent::Ask { .. });
    let problems = ctx.events.problems();
    crate::commands::run_intent(ctx, request_id, intent).await;
    let ok = ctx.events.problems() == problems;

    let opened = match recall.filter(Recall::is_useful) {
        Some(recall) if ok => {
            ctx.conversation.open_at(recall, Instant::now());
            true
        }
        _ => false,
    };
    if config.stay_secs > 0 {
        keep_open(ctx, request_id, opened);
    } else if opened {
        ctx.events.emit(WorkerToShell::FollowUp { request_id, secs: FOLLOW_UP.as_secs() });
    }

    if spoken && config.speak_replies && !answered_itself {
        let line = if ok { reply } else { Some(pick(&FAILED, ctx.conversation.next_reply()).to_string()) };
        if let Some(line) = line {
            say(ctx, &line, config.listen_after_reply).await;
        }
    }
    listen_again(ctx, request_id, spoken);
    ctx.conversation.forget_heard(request_id);
}

/// Keeps the conversation open for `[conversation] stay_secs` and tells the
/// island; with the mode off, only after opening something (`opened`).
fn keep_open(ctx: &WorkerContext, request_id: Uuid, opened: bool) {
    let stay = ctx.config.conversation.stay_secs;
    if stay > 0 {
        ctx.conversation.stay_at(Instant::now(), stay);
        ctx.events.emit(WorkerToShell::FollowUp { request_id, secs: stay });
    } else if opened {
        ctx.events.emit(WorkerToShell::FollowUp { request_id, secs: FOLLOW_UP.as_secs() });
    }
}

/// In conversation, after a request heard from the microphone: listen again,
/// without the key.
fn listen_again(ctx: &WorkerContext, request_id: Uuid, spoken: bool) {
    let config = &ctx.config.conversation;
    if spoken && config.listen_after_reply && config.stay_secs > 0 && ctx.conversation.in_mode() {
        ctx.events.emit(WorkerToShell::ListenAgain { request_id });
    }
}

/// "Eva, ¿qué es un worktree?": answers out loud with the local model, in a
/// sentence or two, and shows it on the island. Without a local model (or if
/// it says this is work for an agent) the question goes to an agent as before.
pub async fn answer(ctx: &Arc<WorkerContext>, request_id: Uuid, question: &str, intent_json: serde_json::Value) {
    let config = &ctx.config;
    let answerer = (config.conversation.answer_questions && config.resolver.enabled)
        .then(|| eva_text::Answerer::new(&config.resolver.base_url, &config.resolver.model))
        .flatten();
    let Some(answerer) = answerer else {
        return crate::tasks::start_new(ctx, request_id, intent_json, question.to_string(), None).await;
    };
    ctx.events.state(request_id, WorkerState::Thinking);
    let (said, earlier, now) = (question.to_string(), ctx.conversation.turns(), now_in_words());
    let reply = tokio::task::spawn_blocking(move || answerer.answer(&said, &earlier, &now)).await.ok().flatten();
    match reply {
        Some(text) if !hands_off(&text) => {
            tracing::info!(%request_id, question, answer = %text, "pregunta respondida por el modelo local");
            ctx.events.emit(WorkerToShell::Reply { request_id, text: text.clone() });
            ctx.conversation.remember_turn(question, &text);
            let spoken = ctx.conversation.heard(request_id).is_some();
            if config.conversation.speak_replies && spoken {
                say(ctx, &text, config.conversation.listen_after_reply).await;
            }
            ctx.events.state(request_id, WorkerState::Done(true));
        }
        _ => {
            tracing::info!(%request_id, question, "el modelo local no respondió; la pregunta va a un agente");
            crate::tasks::start_new(ctx, request_id, intent_json, question.to_string(), None).await;
        }
    }
}

/// Whether the model's answer is it handing the question on to an agent.
fn hands_off(answer: &str) -> bool {
    let folded = eva_text::fold_diacritics(answer).to_lowercase();
    folded.contains("agente")
}

/// Says `text` out loud, waiting until it has been said if `wait` (so that
/// listening after it does not hear it).
async fn say(ctx: &WorkerContext, text: &str, wait: bool) {
    let (desktop, text) = (Arc::clone(&ctx.desktop), text.to_string());
    let spoken = tokio::task::spawn_blocking(move || {
        if wait {
            desktop.speak_and_wait(&text)
        } else {
            desktop.speak(&text)
        }
    })
    .await;
    if !matches!(spoken, Ok(Ok(()))) {
        tracing::warn!("no se pudo decir la respuesta en voz alta");
    }
    if wait {
        tokio::time::sleep(AFTER_SPEAKING).await;
    }
}

/// What EVA says after doing `intent`, if it is something worth a word.
/// `turn` varies the wording, so she does not sound like a recording.
fn reply_for(intent: &Intent, turn: usize) -> Option<String> {
    let line = match intent {
        Intent::OpenApp { app } => pick(&["Listo, abrí {}.", "Ahí está {}.", "Abriendo {}."], turn).replace("{}", app),
        Intent::OpenUrl { url } => {
            pick(&["Listo, abrí {}.", "Ahí tienes {}."], turn).replace("{}", &site_of(url))
        }
        Intent::CloseApp { app } => pick(&["Cerré {}.", "Listo, {} cerrado."], turn).replace("{}", app),
        Intent::WebSearch { query } => pick(&["Buscando {}.", "Aquí tienes {}."], turn).replace("{}", query),
        Intent::SearchInSite { query, site, .. } => {
            pick(&["Buscando {q} en {s}.", "Listo, busqué {q}."], turn).replace("{q}", query).replace("{s}", site)
        }
        Intent::ClickUi { .. } | Intent::Media { .. } | Intent::Custom { .. } => {
            pick(&["Listo.", "Hecho."], turn).to_string()
        }
        Intent::EditSelection { .. } => pick(&["Listo, lo cambié.", "Hecho, ya está reescrito."], turn).to_string(),
        _ => return None,
    };
    Some(line)
}

const TASK_ACKS: [&str; 3] = ["Va, se lo paso a un agente.", "Claro, me pongo con eso.", "Entendido, lo encargo ahora."];
const GOODBYES: [&str; 3] = ["De nada, aquí estoy.", "Con gusto. Hasta luego.", "Para eso estoy."];
const FAILED: [&str; 2] = ["Eso no salió; te dejo el detalle en pantalla.", "No pude hacerlo; mira la pantalla."];

fn pick<'a>(options: &[&'a str], turn: usize) -> &'a str {
    options.get(turn % options.len().max(1)).copied().unwrap_or_default()
}

/// The part of `url` worth saying: its host, without `www.`.
fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// The date and time in words, for the local model, which cannot know them.
fn now_in_words() -> String {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    const DAYS: [&str; 7] = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    const MONTHS: [&str; 12] = [
        "enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre",
        "noviembre", "diciembre",
    ];
    let day = DAYS[now.weekday().num_days_from_monday() as usize];
    let month = MONTHS[now.month0() as usize];
    format!(
        "Ahora es {day} {} de {month} de {}, y son las {}:{:02}.",
        now.day(),
        now.year(),
        now.hour(),
        now.minute()
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::ShellToWorker;
    use eva_mcp::desktop::mock::Call;

    fn typed(text: &str) -> ShellToWorker {
        ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: text.to_string() }
    }

    #[tokio::test]
    async fn after_opening_youtube_the_next_search_needs_no_wake_word_and_searches_there() {
        let youtube = eva_macos::RunningAppInfo {
            localized_name: Some("Brave Browser".to_string()),
            bundle_identifier: Some("com.brave.Browser".to_string()),
            pid: 1,
            window_title: Some("YouTube - Brave".to_string()),
        };
        let mut rig =
            Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::new().with_active_window(youtube)).build();

        let events = rig.run(typed("Adán, abre YouTube")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::OpenUrl("https://www.youtube.com".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::FollowUp { secs: 5, .. })), "{events:?}");

        // With YouTube in front, the search is typed into its own search box.
        rig.run(typed("ahora busca Naruto")).await;
        let calls = rig.desktop.calls();
        assert_eq!(
            calls[1..],
            [
                Call::PressCombo("/".to_string()),
                Call::PressCombo("cmd+a".to_string()),
                Call::InsertText("Naruto".to_string()),
                Call::PressCombo("return".to_string()),
            ]
        );

        // The conversation goes on: same site, still in front.
        let more = rig.run(typed("busca Boruto")).await;
        assert_eq!(rig.desktop.calls().iter().filter(|c| **c == Call::InsertText("Boruto".to_string())).count(), 1);
        assert!(more.iter().any(|e| matches!(e, WorkerToShell::FollowUp { .. })));
    }

    #[tokio::test]
    async fn a_web_app_opened_by_eva_is_brought_back_before_searching_even_if_focus_drifted() {
        // A YouTube "web app" shortcut (a real .app of its own, as Brave's
        // "Create Shortcut" makes): once opened, focus can land anywhere else
        // (an editor, a different browser window with other tabs) without the
        // search having to give up and open a brand new page.
        let elsewhere = eva_macos::RunningAppInfo {
            localized_name: Some("Brave Browser".to_string()),
            bundle_identifier: Some("com.brave.Browser".to_string()),
            pid: 2,
            window_title: Some("Your Repositories - GitHub".to_string()),
        };
        let youtube_app = eva_macos::RunningAppInfo {
            localized_name: Some("YouTube".to_string()),
            bundle_identifier: Some("com.brave.app.youtube".to_string()),
            pid: 3,
            window_title: Some("YouTube".to_string()),
        };
        let apps = crate::apps::AppCatalog::fixed(AppIndex::new(vec![eva_intent::AppEntry::new("YouTube")]));
        let desktop = eva_mcp::desktop::mock::MockDesktop::new()
            .with_active_window(elsewhere)
            .with_active_window_after_open_app("YouTube", youtube_app);
        let mut rig = Rig::builder().apps(apps).desktop(desktop).build();

        rig.run(typed("Adán, abre YouTube")).await;
        // Focus drifted (the mock never moves on its own — this stands for
        // whatever the user clicked into meanwhile).
        rig.run(typed("busca Naruto")).await;

        assert_eq!(
            rig.desktop.calls()[1..],
            [
                Call::OpenApp("YouTube".to_string()),
                Call::PressCombo("/".to_string()),
                Call::PressCombo("cmd+a".to_string()),
                Call::InsertText("Naruto".to_string()),
                Call::PressCombo("return".to_string()),
            ],
            "reactivated before typing, never opened as a new page"
        );
    }

    #[tokio::test]
    async fn if_the_user_went_elsewhere_the_search_opens_the_page_as_before() {
        let notes = eva_macos::RunningAppInfo {
            localized_name: Some("Notes".to_string()),
            bundle_identifier: Some("com.apple.Notes".to_string()),
            pid: 1,
            window_title: Some("Lista".to_string()),
        };
        let mut rig =
            Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::new().with_active_window(notes)).build();
        rig.run(typed("Adán, abre YouTube")).await;
        rig.run(typed("busca Naruto")).await;
        assert_eq!(
            rig.desktop.calls().last(),
            Some(&Call::OpenUrl("https://www.youtube.com/results?search_query=Naruto".to_string())),
            "nothing is typed into Notes"
        );
        assert!(!rig.desktop.calls().iter().any(|c| matches!(c, Call::PressCombo(_))));
    }

    #[tokio::test]
    async fn without_a_conversation_the_same_words_are_dictation() {
        let mut rig = Rig::new();
        rig.run(typed("ahora busca Naruto")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Ahora busca Naruto. ".to_string())]);
    }

    #[tokio::test]
    async fn a_command_that_failed_opens_no_conversation() {
        let mut rig = Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::failing()).build();
        let events = rig.run(typed("Adán, abre YouTube")).await;
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::FollowUp { .. })), "{events:?}");
        assert!(rig.ctx.conversation.current().is_none());
    }

    /// A rig in "modo Eva", speaking its replies.
    fn talking() -> Rig {
        Rig::builder()
            .configure(|c| {
                c.conversation.stay_secs = 20;
                c.conversation.speak_replies = true;
            })
            .build()
    }

    fn said(rig: &Rig) -> Vec<String> {
        rig.desktop
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Speak(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn in_conversation_what_is_said_next_is_a_command_without_the_wake_word() {
        let mut rig = talking();
        let events = rig.run(typed("Adán, abre Brave")).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::FollowUp { secs: 20, .. })), "{events:?}");

        rig.run(typed("cierra Brave")).await;
        assert_eq!(
            rig.desktop.calls(),
            vec![Call::OpenApp("Brave Browser".to_string()), Call::CloseApp("Brave Browser".to_string())]
        );
        assert!(said(&rig).is_empty(), "typed, not heard: nothing is said out loud");
    }

    #[tokio::test]
    async fn a_goodbye_ends_the_conversation_and_dictation_comes_back() {
        let mut rig = talking();
        rig.run(typed("Adán, abre Brave")).await;
        let events = rig.run(typed("gracias, eso es todo")).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::FollowUp { secs: 0, .. })), "{events:?}");
        assert!(!rig.ctx.conversation.in_mode());

        rig.run(typed("cierra Brave")).await;
        assert_eq!(rig.desktop.calls().last(), Some(&Call::InsertText("Cierra Brave. ".to_string())));
    }

    #[tokio::test]
    async fn a_command_heard_from_the_microphone_is_answered_out_loud() {
        let mut rig = talking();
        let request_id = Uuid::new_v4();
        rig.ctx.conversation.mark_heard(request_id, false);
        rig.run(ShellToWorker::RunIntentText { request_id, text: "Adán, abre Brave".to_string() }).await;
        assert_eq!(said(&rig), vec!["Listo, abrí Brave Browser.".to_string()]);
        assert_eq!(rig.ctx.conversation.heard(request_id), None, "forgotten once over");
    }

    #[tokio::test]
    async fn after_speaking_in_conversation_eva_listens_again() {
        let mut rig = Rig::builder()
            .configure(|c| {
                c.conversation.stay_secs = 20;
                c.conversation.speak_replies = true;
                c.conversation.listen_after_reply = true;
            })
            .build();
        let request_id = Uuid::new_v4();
        rig.ctx.conversation.mark_heard(request_id, false);
        let events =
            rig.run(ShellToWorker::RunIntentText { request_id, text: "Adán, abre Brave".to_string() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::ListenAgain { .. })), "{events:?}");

        // Typed: nothing to listen for afterwards.
        let typed_events = rig.run(typed("Adán, cierra Brave")).await;
        assert!(!typed_events.iter().any(|e| matches!(e, WorkerToShell::ListenAgain { .. })));
    }

    #[tokio::test]
    async fn words_heard_without_the_key_are_never_typed() {
        let mut rig = Rig::new();
        let request_id = Uuid::new_v4();
        rig.ctx.conversation.mark_heard(request_id, true);
        let events = rig.run(ShellToWorker::RunIntentText { request_id, text: "hola qué tal".to_string() }).await;
        assert!(rig.desktop.calls().is_empty(), "{:?}", rig.desktop.calls());
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Idle, .. })));
    }

    #[tokio::test]
    async fn a_task_heard_without_the_key_is_asked_about_first() {
        let mut rig = talking();
        let request_id = Uuid::new_v4();
        rig.ctx.conversation.mark_heard(request_id, true);
        let events = rig
            .run_answering(
                ShellToWorker::RunIntentText { request_id, text: "Adán, refactoriza el módulo de pagos".to_string() },
                false,
            )
            .await;
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("agente"))),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::TaskStarted { .. })));
    }

    /// A fake local model on this Mac that always answers `content`.
    fn local_model(content: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = vec![0u8; 65536];
                let _ = stream.read(&mut buffer);
                let payload = serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len())
                        .as_bytes(),
                );
            }
        });
        url
    }

    fn answering(url: String) -> Rig {
        Rig::builder()
            .configure(|c| {
                c.conversation.stay_secs = 20;
                c.conversation.speak_replies = true;
                c.resolver.enabled = true;
                c.resolver.base_url = url;
            })
            .build()
    }

    #[tokio::test]
    async fn a_question_is_answered_by_the_local_model_out_loud_without_an_agent() {
        let mut rig = answering(local_model("Un worktree es una copia de trabajo extra de tu repositorio."));
        let request_id = Uuid::new_v4();
        rig.ctx.conversation.mark_heard(request_id, false);
        let events =
            rig.run(ShellToWorker::RunIntentText { request_id, text: "Adán, ¿qué es un worktree?".to_string() }).await;
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::Reply { text, .. } if text.contains("copia de trabajo"))),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::TaskStarted { .. })));
        assert_eq!(said(&rig), vec!["Un worktree es una copia de trabajo extra de tu repositorio.".to_string()]);
        assert_eq!(rig.ctx.conversation.turns().len(), 1, "remembered for the next question");
    }

    #[tokio::test]
    async fn without_a_local_model_a_question_goes_to_an_agent_as_before() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, ¿qué es un worktree?")).await;
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::Reply { .. })));
        // No agent in this rig: what matters is that it was taken as a task.
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::Error { .. } | WorkerToShell::TaskStarted { .. })),
            "{events:?}"
        );
    }

    #[test]
    fn the_conversation_ends_by_itself_and_forgets_what_was_said() {
        let conversation = Conversation::default();
        let start = Instant::now();
        conversation.stay_at(start, 20);
        conversation.remember_turn("¿qué es git?", "Un control de versiones.");
        assert!(conversation.in_mode_at(start + Duration::from_secs(20)));
        assert!(!conversation.in_mode_at(start + Duration::from_secs(27)));
        assert!(conversation.turns().is_empty());
    }

    #[test]
    fn replies_vary_and_name_what_was_done() {
        let open = Intent::OpenApp { app: "Spotify".to_string() };
        assert_eq!(reply_for(&open, 0).unwrap(), "Listo, abrí Spotify.");
        assert_ne!(reply_for(&open, 0), reply_for(&open, 1));
        let url = Intent::OpenUrl { url: "https://www.youtube.com/watch?v=1".to_string() };
        assert_eq!(reply_for(&url, 0).unwrap(), "Listo, abrí youtube.com.");
        assert_eq!(reply_for(&Intent::Dictation, 0), None);
    }

    #[test]
    fn the_time_is_given_in_words() {
        let now = now_in_words();
        assert!(now.starts_with("Ahora es ") && now.contains(" de "), "{now}");
    }

    #[test]
    fn the_window_closes_by_itself() {
        let conversation = Conversation::default();
        let recall = Recall::of(&Intent::OpenUrl { url: "https://www.youtube.com".to_string() }).unwrap();
        let start = Instant::now();
        conversation.open_at(recall, start);
        assert!(conversation.current_at(start + Duration::from_secs(6)).is_some());
        assert!(conversation.current_at(start + Duration::from_secs(7)).is_none());
        assert!(conversation.current_at(start).is_none(), "and stays closed once it has closed");
    }
}
