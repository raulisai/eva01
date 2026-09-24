//! Running a recognized voice command. Every action goes through the gateway
//! (`ctx.voice`), so the same policy, confirmation and audit that rule an
//! agent's tool calls rule the user's own words.

use crate::context::WorkerContext;
use eva_intent::Intent;
use eva_ipc::{WorkerState, WorkerToShell};
use eva_mcp::{DesktopService, Outcome, ServiceError};
use eva_store::Decision;
use std::sync::Arc;
use uuid::Uuid;

/// Runs `intent`, reporting through `ctx.events`.
pub async fn run_intent(ctx: &Arc<WorkerContext>, request_id: Uuid, intent: Intent) {
    let intent_json = serde_json::to_value(&intent).unwrap_or(serde_json::Value::Null);
    ctx.events.emit(WorkerToShell::IntentRecognized { request_id, intent_json: intent_json.clone() });

    let voice = ctx.voice.scoped_to_intent(intent_json.clone());
    match intent {
        // `interpret` returns a `Command` only after the wake word matched,
        // so this cannot come out of the normal path — logged and treated
        // as a no-op rather than guessed at.
        Intent::Dictation => {
            tracing::warn!("Intent::Dictation llegó a run_intent; esto no debería pasar, se ignora");
            ctx.events.state(request_id, WorkerState::Idle);
        }
        Intent::Blocked { matched_stem, text } => {
            tracing::warn!(%matched_stem, %text, "comando bloqueado por la lista negra de verbos destructivos");
            match ctx.store.log_decision(None, &intent_json, Decision::Blocked) {
                Ok(audit_id) => {
                    let _ = ctx.store.record_audit_result(audit_id, "bloqueado: verbo destructivo detectado");
                }
                Err(e) => tracing::warn!("no se pudo escribir la auditoría del bloqueo: {e}"),
            }
            ctx.events.fail(request_id, format!("no ejecuto eso: contiene el verbo bloqueado \"{matched_stem}\""));
        }
        Intent::OpenApp { app } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.open_app(&app).await);
        }
        Intent::CloseApp { app } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.close_app(&app).await);
        }
        Intent::OpenUrl { url } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.open_url(&url).await);
        }
        Intent::WebSearch { query } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.web_search(&query).await);
        }
        Intent::AppNotFound { name, opening } => app_not_found(ctx, request_id, &name, opening, &voice).await,
        Intent::Custom { phrase } => run_custom(ctx, request_id, &phrase, intent_json).await,
        Intent::AgentTask { prompt, provider } => {
            crate::tasks::start_new(ctx, request_id, intent_json, prompt, provider).await;
        }
        Intent::ContinueAgentTask { extra_prompt } => {
            crate::tasks::continue_last(ctx, request_id, intent_json, extra_prompt).await;
        }
        Intent::EditSelection { instruction } => {
            crate::dictation::edit_selection(ctx, request_id, instruction, intent_json).await;
        }
    }
}

/// "Abre Photoshop" without Photoshop: says so, and — for opening — offers to
/// look for it in the App Store. The offer is a question on the overlay
/// answered with a key, like any other confirmation; only a yes opens the
/// App Store, and it opens on the search, not on anything to buy.
async fn app_not_found(ctx: &WorkerContext, request_id: Uuid, name: &str, opening: bool, voice: &impl DesktopService) {
    if !opening {
        ctx.events.fail(request_id, format!("no encuentro «{name}» entre tus aplicaciones"));
        return;
    }
    ctx.events.state(request_id, WorkerState::Executing);
    let wants_store = voice
        .ask_confirmation(&format!("«{name}» no está instalada"), "¿La busco en la App Store para instalarla?")
        .await
        .unwrap_or(false);
    if !wants_store {
        ctx.events.fail(request_id, format!("«{name}» no está instalada"));
        return;
    }
    report(ctx, request_id, voice.open_url(&eva_mcp::app_store_search_url(name)).await);
}

