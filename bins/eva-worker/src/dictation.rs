//! What happens to a transcript: either it is plain dictation, cleaned in
//! the style of the app it is going into and pasted at the cursor, or it
//! starts with the wake word and becomes a command ([`crate::commands`]) —
//! including the edit mode, where a spoken instruction rewrites the text the
//! user has selected (`docs/PLAN.md` fase 9).

use crate::context::WorkerContext;
use eva_config::{ActionKind, Origin};
use eva_gateway::{Action, Verdict};
use eva_intent::InterpretResult;
use eva_ipc::{WorkerState, WorkerToShell};
use eva_mcp::{Desktop, DesktopService};
use eva_text::{Dictionary, FormatError, RuleOnlyFormatter, Style};
use std::sync::Arc;
use uuid::Uuid;

/// A real transcript, from either STT or (in `RunIntentText`'s case) typed
/// text, is either plain dictation or a wake-word-prefixed command.
pub async fn process_text(ctx: &Arc<WorkerContext>, request_id: Uuid, text: &str) {
    let result = classify(ctx, text);
    // The line to read when "it didn't do what I said": what was heard, what
    // the gate made of it, and so whether it was pasted or run.
    match &result {
        InterpretResult::Dictation => tracing::info!(
            %request_id, heard = %text, wake_word = %ctx.wake_word,
            "enunciado tomado como dictado (no empieza con la palabra de activación)"
        ),
        InterpretResult::Command(intent) => tracing::info!(
            %request_id, heard = %text, intent = %serde_json::to_string(intent).unwrap_or_default(),
            "enunciado tomado como orden"
        ),
    }
    match result {
        InterpretResult::Dictation => {
            // So the shell can say "Escribiendo" instead of a bare "Pensando".
            ctx.events.emit(WorkerToShell::IntentRecognized {
                request_id,
                intent_json: serde_json::json!({ "kind": "dictation" }),
            });
            dictate(ctx, request_id, text).await
        }
        InterpretResult::Command(intent) => {
            // A misheard command is as worth flagging as a misheard dictation.
            ctx.harvest.remember_dictation(request_id, None, text, None);
            crate::commands::run_intent(ctx, request_id, intent).await
        }
    }
}

/// What `text` is: dictation, or a command (the user's own phrases included).
/// Does nothing about it.
fn classify(ctx: &WorkerContext, text: &str) -> InterpretResult {
    // A hesitation before the wake word ("eh, Adán, abre Brave") is normal,
    // natural speech, but `strip_wake_word` requires the wake word to be the
    // literal first word — found while testing the recording pipeline end
    // to end: a leading "eh" silently defeated the gate and the whole
    // utterance fell through to plain dictation instead of a command.
    // Stripping universal (never-a-real-word) fillers first is always safe
    // — see `eva_text::filler`'s own doc for why — and fixes this without
    // weakening the gate itself.
    let gate_input = eva_text::filler::remove_universal_fillers(text);

    let phrases: Vec<&str> = ctx.config.custom_commands().flat_map(|c| c.phrases()).collect();
    let interpret =
        |apps: &eva_intent::AppIndex| eva_intent::interpret_with(&gate_input, &ctx.wake_word, apps, &phrases);
    let result = interpret(&ctx.app_index.current());
    // An app the index does not know may have been installed since it was
    // built: look again before saying it is not there.
    if matches!(result, InterpretResult::Command(eva_intent::Intent::AppNotFound { .. })) && ctx.app_index.refresh() {
        return interpret(&ctx.app_index.current());
    }
    result
}

/// Says what `text` would be taken for — `eva intent`'s default — without
/// pasting, opening or asking anything.
pub fn interpret_text(ctx: &WorkerContext, request_id: Uuid, text: &str) {
    let intent_json = match classify(ctx, text) {
        InterpretResult::Dictation => serde_json::json!({ "kind": "dictation" }),
        InterpretResult::Command(intent) => serde_json::to_value(intent).unwrap_or(serde_json::Value::Null),
    };
    ctx.events.emit(WorkerToShell::IntentRecognized { request_id, intent_json });
    ctx.events.emit(WorkerToShell::Ack { request_id });
}

