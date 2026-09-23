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
