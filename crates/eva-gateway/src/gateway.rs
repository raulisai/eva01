//! The ruling itself: policy + floor → allow, ask, or refuse; audit either way.

use crate::action::Action;
use crate::confirmer::Confirmer;
use crate::rules::safety_floor;
use eva_config::{GatewayConfig, Policy};
use eva_store::{Decision, Store};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Proof an action was ruled on, carrying its audit entry so the outcome can
/// be recorded against it afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    audit_id: Option<Uuid>,
}

/// The gateway's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Go ahead.
    Allowed(Ticket),
    /// Do not — with why, in words fit for the user.
    Refused {
        /// Why it was refused.
        reason: String,
    },
}

/// Why [`Gateway::run`] did not produce a value.
#[derive(Debug, PartialEq, Eq)]
pub enum GatewayError<E> {
    /// The gateway refused the action; it never ran.
    Refused(String),
    /// The action was allowed and ran, and itself failed.
    Failed(E),
}

impl<E: fmt::Display> fmt::Display for GatewayError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::Refused(reason) => write!(f, "{reason}"),
            GatewayError::Failed(error) => write!(f, "{error}"),
        }
    }
}

/// Rules on actions. Cheap to clone-share behind an `Arc`.
pub struct Gateway {
    config: GatewayConfig,
    store: Store,
    confirmer: Arc<dyn Confirmer>,
}

impl Gateway {
    /// Builds a gateway over a policy config, the audit store, and whatever
    /// asks the user when a policy says to.
    pub fn new(config: GatewayConfig, store: Store, confirmer: Arc<dyn Confirmer>) -> Gateway {
        Gateway { config, store, confirmer }
    }

    /// Rules on `action`. The stricter of the configured policy and the
    /// built-in safety floor applies; `Confirm` waits for the user (up to the
    /// configured timeout, after which it counts as "no"). Every outcome is
    /// written to the audit trail.
    pub async fn authorize(&self, action: &Action) -> Verdict {
        let configured = self.config.policy(action.origin, action.kind);
        let (floor, floor_reason) = safety_floor(action);
        let policy = configured.max(floor);

        match policy {
            Policy::Auto => Verdict::Allowed(Ticket { audit_id: self.log(action, Decision::AutoApproved) }),
            Policy::Block => {
                let reason = match (floor, floor_reason) {
                    (Policy::Block, Some(why)) => format!("no lo hago: {why}"),
                    _ => format!("no lo hago: «{}» está bloqueado en tu configuración", action.kind.name()),
                };
                let audit_id = self.log(action, Decision::Blocked);
                self.record(audit_id, &reason);
                Verdict::Refused { reason }
            }
            Policy::Confirm => {
                let detail = match floor_reason {
                    Some(why) if floor == Policy::Confirm => format!("{}: {why}", capitalize(action.origin_label())),
                    _ => capitalize(action.origin_label()),
                };
                let timeout = Duration::from_secs(self.config.confirm_timeout_secs());
                if self.confirmer.confirm(&action.describe(), &detail, timeout).await {
                    Verdict::Allowed(Ticket { audit_id: self.log(action, Decision::UserConfirmed) })
                } else {
                    let reason = "no confirmaste, así que no lo hice".to_string();
                    let audit_id = self.log(action, Decision::UserRejected);
                    self.record(audit_id, &reason);
                    Verdict::Refused { reason }
                }
            }
        }
    }

    /// Writes what came of an allowed action into its audit entry.
    pub fn record_result(&self, ticket: &Ticket, summary: &str) {
        self.record(ticket.audit_id, summary);
    }