/// Cleans `raw` in the style of the frontmost app, saves it to the corpus and
/// pastes it at the cursor. Shared by typed text (`eva intent`) and real
/// transcripts — both are, deliberately, the exact same downstream logic.
///
/// The clean-and-format pass runs on a blocking thread: when the formatter is
/// the Apple Intelligence bridge this can take real wall-clock time waiting
/// on the on-device model (bounded by its own internal timeout,
/// `docs/PLAN.md` §3.3 point 3).
async fn dictate(ctx: &Arc<WorkerContext>, request_id: Uuid, raw: &str) {
    let dictionary = Dictionary::new(ctx.store.list_custom_words().unwrap_or_default());
    let style = style_for(ctx, ctx.active_window().await.as_ref().and_then(|w| w.bundle_identifier.clone()).as_deref());

    let formatter = Arc::clone(&ctx.formatter);
    let raw_owned = raw.to_string();
    let cleaned =
        tokio::task::spawn_blocking(move || eva_text::clean_styled(&raw_owned, &dictionary, formatter.as_ref(), style))
            .await
            .unwrap_or_else(|join_error| {
                // Cannot happen in practice — `eva_text::clean_styled` never panics,
                // per the workspace's no-panic policy — but a `JoinError` here (the
                // runtime shutting down mid-call) must still degrade to the same
                // "never silent" guarantee as every other failure path.
                tracing::error!("la tarea de formateo terminó de forma inesperada: {join_error}");
                eva_text::clean_styled(raw, &Dictionary::new(Vec::<String>::new()), &RuleOnlyFormatter, style)
            });

    remember(ctx, request_id, &cleaned);
    ctx.events.emit(WorkerToShell::Transcript {
        request_id,
        raw: cleaned.raw.clone(),
        cleaned: cleaned.formatted.clone(),
    });

    if cleaned.formatted.trim().is_empty() {
        // Nothing worth pasting (the whole utterance was filler) — not an
        // error, just nothing to do.
        ctx.events.state(request_id, WorkerState::Done(true));
        return;
    }

    let trailing_space = ctx.config.dictation.trailing_space && style != Style::Terminal;
    let text = if trailing_space { format!("{} ", cleaned.formatted) } else { cleaned.formatted };
    deliver(ctx, request_id, text).await;
}

/// Saves what was dictated to the history (if the config keeps one) and holds
/// it for the flag hotkey. A dictation into a password field is neither: it
/// must leave no trace.
fn remember(ctx: &WorkerContext, request_id: Uuid, cleaned: &eva_text::CleanedTranscript) {
    if ctx.desktop.secure_input_active() {
        ctx.harvest.forget();
        return;
    }
    let transcript_id = if ctx.config.history.save_transcripts {
        ctx.store
            .save_transcript(&cleaned.raw, &cleaned.pre_formatted, &cleaned.formatted)
            .map_err(|e| tracing::warn!("no se pudo guardar el transcript en el historial: {e}"))
            .ok()
    } else {
        None
    };
    ctx.harvest.remember_dictation(request_id, transcript_id, &cleaned.raw, Some(&cleaned.formatted));
}

/// The style for the app with this bundle id: the user's own rules from
/// `[styles]`, then the built-in table.
fn style_for(ctx: &WorkerContext, bundle_id: Option<&str>) -> Style {
    let overrides: Vec<(String, Style)> = ctx
        .config
        .styles
        .apps
        .iter()
        .filter_map(|rule| Some((rule.bundle_id.clone(), Style::from_name(&rule.style)?)))
        .collect();
    Style::for_bundle_id(bundle_id, &overrides)
}