/// Runs one of the user's own `[[commands]]`. Each thing it does goes through
/// the gateway, so `[gateway.voice]` rules it like any other command.
async fn run_custom(ctx: &Arc<WorkerContext>, request_id: Uuid, phrase: &str, intent_json: serde_json::Value) {
    use eva_config::CommandAction;

    let Some(command) = ctx.config.custom_commands().find(|c| c.phrases().any(|say| say == phrase)) else {
        ctx.events.fail(request_id, format!("la orden «{phrase}» ya no está en la configuración"));
        return;
    };
    match command.action() {
        Ok(CommandAction::Insert(text)) => crate::dictation::insert_text(ctx, request_id, text, intent_json).await,
        Ok(CommandAction::Task(prompt)) => {
            crate::tasks::start_new(ctx, request_id, intent_json, prompt.to_string(), None).await;
        }
        Ok(CommandAction::Open(targets)) => {
            ctx.events.state(request_id, WorkerState::Executing);
            let voice = ctx.voice.scoped_to_intent(intent_json);
            let mut problems = Vec::new();
            for target in targets {
                if let Err(problem) = open_target(ctx, &voice, target).await {
                    problems.push(problem);
                }
            }
            if problems.is_empty() {
                ctx.events.state(request_id, WorkerState::Done(true));
            } else {
                ctx.events.fail(request_id, problems.join("; "));
            }
        }
        Err(why) => ctx.events.fail(request_id, format!("la orden «{phrase}» no se puede ejecutar: {why}")),
    }
}

/// Opens one app or address the way "Adán, abre …" would — same grammar, same
/// app aliases, same URL detection — so a custom command cannot understand
/// a name differently from a spoken one.
async fn open_target(ctx: &WorkerContext, voice: &impl DesktopService, target: &str) -> Result<(), String> {
    let outcome = match eva_intent::intent::parse(&format!("abre {target}"), &ctx.app_index.current()) {
        Intent::OpenApp { app } => voice.open_app(&app).await,
        Intent::OpenUrl { url } => voice.open_url(&url).await,
        _ => return Err(format!("no encontré «{target}» entre tus apps ni parece una dirección web")),
    };
    outcome.map_err(|e| match e {
        ServiceError::Refused(message) | ServiceError::Failed(message) => message,
    })
}

