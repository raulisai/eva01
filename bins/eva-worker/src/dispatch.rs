//! Turns one [`ShellToWorker`] command into the [`WorkerToShell`] events it
//! produces. This is `eva-worker`'s actual logic; `main.rs` is just the
//! stdio plumbing around it. Kept separate so it is testable against
//! [`eva_mcp::desktop::mock::MockDesktop`] and [`eva_agents::mock::MockProvider`]
//! per `docs/ENGINEERING.md` #5 — no real AppKit call or spawned CLI needed
//! to test what happens to a given transcript.

use eva_agents::{AgentEvent, AgentRegistry, AgentTask};
use eva_audio::{AudioSource, CaptureHandle, SpeechToText};
use eva_intent::{AppIndex, Intent, InterpretResult};
use eva_ipc::{HealthReport, ShellToWorker, WorkerState, WorkerToShell};
use eva_mcp::Desktop;
use eva_store::{Decision, Store};
use eva_text::{Dictionary, RuleOnlyFormatter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// The audio capture + speech-to-text pieces, present only when a model was
/// successfully configured and loaded at startup (`docs/PLAN.md` §3.3 point
/// 5: "el modelo STT no carga → overlay en rojo + notificación clara al
/// primer intento de grabar", never a silent no-op).
pub struct AudioContext {
    /// Captures the microphone, resampled to 16 kHz mono.
    pub source: Arc<dyn AudioSource>,
    /// Transcribes a finished recording.
    pub stt: Arc<dyn SpeechToText>,
    /// A human-readable identifier for the loaded model, for `eva doctor`.
    pub model_id: String,
}

/// The buffer and capture handle for a recording currently in progress,
/// between a `StartRecording` and its matching `StopRecording`.
struct RecordingSession {
    request_id: Uuid,
    handle: Box<dyn CaptureHandle>,
    buffer: Arc<Mutex<Vec<f32>>>,
}

/// Everything a single command needs to be handled — the pieces `main.rs`
/// builds once at startup and passes in by reference for every command.
pub struct WorkerContext {
    /// The history/dictionary/settings/audit database.
    pub store: Store,
    /// Resolves spoken app names to canonical ones. Empty for the MVP
    /// (`docs/PLAN.md` §10 decision 8: the focused project/app is used
    /// instead of a scanned catalog) — see `docs/PLAN.md` fase 6.
    pub app_index: AppIndex,
    /// The configured wake word ("Adán" by default).
    pub wake_word: String,
    /// Where dictated tasks run — `docs/PLAN.md` fase 6's "proyecto activo
    /// por contexto" is a later increment; for now this is a fixed
    /// directory (e.g. the current working directory `eva-worker` was
    /// started in).
    pub project_dir: PathBuf,
    /// Opens/closes apps, opens URLs, pastes text, reads the active window.
    pub desktop: Arc<dyn Desktop>,
    /// Codex/Claude Code, in priority order.
    pub agents: AgentRegistry,
    /// `None` until a real STT model is configured and loaded — see
    /// [`AudioContext`]'s doc for why `StartRecording` reports a real error
    /// rather than pretending to record when this is `None`.
    pub audio: Option<AudioContext>,
    /// The in-progress recording, if any. A plain `std::sync::Mutex`
    /// (not `tokio::sync::Mutex`): every lock is held only across
    /// synchronous code, never across an `.await`, so the cheaper
    /// std-library lock is the correct choice, not a shortcut.
    recording: Mutex<Option<RecordingSession>>,
}

impl WorkerContext {
    /// Builds a context. `recording` always starts empty; every other field
    /// is supplied by the caller.
    pub fn new(
        store: Store,
        app_index: AppIndex,
        wake_word: String,
        project_dir: PathBuf,
        desktop: Arc<dyn Desktop>,
        agents: AgentRegistry,
        audio: Option<AudioContext>,
    ) -> Self {
        WorkerContext { store, app_index, wake_word, project_dir, desktop, agents, audio, recording: Mutex::new(None) }
    }
}

/// Handles one command, returning every event it produced, in order.
pub async fn handle(ctx: &WorkerContext, command: ShellToWorker) -> Vec<WorkerToShell> {
    match command {
        ShellToWorker::RunIntentText { request_id, text } => handle_run_intent_text(ctx, request_id, &text).await,
        ShellToWorker::HealthCheck { request_id } => vec![handle_health_check(ctx, request_id)],
        ShellToWorker::StartRecording { request_id } => handle_start_recording(ctx, request_id),
        ShellToWorker::StopRecording { request_id } => handle_stop_recording(ctx, request_id).await,
        ShellToWorker::Cancel { request_id } => {
            if let Some(session) = take_recording_session(ctx, request_id) {
                session.handle.stop();
            }
            vec![WorkerToShell::StateChanged { state: WorkerState::Idle, request_id: Some(request_id) }]
        }
        ShellToWorker::AddCustomWord { request_id, word } => handle_add_custom_word(ctx, request_id, &word),
        ShellToWorker::RemoveCustomWord { request_id, word } => handle_remove_custom_word(ctx, request_id, &word),
        ShellToWorker::ListCustomWords { request_id } => vec![list_custom_words_event(ctx, request_id)],
        ShellToWorker::SetWakeWord { request_id, word } => handle_set_wake_word(ctx, request_id, word),
        ShellToWorker::Shutdown => Vec::new(),
    }
}

fn list_custom_words_event(ctx: &WorkerContext, request_id: Uuid) -> WorkerToShell {
    let words = ctx.store.list_custom_words().unwrap_or_default();
    WorkerToShell::CustomWords { request_id, words }
}

fn handle_add_custom_word(ctx: &WorkerContext, request_id: Uuid, word: &str) -> Vec<WorkerToShell> {
    if word.trim().is_empty() {
        return vec![WorkerToShell::Error {
            request_id: Some(request_id),
            message: "la palabra está vacía".to_string(),
            recoverable: true,
        }];
    }
    match ctx.store.add_custom_word(word) {
        Ok(()) => vec![list_custom_words_event(ctx, request_id)],
        Err(e) => vec![WorkerToShell::Error { request_id: Some(request_id), message: e.to_string(), recoverable: true }],
    }
}

fn handle_remove_custom_word(ctx: &WorkerContext, request_id: Uuid, word: &str) -> Vec<WorkerToShell> {
    match ctx.store.remove_custom_word(word) {
        Ok(()) => vec![list_custom_words_event(ctx, request_id)],
        Err(e) => vec![WorkerToShell::Error { request_id: Some(request_id), message: e.to_string(), recoverable: true }],
    }
}

/// Setting the wake word takes effect immediately for this run (a plain
/// `String`, not `Mutex`-guarded, would need `&mut self` throughout —
/// `ctx.wake_word` staying fixed for the process's lifetime and requiring a
/// restart to pick up a change is a small, deliberate trade-off, not an
/// oversight: `docs/PLAN.md` never asked for changing it without a restart,
/// and this is far simpler than adding interior mutability for one setting).
fn handle_set_wake_word(ctx: &WorkerContext, request_id: Uuid, word: String) -> Vec<WorkerToShell> {
    if word.trim().is_empty() {
        return vec![WorkerToShell::Error {
            request_id: Some(request_id),
            message: "la palabra de activación no puede estar vacía".to_string(),
            recoverable: true,
        }];
    }
    match ctx.store.set_setting("wake_word", &word) {
        // Takes effect on the next start (see the doc comment above) —
        // `Ack` alone, with no false-alarm `Error` for what is not one.
        Ok(()) => vec![WorkerToShell::Ack { request_id }],
        Err(e) => vec![WorkerToShell::Error { request_id: Some(request_id), message: e.to_string(), recoverable: true }],
    }
}

async fn handle_run_intent_text(ctx: &WorkerContext, request_id: Uuid, text: &str) -> Vec<WorkerToShell> {
    let mut events = vec![WorkerToShell::StateChanged { state: WorkerState::Thinking, request_id: Some(request_id) }];
    events.extend(process_dictation_or_intent(ctx, request_id, text).await);
    events
}

/// Cleans `raw` text, saves it to the corpus, pastes it at the cursor, and
/// returns the resulting events. Shared by [`handle_run_intent_text`] (typed
/// text, e.g. from the `eva intent` CLI) and [`handle_stop_recording`] (a
/// real transcript) — both are, deliberately, the exact same downstream
/// logic, per `eva-ipc`'s own doc comment on `RunIntentText`.
fn process_dictation(ctx: &WorkerContext, request_id: Uuid, raw: &str) -> Vec<WorkerToShell> {
    let custom_words = ctx.store.list_custom_words().unwrap_or_default();
    let dictionary = Dictionary::new(custom_words);
    let cleaned = eva_text::clean(raw, &dictionary, &RuleOnlyFormatter);

    if let Err(e) = ctx.store.save_transcript(&cleaned.raw, &cleaned.pre_formatted, &cleaned.formatted) {
        tracing::warn!("no se pudo guardar el transcript para el corpus: {e}");
    }

    let mut events = vec![WorkerToShell::Transcript {
        request_id,
        raw: cleaned.raw,
        cleaned: cleaned.formatted.clone(),
    }];

    let final_state = if cleaned.formatted.trim().is_empty() {
        // Nothing worth pasting (e.g. the whole utterance was filler) — not
        // an error, just nothing to do.
        WorkerState::Done(true)
    } else {
        match ctx.desktop.insert_text(&cleaned.formatted) {
            Ok(()) => WorkerState::Done(true),
            Err(e) => {
                events.push(WorkerToShell::Error {
                    request_id: Some(request_id),
                    message: e.to_string(),
                    recoverable: true,
                });
                WorkerState::Done(false)
            }
        }
    };
    events.push(WorkerToShell::StateChanged { state: final_state, request_id: Some(request_id) });

    events
}

/// Starts capturing audio, if a model is configured. `chunk_size` is
/// arbitrary here (push-to-talk needs no VAD segmentation — the hotkey
/// itself marks the utterance's boundaries) but must be non-zero for
/// `AudioSource::start`'s rechunking; 1600 samples is 100ms at 16 kHz, a
/// reasonable balance between callback frequency and overhead.
const CAPTURE_CHUNK_SIZE: usize = 1_600;

fn handle_start_recording(ctx: &WorkerContext, request_id: Uuid) -> Vec<WorkerToShell> {
    let Some(audio) = &ctx.audio else {
        // The real, documented "model not loaded" degradation from
        // `docs/PLAN.md` §3.3 point 5 — never a silent no-op.
        return vec![WorkerToShell::Error {
            request_id: Some(request_id),
            message: "el modelo de reconocimiento de voz no está configurado (falta EVA_STT_MODEL_PATH)".to_string(),
            recoverable: true,
        }];
    };

    #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
    let mut recording = ctx.recording.lock().unwrap();
    if recording.is_some() {
        return vec![WorkerToShell::Error {
            request_id: Some(request_id),
            message: "ya hay una grabación en curso".to_string(),
            recoverable: true,
        }];
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let buffer_for_callback = Arc::clone(&buffer);
    let start_result = audio.source.start(
        CAPTURE_CHUNK_SIZE,
        Box::new(move |chunk| {
            #[allow(clippy::unwrap_used)] // only poisoned if this same callback panicked, forbidden by workspace policy
            buffer_for_callback.lock().unwrap().extend(chunk);
        }),
    );

    match start_result {
        Ok(handle) => {
            *recording = Some(RecordingSession { request_id, handle, buffer });
            vec![WorkerToShell::StateChanged { state: WorkerState::Listening, request_id: Some(request_id) }]
        }
        Err(e) => vec![WorkerToShell::Error {
            request_id: Some(request_id),
            message: e.to_string(),
            recoverable: true,
        }],
    }
}

/// Takes the current recording session, if `request_id` matches it — used by
/// both `StopRecording` and `Cancel` so a stray, mismatched id (e.g. a
/// duplicate or delayed message) can never stop someone else's recording.
fn take_recording_session(ctx: &WorkerContext, request_id: Uuid) -> Option<RecordingSession> {
    #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
    let mut recording = ctx.recording.lock().unwrap();
    if recording.as_ref().is_some_and(|s| s.request_id == request_id) {
        recording.take()
    } else {
        None
    }
}

async fn handle_stop_recording(ctx: &WorkerContext, request_id: Uuid) -> Vec<WorkerToShell> {
    let Some(session) = take_recording_session(ctx, request_id) else {
        return vec![WorkerToShell::Error {
            request_id: Some(request_id),
            message: "no había una grabación en curso con ese id".to_string(),
            recoverable: true,
        }];
    };
    session.handle.stop();

    #[allow(clippy::unwrap_used)] // only poisoned if the capture callback panicked, forbidden by workspace policy
    let samples = std::mem::take(&mut *session.buffer.lock().unwrap());

    let mut events = vec![WorkerToShell::StateChanged { state: WorkerState::Thinking, request_id: Some(request_id) }];

    let Some(audio) = &ctx.audio else {
        // Can only happen if the model was unloaded between Start and Stop,
        // which nothing in this binary does today — defensive, not expected.
        events.push(WorkerToShell::Error {
            request_id: Some(request_id),
            message: "el modelo de reconocimiento de voz ya no está disponible".to_string(),
            recoverable: true,
        });
        return events;
    };

    // Whisper inference is CPU-bound and can take real time; running it
    // directly here would block the async task's thread for that whole
    // duration. `spawn_blocking` moves it to a thread meant for exactly
    // this, so the runtime's other work (an agent task's event streaming,
    // for instance) is never held up by one transcription.
    let stt = Arc::clone(&audio.stt);
    let transcribe_result =
        tokio::task::spawn_blocking(move || stt.transcribe(&samples)).await;

    match transcribe_result {
        Ok(Ok(transcript)) => events.extend(process_dictation_or_intent(ctx, request_id, &transcript.text).await),
        Ok(Err(e)) => events.push(WorkerToShell::Error {
            request_id: Some(request_id),
            message: e.to_string(),
            recoverable: true,
        }),
        Err(join_error) => events.push(WorkerToShell::Error {
            request_id: Some(request_id),
            message: format!("la tarea de transcripción falló: {join_error}"),
            recoverable: true,
        }),
    }

    events
}

/// A real transcript, from either STT or (in `RunIntentText`'s case) typed
/// text, is either plain dictation or a wake-word-prefixed command —
/// exactly what [`handle_run_intent_text`] already does, factored out so
/// [`handle_stop_recording`] does not duplicate it.
async fn process_dictation_or_intent(ctx: &WorkerContext, request_id: Uuid, text: &str) -> Vec<WorkerToShell> {
    // A hesitation before the wake word ("eh, Adán, abre Brave") is normal,
    // natural speech, but `strip_wake_word` requires the wake word to be the
    // literal first word — found while testing the recording pipeline
    // end to end: a leading "eh" silently defeated the gate and the whole
    // utterance fell through to plain dictation instead of a command.
    // Stripping universal (never-a-real-word) fillers first is always safe
    // — see `eva_text::filler`'s own doc for why — and fixes this without
    // weakening the gate itself.
    let gate_input = eva_text::filler::remove_universal_fillers(text);

    match eva_intent::interpret(&gate_input, &ctx.wake_word, &ctx.app_index) {
        InterpretResult::Dictation => process_dictation(ctx, request_id, text),
        InterpretResult::Command(intent) => handle_intent(ctx, request_id, intent).await,
    }
}

async fn handle_intent(ctx: &WorkerContext, request_id: Uuid, intent: Intent) -> Vec<WorkerToShell> {
    let mut events = Vec::new();
    let intent_json = serde_json::to_value(&intent).unwrap_or(serde_json::Value::Null);
    events.push(WorkerToShell::IntentRecognized { request_id, intent_json: intent_json.clone() });

    match intent {
        // `eva_intent::intent::parse` never actually constructs this
        // variant — it exists on `Intent` for a caller that runs the parser
        // on text that was never wake-word-stripped in the first place (see
        // its own doc comment). `handle_intent` is only ever reached via
        // `InterpretResult::Command`, so getting here would mean an
        // inconsistency upstream, not a real "the user just dictated"
        // moment (the raw text is not even available at this point to
        // treat as dictation) — logged and treated as a no-op rather than
        // guessed at.
        Intent::Dictation => {
            tracing::warn!("Intent::Dictation llegó a handle_intent; esto no debería pasar, se ignora");
            events.push(WorkerToShell::StateChanged { state: WorkerState::Idle, request_id: Some(request_id) });
        }
        Intent::Blocked { matched_stem, text } => {
            let audit_id = ctx.store.log_decision(None, &intent_json, Decision::Blocked).ok();
            tracing::warn!(%matched_stem, %text, "comando bloqueado por la lista negra de verbos destructivos");
            if let Some(id) = audit_id {
                let _ = ctx.store.record_audit_result(id, "bloqueado: verbo destructivo detectado");
            }
            events.push(WorkerToShell::Error {
                request_id: Some(request_id),
                message: format!("no ejecuto eso: contiene el verbo bloqueado \"{matched_stem}\""),
                recoverable: true,
            });
            events.push(WorkerToShell::StateChanged { state: WorkerState::Done(false), request_id: Some(request_id) });
        }
        Intent::OpenApp { app } => {
            events.extend(run_desktop_action(ctx, request_id, &intent_json, "open_app", || ctx.desktop.open_app(&app)));
        }
        Intent::CloseApp { app } => {
            events.extend(run_desktop_action(ctx, request_id, &intent_json, "close_app", || {
                ctx.desktop.close_app(&app)
            }));
        }
        Intent::OpenUrl { url } => {
            events.extend(run_desktop_action(ctx, request_id, &intent_json, "open_url", || {
                ctx.desktop.open_url(&url)
            }));
        }
        Intent::WebSearch { query } => {
            let url = format!("https://www.google.com/search?q={}", urlencode(&query));
            events.extend(run_desktop_action(ctx, request_id, &intent_json, "web_search", || {
                ctx.desktop.open_url(&url)
            }));
        }
        Intent::AgentTask { prompt } => {
            events.extend(dispatch_agent_task(ctx, request_id, &intent_json, prompt).await);
        }
    }

    events
}

/// Runs a `Desktop` action, auto-approving it (`docs/PLAN.md` fase 5: these
/// action kinds — open/close app, open a URL, search — are the "auto" tier
/// of the not-yet-built graduated policy; nothing here is a destructive
/// verb, which was already ruled out by `Intent::Blocked` upstream of this
/// call), and turns the result into the matching events + audit log entry.
fn run_desktop_action(
    ctx: &WorkerContext,
    request_id: Uuid,
    intent_json: &serde_json::Value,
    action_name: &str,
    action: impl FnOnce() -> Result<(), eva_mcp::DesktopError>,
) -> Vec<WorkerToShell> {
    let audit_id = ctx.store.log_decision(None, intent_json, Decision::AutoApproved).ok();
    let result = action();

    let (summary, state, error_event) = match &result {
        Ok(()) => (format!("{action_name}: ok"), WorkerState::Done(true), None),
        Err(e) => (
            format!("{action_name}: error: {e}"),
            WorkerState::Done(false),
            Some(WorkerToShell::Error { request_id: Some(request_id), message: e.to_string(), recoverable: true }),
        ),
    };

    if let Some(id) = audit_id {
        let _ = ctx.store.record_audit_result(id, &summary);
    }

    let mut events = Vec::new();
    events.extend(error_event);
    events.push(WorkerToShell::StateChanged { state, request_id: Some(request_id) });
    events
}

async fn dispatch_agent_task(
    ctx: &WorkerContext,
    request_id: Uuid,
    intent_json: &serde_json::Value,
    prompt: String,
) -> Vec<WorkerToShell> {
    let audit_id = ctx.store.log_decision(None, intent_json, Decision::AutoApproved).ok();

    let provider = match ctx.agents.select(None).await {
        Ok(provider) => provider,
        Err(e) => {
            if let Some(id) = audit_id {
                let _ = ctx.store.record_audit_result(id, &format!("sin agente disponible: {e}"));
            }
            return vec![
                WorkerToShell::Error { request_id: Some(request_id), message: e.to_string(), recoverable: true },
                WorkerToShell::StateChanged { state: WorkerState::Done(false), request_id: Some(request_id) },
            ];
        }
    };

    let task = AgentTask {
        prompt,
        project_dir: ctx.project_dir.clone(),
        session_id: request_id,
        resume_session_id: None,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut events = vec![WorkerToShell::StateChanged { state: WorkerState::Executing, request_id: Some(request_id) }];

    let running = match provider.execute(&task, tx).await {
        Ok(running) => running,
        Err(e) => {
            if let Some(id) = audit_id {
                let _ = ctx.store.record_audit_result(id, &format!("no se pudo iniciar: {e}"));
            }
            events.push(WorkerToShell::Error { request_id: Some(request_id), message: e.to_string(), recoverable: true });
            events.push(WorkerToShell::StateChanged { state: WorkerState::Done(false), request_id: Some(request_id) });
            return events;
        }
    };

    while let Some(event) = rx.recv().await {
        forward_agent_event(&mut events, request_id, &event);
    }

    let outcome = running.wait().await;
    let (summary, final_state) = summarize_outcome(&outcome);
    if let Some(id) = audit_id {
        let _ = ctx.store.record_audit_result(id, &summary);
    }
    events.push(WorkerToShell::StateChanged { state: final_state, request_id: Some(request_id) });

    events
}

fn forward_agent_event(events: &mut Vec<WorkerToShell>, request_id: Uuid, event: &AgentEvent) {
    let event_json = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    events.push(WorkerToShell::AgentEvent { request_id, event_json });
}

fn summarize_outcome(outcome: &eva_agents::AgentOutcome) -> (String, WorkerState) {
    match outcome {
        eva_agents::AgentOutcome::Completed { summary } => (
            summary.clone().unwrap_or_else(|| "completado".to_string()),
            WorkerState::Done(true),
        ),
        eva_agents::AgentOutcome::Failed { message } => (message.clone(), WorkerState::Done(false)),
        eva_agents::AgentOutcome::Cancelled => ("cancelado".to_string(), WorkerState::Done(false)),
    }
}

fn handle_health_check(ctx: &WorkerContext, request_id: Uuid) -> WorkerToShell {
    // Agent detection is deliberately not run here (it spawns real
    // processes and can take real time); `eva doctor` per `docs/PLAN.md`
    // §3.4 is a separate, explicit command for that. This health check
    // reports what is cheap and instant to know.
    WorkerToShell::Health {
        request_id,
        report: HealthReport {
            stt_model_loaded: ctx.audio.is_some(),
            stt_model_id: ctx.audio.as_ref().map(|a| a.model_id.clone()),
            store_ok: true,
            agents: Vec::new(),
        },
    }
}

/// A minimal, dependency-free percent-encoder for a search query in a URL.
/// Not a general-purpose URL encoder — just enough for the common
/// characters a spoken search query produces (spaces, accented letters).
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use eva_mcp::desktop::mock::{Call, MockDesktop};

    fn test_context(desktop: MockDesktop, agents: AgentRegistry) -> WorkerContext {
        WorkerContext::new(
            Store::open_in_memory().expect("in-memory store must open"),
            AppIndex::new(vec![eva_intent::AppEntry::new("Brave Browser").with_aliases(["brave"])]),
            "Adán".to_string(),
            std::env::temp_dir(),
            Arc::new(desktop),
            agents,
            None,
        )
    }

    fn empty_registry() -> AgentRegistry {
        AgentRegistry::new(Vec::new())
    }

    #[tokio::test]
    async fn plain_dictation_produces_a_cleaned_transcript_saves_it_and_pastes_it() {
        let desktop = Arc::new(MockDesktop::new());
        let ctx = WorkerContext { desktop: desktop.clone(), ..test_context(MockDesktop::new(), empty_registry()) };
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "eh hola mundo".to_string() }).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Transcript { cleaned, .. } if cleaned == "Hola mundo.")));
        assert_eq!(ctx.store.recent_transcripts(10).expect("must succeed").len(), 1);
        // The fix for the gap found while testing the MVP end to end: plain
        // dictation must actually reach the cursor, not just get logged.
        assert_eq!(desktop.calls(), vec![Call::InsertText("Hola mundo.".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn a_full_recording_is_captured_transcribed_cleaned_and_pasted() {
        let script = vec![0.0_f32; CAPTURE_CHUNK_SIZE * 2];
        let source = eva_audio::capture::mock::ScriptedSource::new(script);
        let stt = eva_audio::transcribe::mock::FixedTranscript::new("eh adán abre brave");
        let audio = AudioContext {
            source: Arc::new(source),
            stt: Arc::new(stt),
            model_id: "mock-model".to_string(),
        };

        let desktop = Arc::new(MockDesktop::new());
        let mut ctx = test_context(MockDesktop::new(), empty_registry());
        ctx.desktop = desktop.clone();
        ctx.audio = Some(audio);
        let request_id = Uuid::new_v4();

        let start_events = handle(&ctx, ShellToWorker::StartRecording { request_id }).await;
        assert!(start_events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Listening, .. })));

        let stop_events = handle(&ctx, ShellToWorker::StopRecording { request_id }).await;

        // The mock STT ignores the audio and always returns the same text,
        // which starts with the wake word — so this must resolve to a
        // command (OpenApp), not dictation, exercising the exact same
        // interpret-then-act pipeline a real transcript would go through.
        assert_eq!(desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
        assert!(stop_events
            .iter()
            .any(|e| matches!(e, WorkerToShell::IntentRecognized { .. })));
    }

    #[tokio::test]
    async fn stop_recording_with_an_unknown_request_id_is_a_clear_error() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let events = handle(&ctx, ShellToWorker::StopRecording { request_id: Uuid::new_v4() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn starting_a_second_recording_while_one_is_active_is_rejected() {
        let script = vec![0.0_f32; CAPTURE_CHUNK_SIZE];
        let audio = AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(script)),
            stt: Arc::new(eva_audio::transcribe::mock::FixedTranscript::new("hola")),
            model_id: "mock-model".to_string(),
        };
        let mut ctx = test_context(MockDesktop::new(), empty_registry());
        ctx.audio = Some(audio);

        let first = handle(&ctx, ShellToWorker::StartRecording { request_id: Uuid::new_v4() }).await;
        assert!(first.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Listening, .. })));

        let second = handle(&ctx, ShellToWorker::StartRecording { request_id: Uuid::new_v4() }).await;
        assert!(second.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn cancel_stops_an_in_progress_recording() {
        let script = vec![0.0_f32; CAPTURE_CHUNK_SIZE];
        let audio = AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(script)),
            stt: Arc::new(eva_audio::transcribe::mock::FixedTranscript::new("hola")),
            model_id: "mock-model".to_string(),
        };
        let mut ctx = test_context(MockDesktop::new(), empty_registry());
        ctx.audio = Some(audio);
        let request_id = Uuid::new_v4();

        handle(&ctx, ShellToWorker::StartRecording { request_id }).await;
        let events = handle(&ctx, ShellToWorker::Cancel { request_id }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Idle, .. })));

        // The session must actually be gone — stopping it again is now an error.
        let stop_events = handle(&ctx, ShellToWorker::StopRecording { request_id }).await;
        assert!(stop_events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn open_app_command_calls_the_desktop_and_logs_audit() {
        let desktop = Arc::new(MockDesktop::new());
        let ctx = WorkerContext {
            desktop: desktop.clone(),
            ..test_context(MockDesktop::new(), empty_registry())
        };
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "Adán, abre brave".to_string() }).await;

        assert_eq!(desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        assert_eq!(ctx.store.recent_audit(10).expect("must succeed").len(), 1);
    }

    #[tokio::test]
    async fn destructive_command_is_blocked_and_never_reaches_the_desktop() {
        let desktop = Arc::new(MockDesktop::new());
        let ctx = WorkerContext { desktop: desktop.clone(), ..test_context(MockDesktop::new(), empty_registry()) };
        let request_id = Uuid::new_v4();

        let events =
            handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "Adán, borra el proyecto".to_string() }).await;

        assert!(desktop.calls().is_empty(), "a blocked command must never call the desktop");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        let audit = ctx.store.recent_audit(10).expect("must succeed");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, Decision::Blocked);
    }

    #[tokio::test]
    async fn agent_task_with_no_provider_reports_a_clear_error() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let request_id = Uuid::new_v4();

        let events =
            handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "Adán, agrega tests al login".to_string() }).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("disponible"))));
    }

    #[tokio::test]
    async fn agent_task_with_a_working_mock_provider_streams_events_and_completes() {
        let mock_agent = eva_agents::mock::MockProvider::always_completes("codex", "3 archivos cambiados");
        let registry = AgentRegistry::new(vec![Box::new(mock_agent)]);
        let ctx = test_context(MockDesktop::new(), registry);
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "Adán, agrega tests".to_string() }).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn web_search_opens_a_google_url_with_the_query_encoded() {
        let desktop = Arc::new(MockDesktop::new());
        let ctx = WorkerContext { desktop: desktop.clone(), ..test_context(MockDesktop::new(), empty_registry()) };
        let request_id = Uuid::new_v4();

        handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "Adán, busca clima hoy".to_string() }).await;

        let calls = desktop.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], Call::OpenUrl(url) if url.contains("google.com/search") && url.contains("clima")));
    }

    #[tokio::test]
    async fn start_recording_without_a_configured_model_is_a_clear_recoverable_error() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::StartRecording { request_id }).await;
        assert!(matches!(&events[0], WorkerToShell::Error { recoverable: true, .. }));
    }

    #[test]
    fn urlencode_handles_spaces_and_accents() {
        assert_eq!(urlencode("clima hoy"), "clima%20hoy");
        assert!(urlencode("café").starts_with("caf"));
    }

    #[tokio::test]
    async fn add_custom_word_persists_it_and_returns_the_updated_list() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::AddCustomWord { request_id, word: "García".to_string() }).await;

        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::CustomWords { words, .. } if words == &vec!["García".to_string()])));
        assert_eq!(ctx.store.list_custom_words().expect("must succeed"), vec!["García"]);
    }

    #[tokio::test]
    async fn adding_a_custom_word_actually_improves_later_dictation() {
        // The point of the whole feature, exercised end to end: a word
        // added via the IPC command must be picked up by the very next
        // dictation, not just sit in the store unused.
        let desktop = Arc::new(MockDesktop::new());
        let ctx = WorkerContext { desktop: desktop.clone(), ..test_context(MockDesktop::new(), empty_registry()) };

        handle(&ctx, ShellToWorker::AddCustomWord { request_id: Uuid::new_v4(), word: "García".to_string() }).await;
        handle(&ctx, ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: "hola Garcia".to_string() }).await;

        assert_eq!(desktop.calls(), vec![Call::InsertText("Hola García.".to_string())]);
    }

    #[tokio::test]
    async fn remove_custom_word_takes_it_out_of_the_list() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        ctx.store.add_custom_word("García").expect("must succeed");

        let events =
            handle(&ctx, ShellToWorker::RemoveCustomWord { request_id: Uuid::new_v4(), word: "García".to_string() })
                .await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::CustomWords { words, .. } if words.is_empty())));
    }

    #[tokio::test]
    async fn adding_an_empty_word_is_a_clear_error_not_a_silent_no_op() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let events = handle(&ctx, ShellToWorker::AddCustomWord { request_id: Uuid::new_v4(), word: "   ".to_string() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn set_wake_word_persists_to_settings_and_acknowledges() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::SetWakeWord { request_id, word: "Eva".to_string() }).await;

        assert_eq!(events, vec![WorkerToShell::Ack { request_id }]);
        let saved: Option<String> = ctx.store.get_setting("wake_word").expect("must succeed");
        assert_eq!(saved, Some("Eva".to_string()));
    }
}