/// Pastes `text` at the cursor and ends the request, or — when macOS is
/// blocking synthesized keystrokes because a password field has focus — puts
/// it on the clipboard and says so, instead of silently losing it.
async fn deliver(ctx: &Arc<WorkerContext>, request_id: Uuid, text: String) {
    let desktop = Arc::clone(&ctx.desktop);
    let result = tokio::task::spawn_blocking(move || paste_or_copy(desktop.as_ref(), &text)).await;

    match result {
        Ok(Ok(Delivery::Pasted)) => ctx.events.state(request_id, WorkerState::Done(true)),
        Ok(Ok(Delivery::CopiedInstead)) => ctx.events.fail(
            request_id,
            "hay un campo de contraseña activo y macOS no deja pegar aquí; el texto quedó en el portapapeles",
        ),
        Ok(Err(e)) => ctx.events.fail(request_id, e.to_string()),
        Err(join_error) => ctx.events.fail(request_id, format!("el pegado se interrumpió: {join_error}")),
    }
}

enum Delivery {
    Pasted,
    CopiedInstead,
}

fn paste_or_copy(desktop: &dyn Desktop, text: &str) -> Result<Delivery, eva_mcp::DesktopError> {
    if desktop.secure_input_active() {
        desktop.copy_text(text)?;
        return Ok(Delivery::CopiedInstead);
    }
    desktop.insert_text(text).map(|()| Delivery::Pasted)
}

/// A custom command's `insert`: the user's own text, pasted through the
/// gateway like any other paste (and to the clipboard instead, in a password
/// field), with nothing added to it.
pub async fn insert_text(ctx: &Arc<WorkerContext>, request_id: Uuid, text: &str, intent_json: serde_json::Value) {
    let action = Action::new(ActionKind::InsertText, Origin::Voice, text).with_intent(intent_json);
    let ticket = match ctx.gateway.authorize(&action).await {
        Verdict::Allowed(ticket) => ticket,
        Verdict::Refused { reason } => {
            ctx.events.fail(request_id, reason);
            return;
        }
    };
    ctx.events.state(request_id, WorkerState::Executing);
    ctx.gateway.record_result(&ticket, "texto pegado");
    deliver(ctx, request_id, text.to_string()).await;
}

/// "Adán, hazlo más formal": rewrites the selected text following the spoken
/// instruction and pastes the result over the selection. Never touches the
/// text on any failure — no selection, no formatter that can rewrite, a
/// rewrite that does not look like an edit — it says what went wrong.
pub async fn edit_selection(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    instruction: String,
    intent_json: serde_json::Value,
) {
    let action =
        Action::new(ActionKind::EditSelection, Origin::Voice, instruction.as_str()).with_intent(intent_json.clone());
    let ticket = match ctx.gateway.authorize(&action).await {
        Verdict::Allowed(ticket) => ticket,
        Verdict::Refused { reason } => {
            ctx.events.fail(request_id, reason);
            return;
        }
    };
    ctx.events.state(request_id, WorkerState::Executing);

    let selection = match ctx.voice.scoped_to_intent(intent_json).selected_text().await {
        Ok(Some(text)) => text,
        Ok(None) => {
            ctx.gateway.record_result(&ticket, "sin texto seleccionado");
            ctx.events.fail(request_id, "no hay texto seleccionado que reescribir; selecciónalo y vuelve a pedirlo");
            return;
        }
        Err(e) => {
            ctx.gateway.record_result(&ticket, &format!("no se pudo leer la selección: {e}"));
            ctx.events.fail(request_id, e.to_string());
            return;
        }
    };

    let formatter = Arc::clone(&ctx.formatter);
    let (text, spoken) = (selection.clone(), instruction);
    let rewritten = tokio::task::spawn_blocking(move || formatter.rewrite(&text, &spoken)).await;

    let new_text = match rewritten {
        Ok(Ok(new_text)) => new_text,
        Ok(Err(e)) => {
            ctx.gateway.record_result(&ticket, &format!("reescritura fallida: {e}"));
            ctx.events.fail(request_id, explain_rewrite_failure(&e));
            return;
        }
        Err(join_error) => {
            ctx.events.fail(request_id, format!("la reescritura se interrumpió: {join_error}"));
            return;
        }
    };

    if new_text == selection {
        ctx.gateway.record_result(&ticket, "el texto no cambió");
        ctx.events.fail(request_id, "el texto ya estaba como lo pediste");
        return;
    }

    ctx.gateway.record_result(&ticket, "texto reescrito");
    deliver(ctx, request_id, new_text).await;
}