/// Turns the outcome of a gated action into the request's final events.
fn report(ctx: &WorkerContext, request_id: Uuid, outcome: Outcome<()>) {
    match outcome {
        Ok(()) => ctx.events.state(request_id, WorkerState::Done(true)),
        Err(ServiceError::Refused(message) | ServiceError::Failed(message)) => ctx.events.fail(request_id, message),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::ShellToWorker;
    use eva_mcp::desktop::mock::{Call, MockDesktop};

    fn typed(text: &str) -> ShellToWorker {
        ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: text.to_string() }
    }

    /// A rig whose config has the user's own commands, written as in `config.toml`.
    fn with_commands(toml: &str) -> crate::testkit::RigBuilder {
        let config = eva_config::Config::parse(toml).expect("a valid config");
        Rig::builder().configure(move |c| c.commands = config.commands.clone())
    }

    #[tokio::test]
    async fn a_custom_insert_pastes_exactly_the_users_text() {
        let mut rig = with_commands("[[commands]]\nsay = \"mi correo\"\ninsert = \"yo@ejemplo.com\"").build();
        let events = rig.run(typed("Adán, mi correo.")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![Call::InsertText("yo@ejemplo.com".to_string())],
            "no trailing space, no formatting"
        );
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        let audit = rig.ctx.store.recent_audit(10).unwrap();
        assert_eq!(audit[0].intent_json["intent"]["kind"], "custom", "the audit trail says it was a custom command");
        assert_eq!(audit[0].intent_json["intent"]["phrase"], "mi correo");
    }

    #[tokio::test]
    async fn a_custom_open_opens_apps_and_addresses_in_order_with_the_same_names_as_speech() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"modo enfoque\"\nopen = [\"brave\", \"https://ejemplo.com/agenda\"]")
                .build();
        let events = rig.run(typed("adan modo enfoque")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![Call::OpenApp("Brave Browser".to_string()), Call::OpenUrl("https://ejemplo.com/agenda".to_string())]
        );
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn a_custom_open_with_an_unknown_app_does_the_rest_and_says_which_one_failed() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"modo enfoque\"\nopen = [\"Aplicación Que No Existe\", \"brave\"]")
                .build();
        let events = rig.run(typed("Adán, modo enfoque")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())], "the others still open");
        assert!(
            events.iter().any(
                |e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("Aplicación Que No Existe"))
            ),
            "{events:?}"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[tokio::test]
    async fn the_gateway_policy_rules_custom_commands_too() {
        let mut rig = with_commands("[[commands]]\nsay = \"mi correo\"\ninsert = \"yo@ejemplo.com\"")
            .configure(|c| {
                c.gateway.voice.insert(eva_config::ActionKind::InsertText, eva_config::Policy::Block);
            })
            .build();
        let events = rig.run(typed("Adán, mi correo")).await;

        assert!(rig.desktop.calls().is_empty(), "a blocked action never reaches the desktop");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn a_custom_task_goes_to_the_agent_with_exactly_the_written_prompt() {
        use crate::testkit::registry_of;
        let codex = std::sync::Arc::new(eva_agents::mock::MockProvider::always_completes("codex", "listo"));
        let mut rig =
            with_commands("[[commands]]\nsay = \"revisa los tests\"\ntask = \"corre los tests y dime cuáles fallan\"")
                .agents(registry_of(&[&codex]))
                .build();
        let events = rig.run(typed("Adán, revisa los tests")).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::TaskStarted { prompt, .. } if prompt == "corre los tests y dime cuáles fallan")), "{events:?}");
    }

    #[tokio::test]
    async fn interpreting_a_text_says_what_it_is_and_does_none_of_it() {
        let mut rig = with_commands("[[commands]]\nsay = \"mi correo\"\ninsert = \"yo@ejemplo.com\"").build();
        let interpret =
            |text: &str| ShellToWorker::InterpretText { request_id: Uuid::new_v4(), text: text.to_string() };
        let kind = |events: &[WorkerToShell]| {
            events.iter().find_map(|e| match e {
                WorkerToShell::IntentRecognized { intent_json, .. } => {
                    Some(intent_json["kind"].as_str().unwrap().to_string())
                }
                _ => None,
            })
        };

        let events = rig.run(interpret("Adán, abre brave")).await;
        assert_eq!(kind(&events).as_deref(), Some("open_app"));
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Ack { .. })), "the request ends");

        assert_eq!(kind(&rig.run(interpret("Adán, mi correo")).await).as_deref(), Some("custom"));
        assert_eq!(kind(&rig.run(interpret("hola mundo")).await).as_deref(), Some("dictation"));
        assert_eq!(kind(&rig.run(interpret("Adán, borra el proyecto")).await).as_deref(), Some("blocked"));

        assert!(rig.desktop.calls().is_empty(), "nothing was opened, pasted or asked");
        assert!(rig.ctx.store.recent_audit(10).unwrap().is_empty(), "and the gateway never heard of it");
    }

    #[tokio::test]
    async fn a_broken_custom_command_never_runs_and_the_words_fall_back_to_the_normal_rules() {
        // No action at all: skipped, so "abre brave" is the ordinary command.
        let mut rig = with_commands("[[commands]]\nsay = \"abre brave\"").build();
        rig.run(typed("Adán, abre brave")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
    }

    #[tokio::test]
    async fn an_app_that_is_not_installed_says_so_and_offers_the_app_store_only_on_a_yes() {
        let mut rig = Rig::new();
        let declined = rig.run_answering(typed("Adán, abre Photoshop"), false).await;

        assert!(declined.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("Photoshop") && title.contains("no está instalada"))), "{declined:?}");
        assert!(declined
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("no está instalada"))));
        assert!(rig.desktop.calls().is_empty(), "nothing opens unless they say yes");

        let accepted = rig.run_answering(typed("Adán, abre Photoshop"), true).await;
        assert!(accepted
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        assert_eq!(
            rig.desktop.calls(),
            vec![Call::OpenUrl(
                "macappstore://search.itunes.apple.com/WebObjects/MZSearch.woa/wa/search?q=Photoshop&media=software"
                    .to_string()
            )]
        );
    }

    #[tokio::test]
    async fn an_app_installed_after_startup_is_found_without_restarting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let installed = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&installed);
        let catalog = crate::apps::AppCatalog::new(move || {
            let mut apps = vec![eva_intent::AppEntry::new("Brave Browser")];
            if flag.load(Ordering::SeqCst) {
                apps.push(eva_intent::AppEntry::new("Spotify"));
            }
            eva_intent::AppIndex::new(apps)
        });
        let mut rig = Rig::builder().apps(catalog).build();

        installed.store(true, Ordering::SeqCst); // they installed it from the App Store
        rig.ctx.app_index.allow_rescan_now();
        let events = rig.run(typed("Adán, abre Spotify")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Spotify".to_string())]);
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
    }

    #[tokio::test]
    async fn closing_an_app_that_is_not_there_just_says_so() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, cierra Photoshop")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("Photoshop"))));
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
    }

    #[tokio::test]
    async fn open_app_command_calls_the_desktop_and_logs_audit() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, abre brave")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        let audit = rig.ctx.store.recent_audit(10).expect("must succeed");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, Decision::AutoApproved);
        assert_eq!(audit[0].intent_json["intent"]["kind"], "open_app", "the voice intent is kept in the audit trail");
    }

    #[tokio::test]
    async fn destructive_command_is_blocked_and_never_reaches_the_desktop() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, borra el proyecto")).await;

        assert!(rig.desktop.calls().is_empty(), "a blocked command must never call the desktop");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        let audit = rig.ctx.store.recent_audit(10).expect("must succeed");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, Decision::Blocked);
    }

    #[tokio::test]
    async fn web_search_opens_a_google_url_with_the_query_encoded() {
        let mut rig = Rig::new();
        rig.run(typed("Adán, busca clima hoy")).await;

        let calls = rig.desktop.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], Call::OpenUrl(url) if url.contains("google.com/search") && url.contains("clima")));
    }

    #[tokio::test]
    async fn spoken_addresses_open_exactly_the_url_the_gateway_ruled_on() {
        let mut rig = Rig::new();
        rig.run(typed("Adán, abre localhost tres mil.")).await;
        rig.run(typed("Adán, abre google punto com punto mx")).await;
        rig.run(typed("Adán, abre github.com.")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![
                Call::OpenUrl("http://localhost:3000".to_string()),
                Call::OpenUrl("https://google.com.mx".to_string()),
                Call::OpenUrl("https://github.com".to_string()),
            ]
        );
        let audited: Vec<String> = rig
            .ctx
            .store
            .recent_audit(10)
            .unwrap()
            .into_iter()
            .map(|a| a.intent_json["subject"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(audited.contains(&"http://localhost:3000".to_string()), "the audit names what opened: {audited:?}");
    }

    #[tokio::test]
    async fn a_desktop_failure_ends_the_request_as_failed_with_the_reason() {
        let mut rig = Rig::builder().desktop(MockDesktop::failing()).build();
        let events = rig.run(typed("Adán, abre brave")).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[tokio::test]
    async fn a_link_that_is_not_web_asks_first_even_by_voice_and_runs_only_on_yes() {
        let mut rig = Rig::new();
        let events = rig.run_answering(typed("Adán, abre file:///etc/hosts"), true).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("file:///etc/hosts"))), "{events:?}");
        assert_eq!(rig.desktop.calls(), vec![Call::OpenUrl("file:///etc/hosts".to_string())]);
    }

    #[tokio::test]
    async fn declining_that_confirmation_runs_nothing_and_says_why() {
        let mut rig = Rig::new();
        let events = rig.run_answering(typed("Adán, abre file:///etc/hosts"), false).await;

        assert!(rig.desktop.calls().is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("no confirmaste"))));
        assert_eq!(rig.ctx.store.recent_audit(1).expect("audit")[0].decision, Decision::UserRejected);
    }

    #[tokio::test]
    async fn a_link_that_runs_code_is_refused_without_asking() {
        let mut rig = Rig::new();
        let events = rig.run_answering(typed("Adán, abre javascript://x%0Aalert(1)"), true).await;
        assert!(rig.desktop.calls().is_empty());
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
    }

    #[tokio::test]
    async fn the_configured_policy_can_make_a_voice_action_ask_first() {
        let mut rig = Rig::builder()
            .configure(|c| {
                c.gateway.voice.insert(eva_config::ActionKind::OpenApp, eva_config::Policy::Confirm);
            })
            .build();
        let events = rig.run_answering(typed("Adán, abre brave"), true).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
    }
}