    /// Rules on `action` and, if allowed, runs `run` and records how it
    /// went — the whole life of one action in one call.
    ///
    /// # Errors
    /// [`GatewayError::Refused`] if the gateway said no (`run` never
    /// called); [`GatewayError::Failed`] if `run` itself failed.
    pub async fn run<T, E: fmt::Display>(
        &self,
        action: &Action,
        run: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, GatewayError<E>> {
        match self.authorize(action).await {
            Verdict::Refused { reason } => Err(GatewayError::Refused(reason)),
            Verdict::Allowed(ticket) => match run() {
                Ok(value) => {
                    self.record_result(&ticket, &format!("{}: ok", action.kind.name()));
                    Ok(value)
                }
                Err(e) => {
                    self.record_result(&ticket, &format!("{}: error: {e}", action.kind.name()));
                    Err(GatewayError::Failed(e))
                }
            },
        }
    }

    fn log(&self, action: &Action, decision: Decision) -> Option<Uuid> {
        match self.store.log_decision(None, &action.audit_json(), decision) {
            Ok(id) => Some(id),
            Err(e) => {
                // A full disk must not turn into "the user's command did
                // nothing": the audit trail is important, but the ruling
                // stands without it.
                tracing::warn!("no se pudo escribir la auditoría: {e}");
                None
            }
        }
    }

    fn record(&self, audit_id: Option<Uuid>, summary: &str) {
        if let Some(id) = audit_id {
            if let Err(e) = self.store.record_audit_result(id, summary) {
                tracing::warn!("no se pudo completar la auditoría: {e}");
            }
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::confirmer::mock::Scripted;
    use eva_config::{ActionKind, Origin};

    fn gateway(config: &str, confirmer: Arc<Scripted>) -> (Gateway, Store) {
        let store = Store::open_in_memory().expect("store");
        let config: GatewayConfig = toml::from_str(config).expect("valid config");
        (Gateway::new(config, store.clone(), confirmer), store)
    }

    fn open_app(origin: Origin) -> Action {
        Action::new(ActionKind::OpenApp, origin, "Brave Browser")
    }

    #[tokio::test]
    async fn an_auto_action_runs_without_asking_and_is_audited() {
        let confirmer = Arc::new(Scripted::denying());
        let (gateway, store) = gateway("", confirmer.clone());

        let result: Result<u32, GatewayError<String>> = gateway.run(&open_app(Origin::Voice), || Ok(7)).await;

        assert_eq!(result, Ok(7));
        assert!(confirmer.questions().is_empty(), "auto must never bother the user");
        let audit = store.recent_audit(10).expect("audit");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, Decision::AutoApproved);
        assert_eq!(audit[0].result_summary.as_deref(), Some("open_app: ok"));
    }

    #[tokio::test]
    async fn a_confirm_policy_asks_and_runs_only_on_yes() {
        let confirmer = Arc::new(Scripted::approving());
        let (gateway, store) = gateway("", confirmer.clone());
        let action = Action::new(ActionKind::CloseApp, Origin::Agent, "Spotify");

        let result: Result<(), GatewayError<String>> = gateway.run(&action, || Ok(())).await;

        assert_eq!(result, Ok(()));
        let questions = confirmer.questions();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].0, "Cerrar la aplicación Spotify");
        assert_eq!(questions[0].1, "Lo pide un agente");
        assert_eq!(store.recent_audit(1).expect("audit")[0].decision, Decision::UserConfirmed);
    }

    #[tokio::test]
    async fn a_declined_confirmation_never_runs_the_action() {
        let confirmer = Arc::new(Scripted::denying());
        let (gateway, store) = gateway("", confirmer);
        let action = Action::new(ActionKind::CloseApp, Origin::Agent, "Spotify");
        let mut ran = false;

        let result: Result<(), GatewayError<String>> = gateway
            .run(&action, || {
                ran = true;
                Ok(())
            })
            .await;

        assert!(matches!(result, Err(GatewayError::Refused(_))));
        assert!(!ran, "a refused action must not run");
        let audit = &store.recent_audit(1).expect("audit")[0];
        assert_eq!(audit.decision, Decision::UserRejected);
    }