fn explain_rewrite_failure(error: &FormatError) -> String {
    match error {
        FormatError::Unavailable(_) => {
            "editar por voz necesita Apple Intelligence (o un modelo remoto configurado en [remote])".to_string()
        }
        FormatError::Timeout(after) => format!("el modelo no respondió en {after:?}; no toqué tu texto"),
        FormatError::InvalidOutput(_) => "el modelo no devolvió una edición confiable; no toqué tu texto".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::ShellToWorker;
    use eva_macos::RunningAppInfo;
    use eva_mcp::desktop::mock::{Call, MockDesktop};

    fn typed(rig: &Rig, text: &str) -> ShellToWorker {
        let _ = rig;
        ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: text.to_string() }
    }

    fn app(bundle_id: &str) -> RunningAppInfo {
        RunningAppInfo {
            localized_name: Some("App".into()),
            bundle_identifier: Some(bundle_id.into()),
            pid: 1,
            window_title: None,
        }
    }

    #[tokio::test]
    async fn plain_dictation_produces_a_cleaned_transcript_saves_it_and_pastes_it() {
        let mut rig = Rig::new();
        let events = rig.run(typed(&rig, "eh hola mundo")).await;

        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Transcript { cleaned, .. } if cleaned == "Hola mundo.")));
        assert_eq!(rig.ctx.store.recent_transcripts(10).expect("must succeed").len(), 1);
        // Plain dictation must actually reach the cursor, not just get logged
        // — and a space follows it so the next dictation does not glue on.
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Hola mundo. ".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn the_trailing_space_can_be_turned_off_in_the_config() {
        let mut rig = Rig::builder().configure(|c| c.dictation.trailing_space = false).build();
        rig.run(typed(&rig, "hola mundo")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Hola mundo.".to_string())]);
    }

    #[tokio::test]
    async fn dictating_into_a_terminal_is_words_only_with_no_trailing_space() {
        let desktop = MockDesktop::new().with_active_window(app("com.mitchellh.ghostty"));
        let mut rig = Rig::builder().desktop(desktop).build();
        rig.run(typed(&rig, "git status")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("git status".to_string())]);
    }

    #[tokio::test]
    async fn dictating_into_a_chat_drops_the_lone_final_period() {
        let desktop = MockDesktop::new().with_active_window(app("com.tinyspeck.slackmacgap"));
        let mut rig = Rig::builder().desktop(desktop).build();
        rig.run(typed(&rig, "voy para allá")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Voy para allá ".to_string())]);
    }

    #[tokio::test]
    async fn the_users_style_rule_overrides_the_built_in_table() {
        let desktop = MockDesktop::new().with_active_window(app("com.apple.mail"));
        let mut rig = Rig::builder()
            .desktop(desktop)
            .configure(|c| {
                c.styles
                    .apps
                    .push(eva_config::AppStyle { bundle_id: "com.apple.mail".into(), style: "terminal".into() })
            })
            .build();
        rig.run(typed(&rig, "hola")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("hola".to_string())]);
    }

    #[tokio::test]
    async fn a_password_field_gets_the_text_on_the_clipboard_and_an_explanation_not_silence() {
        let desktop = MockDesktop::new().with_secure_input();
        let mut rig = Rig::builder().desktop(desktop).build();
        let events = rig.run(typed(&rig, "hola mundo")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::CopyText("Hola mundo. ".to_string())], "must not try to paste");
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("portapapeles"))));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[tokio::test]
    async fn an_utterance_that_was_only_filler_pastes_nothing_and_is_not_an_error() {
        let mut rig = Rig::new();
        let events = rig.run(typed(&rig, "eh ehm mmm")).await;
        assert!(rig.desktop.calls().is_empty());
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn a_failing_paste_is_reported() {
        let mut rig = Rig::builder().desktop(MockDesktop::failing()).build();
        let events = rig.run(typed(&rig, "hola")).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[tokio::test]
    async fn adding_a_custom_word_actually_improves_later_dictation() {
        // The point of the whole feature, exercised end to end: a word
        // added via the IPC command must be picked up by the very next
        // dictation, not just sit in the store unused.
        let mut rig = Rig::new();
        rig.run(ShellToWorker::AddCustomWord { request_id: Uuid::new_v4(), word: "García".to_string() }).await;
        rig.run(typed(&rig, "hola Garcia")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Hola García. ".to_string())]);
    }

    // ---- edit mode ----

    #[tokio::test]
    async fn edit_mode_rewrites_the_selection_and_pastes_it_over_the_original() {
        let desktop = MockDesktop::new().with_selection("oye mándame eso");
        let mut rig = Rig::builder()
            .desktop(desktop)
            .formatter(crate::testkit::Rewriter::returning("Por favor, envíame eso."))
            .build();

        let events = rig.run(typed(&rig, "Adán, hazlo más formal")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![Call::SelectedText, Call::InsertText("Por favor, envíame eso.".to_string())]
        );
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn edit_mode_hands_the_model_the_selection_and_the_whole_spoken_instruction() {
        let desktop = MockDesktop::new().with_selection("hola a todos");
        let rewriter = crate::testkit::Rewriter::returning("Buenos días a todos.");
        let seen = rewriter.seen();
        let mut rig = Rig::builder().desktop(desktop).formatter(rewriter).build();

        rig.run(typed(&rig, "Adán, tradúcelo al inglés")).await;

        assert_eq!(seen.lock().unwrap().clone(), vec![("hola a todos".to_string(), "tradúcelo al inglés".to_string())]);
    }

    #[tokio::test]
    async fn edit_mode_with_nothing_selected_says_so_and_pastes_nothing() {
        let mut rig = Rig::builder().formatter(crate::testkit::Rewriter::returning("x")).build();
        let events = rig.run(typed(&rig, "Adán, hazlo más formal")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::SelectedText]);
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("seleccionado"))));
    }

    #[tokio::test]
    async fn edit_mode_without_a_model_that_can_rewrite_leaves_the_text_alone_and_explains() {
        let desktop = MockDesktop::new().with_selection("oye mándame eso");
        let mut rig = Rig::builder().desktop(desktop).build(); // RuleOnlyFormatter: cannot rewrite
        let events = rig.run(typed(&rig, "Adán, hazlo más formal")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::SelectedText], "the selection must not be touched");
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("Apple Intelligence"))));
    }

    #[tokio::test]
    async fn an_unchanged_rewrite_is_reported_not_pasted() {
        let desktop = MockDesktop::new().with_selection("ya está formal");
        let mut rig =
            Rig::builder().desktop(desktop).formatter(crate::testkit::Rewriter::returning("ya está formal")).build();
        let events = rig.run(typed(&rig, "Adán, hazlo más formal")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::SelectedText]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn edit_mode_is_audited_with_its_outcome() {
        let desktop = MockDesktop::new().with_selection("hola");
        let mut rig = Rig::builder().desktop(desktop).formatter(crate::testkit::Rewriter::returning("Hola.")).build();
        rig.run(typed(&rig, "Adán, hazlo más formal")).await;

        let audit = rig.ctx.store.recent_audit(10).expect("audit");
        let edit = audit.iter().find(|a| a.intent_json["action"] == "edit_selection").expect("an edit entry");
        assert_eq!(edit.result_summary.as_deref(), Some("texto reescrito"));
    }

    #[test]
    fn rewrite_failures_are_explained_in_words_the_user_can_act_on() {
        assert!(explain_rewrite_failure(&FormatError::Unavailable("x".into())).contains("Apple Intelligence"));
        assert!(explain_rewrite_failure(&FormatError::InvalidOutput("x".into())).contains("no toqué"));
        assert!(explain_rewrite_failure(&FormatError::Timeout(std::time::Duration::from_secs(6))).contains("no toqué"));
    }
}
