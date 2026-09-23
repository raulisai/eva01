//! The operations an agent (or, through the same code, the user's voice) can
//! ask of the desktop, as one async trait — and [`LocalService`], the
//! implementation that runs them *through the gateway*: policy, confirmation
//! on the overlay, audit, and only then the real action. `docs/PLAN.md`
//! fase 8: "todas las herramientas pasan por el gateway".
//!
//! Two implementations exist. [`LocalService`] does the work in-process
//! (`eva-worker`, and the standalone `eva-mcp`). [`crate::remote::RemoteService`]
//! forwards each call over a socket to a running `eva-worker`'s
//! `LocalService`, which is how an agent's `eva-mcp` child reaches the
//! overlay and the worker's audit trail.

use crate::desktop::Desktop;
use async_trait::async_trait;
use eva_config::{ActionKind, Origin};
use eva_gateway::{Action, Confirmer, Gateway, Verdict};
use eva_ipc::rpc::{ProjectEntry, WindowInfo};
use std::sync::Arc;
use std::time::Duration;

/// Why an operation did not produce a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// The gateway refused it: blocked by policy, or the user said no (or
    /// never answered). The reason is fit to show.
    Refused(String),
    /// It was allowed and did not work.
    Failed(String),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::Refused(text) | ServiceError::Failed(text) => write!(f, "{text}"),
        }
    }
}

/// The result of one operation.
pub type Outcome<T> = Result<T, ServiceError>;

/// Everything an agent can ask of the desktop.
#[async_trait]
pub trait DesktopService: Send + Sync {
    /// Opens or focuses an application.
    async fn open_app(&self, name: &str) -> Outcome<()>;
    /// Quits an application.
    async fn close_app(&self, name: &str) -> Outcome<()>;
    /// Opens a URL.
    async fn open_url(&self, url: &str) -> Outcome<()>;
    /// Pastes text at the cursor.
    async fn insert_text(&self, text: &str) -> Outcome<()>;
    /// The frontmost application and its focused window.
    async fn active_window(&self) -> Outcome<Option<WindowInfo>>;
    /// Shows a notification.
    async fn notify(&self, title: &str, body: &str) -> Outcome<()>;
    /// Speaks text aloud.
    async fn speak(&self, text: &str) -> Outcome<()>;
    /// The text selected in the frontmost app.
    async fn selected_text(&self) -> Outcome<Option<String>>;
    /// Asks the user a yes/no question.
    async fn ask_confirmation(&self, question: &str, detail: &str) -> Outcome<bool>;
    /// The projects EVA knows about.
    async fn list_projects(&self) -> Outcome<Vec<ProjectEntry>>;
}

/// Where the list of projects comes from. The worker knows which one is
/// active (from the focused window); a standalone `eva-mcp` only knows the
/// configured roots.
pub trait ProjectSource: Send + Sync {
    /// Every known project, with the active one marked.
    fn list(&self, active_window_title: Option<&str>) -> Vec<ProjectEntry>;
}

/// [`DesktopService`] that runs each operation here, after the gateway has
/// ruled on it.
#[derive(Clone)]
pub struct LocalService {
    origin: Origin,
    gateway: Arc<Gateway>,
    desktop: Arc<dyn Desktop>,
    confirmer: Arc<dyn Confirmer>,
    projects: Arc<dyn ProjectSource>,
    confirm_timeout: Duration,
    intent: Option<serde_json::Value>,
}

impl LocalService {
    /// A service acting on behalf of `origin` (the voice, or an agent).
    pub fn new(
        origin: Origin,
        gateway: Arc<Gateway>,
        desktop: Arc<dyn Desktop>,
        confirmer: Arc<dyn Confirmer>,
        projects: Arc<dyn ProjectSource>,
        confirm_timeout: Duration,
    ) -> LocalService {
        LocalService { origin, gateway, desktop, confirmer, projects, confirm_timeout, intent: None }
    }

    /// The same service, with `intent` (the parsed voice command) attached to
    /// every audit entry it writes.
    #[must_use]
    pub fn scoped_to_intent(&self, intent: serde_json::Value) -> LocalService {
        LocalService { intent: Some(intent), ..self.clone() }
    }