    #[tokio::test]
    async fn a_blocked_policy_refuses_without_asking() {
        let confirmer = Arc::new(Scripted::approving());
        let (gateway, store) = gateway("[voice]\nopen_app = \"block\"", confirmer.clone());

        let result: Result<(), GatewayError<String>> = gateway.run(&open_app(Origin::Voice), || Ok(())).await;

        assert!(matches!(result, Err(GatewayError::Refused(reason)) if reason.contains("open_app")));
        assert!(confirmer.questions().is_empty());
        assert_eq!(store.recent_audit(1).expect("audit")[0].decision, Decision::Blocked);
    }

    #[tokio::test]
    async fn the_config_cannot_loosen_a_safety_floor() {
        // The user configured everything to `auto`; a file:// link still
        // gets asked about and quitting Finder still gets refused.
        let confirmer = Arc::new(Scripted::denying());
        let (gateway, _store) =
            gateway("[voice]\nopen_url = \"auto\"\nclose_app = \"auto\"", confirmer.clone());

        let link = Action::new(ActionKind::OpenUrl, Origin::Voice, "file:///etc/hosts");
        let result: Result<(), GatewayError<String>> = gateway.run(&link, || Ok(())).await;
        assert!(matches!(result, Err(GatewayError::Refused(_))));
        assert_eq!(confirmer.questions().len(), 1, "the floor turned auto into a question");

        let finder = Action::new(ActionKind::CloseApp, Origin::Voice, "Finder");
        let result: Result<(), GatewayError<String>> = gateway.run(&finder, || Ok(())).await;
        match result {
            Err(GatewayError::Refused(reason)) => assert!(reason.contains("escritorio")),
            other => panic!("Finder must be refused, got {other:?}"),
        }
        assert_eq!(confirmer.questions().len(), 1, "a blocked action is not even asked about");
    }

    #[tokio::test]
    async fn the_confirmation_says_why_a_floor_asked() {
        let confirmer = Arc::new(Scripted::approving());
        let (gateway, _store) = gateway("", confirmer.clone());
        let link = Action::new(ActionKind::OpenUrl, Origin::Voice, "smb://servidor/x");

        let _: Result<(), GatewayError<String>> = gateway.run(&link, || Ok(())).await;

        let (_, detail) = &confirmer.questions()[0];
        assert!(detail.contains("no es un enlace web"), "{detail}");
    }

    #[tokio::test]
    async fn an_allowed_action_that_fails_reports_the_failure_and_audits_it() {
        let (gateway, store) = gateway("", Arc::new(Scripted::denying()));

        let result: Result<(), GatewayError<String>> =
            gateway.run(&open_app(Origin::Voice), || Err("la app no existe".to_string())).await;

        assert_eq!(result, Err(GatewayError::Failed("la app no existe".to_string())));
        let summary = store.recent_audit(1).expect("audit")[0].result_summary.clone().expect("summary");
        assert!(summary.contains("la app no existe"), "{summary}");
    }

    #[tokio::test]
    async fn a_confirm_timeout_from_the_config_reaches_the_confirmer() {
        struct RecordsTimeout(std::sync::Mutex<Option<Duration>>);
        #[async_trait::async_trait]
        impl Confirmer for RecordsTimeout {
            async fn confirm(&self, _t: &str, _d: &str, timeout: Duration) -> bool {
                *self.0.lock().unwrap() = Some(timeout);
                true
            }
        }
        let confirmer = Arc::new(RecordsTimeout(std::sync::Mutex::new(None)));
        let store = Store::open_in_memory().expect("store");
        let config: GatewayConfig = toml::from_str("confirm_timeout_secs = 7").expect("valid");
        let gateway = Gateway::new(config, store, confirmer.clone());

        let action = Action::new(ActionKind::InsertText, Origin::Agent, "hola");
        assert!(matches!(gateway.authorize(&action).await, Verdict::Allowed(_)));
        assert_eq!(*confirmer.0.lock().unwrap(), Some(Duration::from_secs(7)));
    }
}
