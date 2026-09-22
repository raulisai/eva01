//! Turns one [`ShellToWorker`] command into the [`WorkerToShell`] events it
//! produces. This is `eva-worker`'s actual logic; `main.rs` is just the
//! stdio plumbing around it. Kept separate so it is testable against
//! [`eva_mcp::desktop::mock::MockDesktop`] and [`eva_agents::mock::MockProvider`]
//! per `docs/ENGINEERING.md` #5 — no real AppKit call or spawned CLI needed
//! to test what happens to a given transcript.

use eva_agents::{AgentEvent, AgentRegistry, AgentTask};
use eva_intent::{AppIndex, Intent, InterpretResult};
use eva_ipc::{HealthReport, ShellToWorker, WorkerState, WorkerToShell};
use eva_mcp::Desktop;
use eva_store::{Decision, Store};
use eva_text::{Dictionary, RuleOnlyFormatter};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

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
}

/// Handles one command, returning every event it produced, in order.
pub async fn handle(ctx: &WorkerContext, command: ShellToWorker) -> Vec<WorkerToShell> {
    match command {
        ShellToWorker::RunIntentText { request_id, text } => handle_run_intent_text(ctx, request_id, &text).await,
        ShellToWorker::HealthCheck { request_id } => vec![handle_health_check(ctx, request_id)],
        ShellToWorker::StartRecording { request_id } | ShellToWorker::StopRecording { request_id } => {
            // Real microphone capture needs a configured STT model
            // (`docs/PLAN.md` fase 3) — an external file the user downloads,
            // not something this binary bundles. Until `WorkerContext` grows
            // an STT engine field wired to one, this is reported as the
            // real, documented "model not loaded" degradation from
            // `docs/PLAN.md` §3.3 point 5, not silently ignored.
            vec![WorkerToShell::Error {
                request_id: Some(request_id),
                message: "el modelo de reconocimiento de voz no está configurado todavía".to_string(),
                recoverable: true,
            }]
        }
        ShellToWorker::Cancel { request_id } => {
            vec![WorkerToShell::StateChanged { state: WorkerState::Idle, request_id: Some(request_id) }]
        }
        ShellToWorker::Shutdown => Vec::new(),
    }
}

async fn handle_run_intent_text(ctx: &WorkerContext, request_id: Uuid, text: &str) -> Vec<WorkerToShell> {
    let mut events = vec![WorkerToShell::StateChanged { state: WorkerState::Thinking, request_id: Some(request_id) }];

    match eva_intent::interpret(text, &ctx.wake_word, &ctx.app_index) {
        InterpretResult::Dictation => {
            let custom_words = ctx.store.list_custom_words().unwrap_or_default();
            let dictionary = Dictionary::new(custom_words);
            let cleaned = eva_text::clean(text, &dictionary, &RuleOnlyFormatter);

            if let Err(e) = ctx.store.save_transcript(&cleaned.raw, &cleaned.pre_formatted, &cleaned.formatted) {
                tracing::warn!("no se pudo guardar el transcript para el corpus: {e}");
            }

            events.push(WorkerToShell::Transcript { request_id, raw: cleaned.raw, cleaned: cleaned.formatted });
            events.push(WorkerToShell::StateChanged { state: WorkerState::Idle, request_id: Some(request_id) });
        }
        InterpretResult::Command(intent) => {
            events.extend(handle_intent(ctx, request_id, intent).await);
        }
    }

    events
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

fn handle_health_check(_ctx: &WorkerContext, request_id: Uuid) -> WorkerToShell {
    // Agent detection is deliberately not run here (it spawns real
    // processes and can take real time); `eva doctor` per `docs/PLAN.md`
    // §3.4 is a separate, explicit command for that. This health check
    // reports what is cheap and instant to know.
    WorkerToShell::Health {
        request_id,
        report: HealthReport { stt_model_loaded: false, stt_model_id: None, store_ok: true, agents: Vec::new() },
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
        WorkerContext {
            store: Store::open_in_memory().expect("in-memory store must open"),
            app_index: AppIndex::new(vec![eva_intent::AppEntry::new("Brave Browser").with_aliases(["brave"])]),
            wake_word: "Adán".to_string(),
            project_dir: std::env::temp_dir(),
            desktop: Arc::new(desktop),
            agents,
        }
    }

    fn empty_registry() -> AgentRegistry {
        AgentRegistry::new(Vec::new())
    }

    #[tokio::test]
    async fn plain_dictation_produces_a_cleaned_transcript_and_saves_it() {
        let ctx = test_context(MockDesktop::new(), empty_registry());
        let request_id = Uuid::new_v4();

        let events = handle(&ctx, ShellToWorker::RunIntentText { request_id, text: "eh hola mundo".to_string() }).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Transcript { cleaned, .. } if cleaned == "Hola mundo.")));
        assert_eq!(ctx.store.recent_transcripts(10).expect("must succeed").len(), 1);
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
}