    /// Rules on `action`; if allowed, runs `op` on a blocking thread (the
    /// AppKit and clipboard calls are synchronous, some sleep) and records
    /// how it went.
    async fn gated<T: Send + 'static>(
        &self,
        kind: ActionKind,
        subject: &str,
        op: impl FnOnce(&dyn Desktop) -> Result<T, crate::DesktopError> + Send + 'static,
    ) -> Outcome<T> {
        let mut action = Action::new(kind, self.origin, subject);
        if let Some(intent) = &self.intent {
            action = action.with_intent(intent.clone());
        }

        let ticket = match self.gateway.authorize(&action).await {
            Verdict::Allowed(ticket) => ticket,
            Verdict::Refused { reason } => return Err(ServiceError::Refused(reason)),
        };

        let desktop = Arc::clone(&self.desktop);
        let result = tokio::task::spawn_blocking(move || op(desktop.as_ref())).await;
        match result {
            Ok(Ok(value)) => {
                self.gateway.record_result(&ticket, &format!("{}: ok", kind.name()));
                Ok(value)
            }
            Ok(Err(e)) => {
                self.gateway.record_result(&ticket, &format!("{}: error: {e}", kind.name()));
                Err(ServiceError::Failed(e.to_string()))
            }
            Err(join_error) => {
                let message = format!("la acción se interrumpió: {join_error}");
                self.gateway.record_result(&ticket, &format!("{}: error: {message}", kind.name()));
                Err(ServiceError::Failed(message))
            }
        }
    }

    /// A web search, by voice ("Adán, busca gatos"). Not an MCP tool — an
    /// agent has `open_url` — but a distinct action for the gateway, so the
    /// policy can treat "search the web" differently from "open this link".
    pub async fn web_search(&self, query: &str) -> Outcome<()> {
        let url = format!("https://www.google.com/search?q={}", percent_encode(query));
        self.gated(ActionKind::WebSearch, query, move |d| d.open_url(&url)).await
    }

    fn window_info(app: eva_macos::RunningAppInfo) -> WindowInfo {
        WindowInfo { name: app.localized_name, bundle_id: app.bundle_identifier, pid: app.pid, title: app.window_title }
    }
}

#[async_trait]
impl DesktopService for LocalService {
    async fn open_app(&self, name: &str) -> Outcome<()> {
        let name = name.to_string();
        self.gated(ActionKind::OpenApp, &name.clone(), move |d| d.open_app(&name)).await
    }

    async fn close_app(&self, name: &str) -> Outcome<()> {
        let name = name.to_string();
        self.gated(ActionKind::CloseApp, &name.clone(), move |d| d.close_app(&name)).await
    }

    async fn open_url(&self, url: &str) -> Outcome<()> {
        let url = url.to_string();
        self.gated(ActionKind::OpenUrl, &url.clone(), move |d| d.open_url(&url)).await
    }

    async fn insert_text(&self, text: &str) -> Outcome<()> {
        let text = text.to_string();
        self.gated(ActionKind::InsertText, &text.clone(), move |d| d.insert_text(&text)).await
    }

    async fn active_window(&self) -> Outcome<Option<WindowInfo>> {
        // Read-only and not sensitive enough to gate: it names the app in
        // front, not what is in it.
        let desktop = Arc::clone(&self.desktop);
        tokio::task::spawn_blocking(move || desktop.active_window().map(Self::window_info))
            .await
            .map_err(|e| ServiceError::Failed(format!("la consulta se interrumpió: {e}")))
    }

    async fn notify(&self, title: &str, body: &str) -> Outcome<()> {
        let (title, body) = (title.to_string(), body.to_string());
        self.gated(ActionKind::Notify, &title.clone(), move |d| d.notify(&title, &body)).await
    }

    async fn speak(&self, text: &str) -> Outcome<()> {
        let text = text.to_string();
        self.gated(ActionKind::Speak, &text.clone(), move |d| d.speak(&text)).await
    }

    async fn selected_text(&self) -> Outcome<Option<String>> {
        self.gated(ActionKind::ReadSelection, "", |d| d.selected_text()).await
    }

    async fn ask_confirmation(&self, question: &str, detail: &str) -> Outcome<bool> {
        // This *is* the human gate, so it is not itself gated.
        Ok(self.confirmer.confirm(question, detail, self.confirm_timeout).await)
    }

    async fn list_projects(&self) -> Outcome<Vec<ProjectEntry>> {
        let title = self.active_window().await?.and_then(|w| w.title);
        Ok(self.projects.list(title.as_deref()))
    }
}

/// Percent-encodes a search query for a URL: everything but unreserved
/// characters becomes `%XX` per UTF-8 byte.
fn percent_encode(input: &str) -> String {
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
    use crate::desktop::mock::{Call, MockDesktop};
    use eva_config::GatewayConfig;
    use eva_gateway::mock::Scripted;
    use eva_macos::RunningAppInfo;
    use eva_store::{Decision, Store};

    struct FixedProjects(Vec<ProjectEntry>);
    impl ProjectSource for FixedProjects {
        fn list(&self, _title: Option<&str>) -> Vec<ProjectEntry> {
            self.0.clone()
        }
    }

    struct Rig {
        service: LocalService,
        desktop: Arc<MockDesktop>,
        confirmer: Arc<Scripted>,
        store: Store,
    }

    fn rig(origin: Origin, desktop: MockDesktop, confirmer: Scripted, config: &str) -> Rig {
        let desktop = Arc::new(desktop);
        let confirmer = Arc::new(confirmer);
        let store = Store::open_in_memory().expect("store");
        let gateway = Arc::new(Gateway::new(
            toml::from_str::<GatewayConfig>(config).expect("valid config"),
            store.clone(),
            confirmer.clone(),
        ));
        let projects = Arc::new(FixedProjects(vec![ProjectEntry {
            name: "eva01".into(),
            path: "/code/eva01".into(),
            active: true,
        }]));
        let service =
            LocalService::new(origin, gateway, desktop.clone(), confirmer.clone(), projects, Duration::from_secs(5));
        Rig { service, desktop, confirmer, store }
    }

    #[tokio::test]
    async fn a_voice_open_app_runs_immediately_and_is_audited() {
        let rig = rig(Origin::Voice, MockDesktop::new(), Scripted::denying(), "");
        rig.service.open_app("Brave Browser").await.expect("allowed");

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".into())]);
        let audit = rig.store.recent_audit(5).expect("audit");
        assert_eq!(audit[0].decision, Decision::AutoApproved);
        assert_eq!(audit[0].result_summary.as_deref(), Some("open_app: ok"));
    }

    #[tokio::test]
    async fn an_agent_closing_an_app_is_asked_first_and_only_runs_on_yes() {
        let rig = rig(Origin::Agent, MockDesktop::new(), Scripted::approving(), "");
        rig.service.close_app("Spotify").await.expect("confirmed");
        assert_eq!(rig.desktop.calls(), vec![Call::CloseApp("Spotify".into())]);
        assert_eq!(rig.confirmer.questions().len(), 1);
    }

    #[tokio::test]
    async fn a_refused_action_never_reaches_the_desktop() {
        let rig = rig(Origin::Agent, MockDesktop::new(), Scripted::denying(), "");
        let result = rig.service.close_app("Spotify").await;
        assert!(matches!(result, Err(ServiceError::Refused(_))));
        assert!(rig.desktop.calls().is_empty());
    }

    #[tokio::test]
    async fn an_agent_pasting_a_multiline_text_is_asked_even_if_the_config_says_auto() {
        let rig = rig(Origin::Agent, MockDesktop::new(), Scripted::denying(), "[agent]\ninsert_text = \"auto\"");
        let result = rig.service.insert_text("ls\nrm -rf ~").await;
        assert!(matches!(result, Err(ServiceError::Refused(_))));
        assert!(rig.desktop.calls().is_empty(), "the safety floor outranks the config");
    }

    #[tokio::test]
    async fn a_desktop_failure_is_reported_as_failed_and_audited() {
        let rig = rig(Origin::Voice, MockDesktop::failing(), Scripted::denying(), "");
        let result = rig.service.open_app("Brave Browser").await;
        assert!(matches!(result, Err(ServiceError::Failed(_))));
        let summary = rig.store.recent_audit(1).expect("audit")[0].result_summary.clone().expect("summary");
        assert!(summary.contains("error"), "{summary}");
    }

    #[tokio::test]
    async fn reading_the_selection_by_an_agent_needs_confirmation() {
        let desktop = MockDesktop::new().with_selection("texto privado");
        let rig = rig(Origin::Agent, desktop, Scripted::approving(), "");
        assert_eq!(rig.service.selected_text().await.expect("confirmed"), Some("texto privado".into()));
        let question = &rig.confirmer.questions()[0];
        assert!(question.0.contains("seleccionado"), "{question:?}");
    }

    #[tokio::test]
    async fn the_users_own_selection_read_is_not_gated() {
        let desktop = MockDesktop::new().with_selection("mi texto");
        let rig = rig(Origin::Voice, desktop, Scripted::denying(), "");
        assert_eq!(rig.service.selected_text().await.expect("auto"), Some("mi texto".into()));
        assert!(rig.confirmer.questions().is_empty());
    }

    #[tokio::test]
    async fn the_active_window_reports_the_app_and_its_title() {
        let desktop = MockDesktop::new().with_active_window(RunningAppInfo {
            localized_name: Some("Visual Studio Code".into()),
            bundle_identifier: Some("com.microsoft.VSCode".into()),
            pid: 7,
            window_title: Some("main.rs — eva01".into()),
        });
        let rig = rig(Origin::Agent, desktop, Scripted::denying(), "");
        let window = rig.service.active_window().await.expect("read-only").expect("a window");
        assert_eq!(window.bundle_id.as_deref(), Some("com.microsoft.VSCode"));
        assert_eq!(window.title.as_deref(), Some("main.rs — eva01"));
    }

    #[tokio::test]
    async fn ask_confirmation_relays_the_users_answer_and_is_not_itself_gated() {
        let rig = rig(Origin::Agent, MockDesktop::new(), Scripted::new(vec![true, false]), "");
        assert!(rig.service.ask_confirmation("¿Continuar?", "detalle").await.expect("asks"));
        assert!(!rig.service.ask_confirmation("¿Otra vez?", "").await.expect("asks"));
        assert_eq!(rig.confirmer.questions()[0], ("¿Continuar?".to_string(), "detalle".to_string()));
    }

    #[tokio::test]
    async fn list_projects_comes_from_the_project_source() {
        let rig = rig(Origin::Agent, MockDesktop::new(), Scripted::denying(), "");
        let projects = rig.service.list_projects().await.expect("ok");
        assert_eq!(projects[0].name, "eva01");
    }

    #[tokio::test]
    async fn a_web_search_opens_google_with_the_query_encoded() {
        let rig = rig(Origin::Voice, MockDesktop::new(), Scripted::denying(), "");
        rig.service.web_search("clima hoy en méxico").await.expect("allowed");
        let calls = rig.desktop.calls();
        assert!(
            matches!(&calls[0], Call::OpenUrl(url)
            if url == "https://www.google.com/search?q=clima%20hoy%20en%20m%C3%A9xico"),
            "{calls:?}"
        );
        assert_eq!(rig.store.recent_audit(1).expect("audit")[0].intent_json["action"], "web_search");
    }

    #[test]
    fn percent_encoding_keeps_unreserved_characters_and_encodes_the_rest() {
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
        assert_eq!(percent_encode("café"), "caf%C3%A9");
    }

    #[tokio::test]
    async fn a_scoped_service_records_the_voice_intent_in_the_audit_trail() {
        let rig = rig(Origin::Voice, MockDesktop::new(), Scripted::denying(), "");
        let scoped = rig.service.scoped_to_intent(serde_json::json!({"kind": "open_app", "app": "Brave Browser"}));
        scoped.open_app("Brave Browser").await.expect("allowed");
        let audit = rig.store.recent_audit(1).expect("audit");
        assert_eq!(audit[0].intent_json["intent"]["kind"], "open_app");
    }
}
