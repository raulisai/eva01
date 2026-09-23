//! The MCP tool router: exposes [`DesktopService`] operations as MCP tools,
//! per `docs/PLAN.md` fase 8. Every tool call goes through the service —
//! and so through the gateway — there is no `run_shell`-style escape hatch
//! here, by design (`docs/PLAN.md` fase 8: "para eso ya está el sandbox del
//! propio agente"). `ask_user_confirmation` is the only path to the human;
//! everything sensitive that an agent does without asking is decided by the
//! gateway's policy, not by the agent's own judgment.

use crate::service::{DesktopService, Outcome, ServiceError};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// The MCP server itself: a thin router over an [`Arc<dyn DesktopService>`].
#[derive(Clone)]
pub struct EvaMcpServer {
    service: Arc<dyn DesktopService>,
    // Read by the code `#[tool_handler]` generates on the `ServerHandler`
    // impl below (list_tools/call_tool dispatch through it) — the dead-code
    // lint's own note explains why it cannot see that usage through the
    // derived `Clone` impl and the macro expansion.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AppNameArgs {
    /// The application's name, e.g. "Brave Browser" or "Visual Studio Code".
    app_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UrlArgs {
    /// The URL to open.
    url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TextArgs {
    /// The text to use.
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NotifyArgs {
    /// The notification's title.
    title: String,
    /// The notification's body.
    body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ConfirmationArgs {
    /// The yes/no question to show the user, e.g. "¿Borro la rama eva/ab12?".
    question: String,
    /// Optional context shown under the question.
    #[serde(default)]
    detail: String,
}

#[tool_router]
impl EvaMcpServer {
    /// Builds a server over the given [`DesktopService`].
    pub fn new(service: Arc<dyn DesktopService>) -> Self {
        EvaMcpServer { service, tool_router: Self::tool_router() }
    }

    #[tool(description = "Abre (o trae al frente) una aplicación por su nombre.")]
    async fn open_app(&self, Parameters(args): Parameters<AppNameArgs>) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.open_app(&args.app_name).await.map(|()| format!("Abierto: {}", args.app_name))))
    }

    #[tool(description = "Cierra una aplicación en ejecución por su nombre. Pide confirmación al usuario.")]
    async fn close_app(&self, Parameters(args): Parameters<AppNameArgs>) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.close_app(&args.app_name).await.map(|()| format!("Cerrado: {}", args.app_name))))
    }

    #[tool(
        description = "Abre una URL en el manejador por defecto del sistema. Los enlaces que no son web piden confirmación."
    )]
    async fn open_url(&self, Parameters(args): Parameters<UrlArgs>) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.open_url(&args.url).await.map(|()| format!("Abierto: {}", args.url))))
    }

    #[tool(
        description = "Pega texto en la posición actual del cursor, en la aplicación que tenga el foco. Pide confirmación al usuario."
    )]
    async fn insert_text(&self, Parameters(args): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.insert_text(&args.text).await.map(|()| "Texto insertado.".to_string())))
    }

    #[tool(
        description = "Devuelve la aplicación en primer plano: nombre, identificador de paquete y título de su ventana."
    )]
    async fn get_active_window(&self) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.active_window().await.map(|window| match window {
            Some(w) => format!(
                "{} ({}){}",
                w.name.unwrap_or_else(|| "(sin nombre)".to_string()),
                w.bundle_id.unwrap_or_else(|| "(desconocido)".to_string()),
                w.title.map(|t| format!(" — «{t}»")).unwrap_or_default(),
            ),
            None => "No se pudo determinar la aplicación en primer plano.".to_string(),
        })))
    }

    #[tool(
        description = "Devuelve el texto que el usuario tiene seleccionado en la aplicación en primer plano. Pide confirmación al usuario."
    )]
    async fn get_selection(&self) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.selected_text().await.map(|text| match text {
            Some(text) => text,
            None => "No hay texto seleccionado.".to_string(),
        })))
    }

    #[tool(description = "Muestra una notificación del sistema con un título y un cuerpo.")]
    async fn notify(&self, Parameters(args): Parameters<NotifyArgs>) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.notify(&args.title, &args.body).await.map(|()| "Notificación mostrada.".to_string())))
    }

    #[tool(description = "Dice un texto en voz alta, en español.")]
    async fn speak(&self, Parameters(args): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.speak(&args.text).await.map(|()| "Reproduciendo.".to_string())))
    }

    #[tool(
        description = "Hace una pregunta de sí o no al usuario en la pantalla y espera su respuesta (con un clic o una tecla, nunca por voz). Úsala antes de cualquier acción destructiva o irreversible."
    )]
    async fn ask_user_confirmation(
        &self,
        Parameters(args): Parameters<ConfirmationArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.ask_confirmation(&args.question, &args.detail).await.map(|approved| {
            if approved {
                "El usuario dijo que sí.".to_string()
            } else {
                "El usuario dijo que no (o no respondió).".to_string()
            }
        })))
    }

    #[tool(
        description = "Lista los proyectos que EVA conoce (repositorios git), marcando el activo: el que el usuario tiene a la vista."
    )]
    async fn list_projects(&self) -> Result<CallToolResult, McpError> {
        Ok(reply(self.service.list_projects().await.map(|projects| {
            if projects.is_empty() {
                return "No hay proyectos conocidos.".to_string();
            }
            projects
                .iter()
                .map(|p| format!("{}{} — {}", if p.active { "* " } else { "  " }, p.name, p.path))
                .collect::<Vec<_>>()
                .join("\n")
        })))
    }
}

/// A successful tool result, or a tool-level error the agent can read and
/// react to — a refusal or a failed action is an answer, not a transport
/// error.
fn reply(outcome: Outcome<String>) -> CallToolResult {
    match outcome {
        Ok(message) => CallToolResult::success(vec![ContentBlock::text(message)]),
        Err(ServiceError::Refused(reason)) => {
            CallToolResult::error(vec![ContentBlock::text(format!("Rechazado: {reason}"))])
        }
        Err(ServiceError::Failed(message)) => CallToolResult::error(vec![ContentBlock::text(message)]),
    }
}

#[tool_handler]
impl ServerHandler for EvaMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Herramientas de escritorio de EVA01: abrir/cerrar apps, abrir URLs, pegar texto, \
                 leer la selección, ver la app en primer plano, listar proyectos, notificar, hablar y \
                 preguntarle algo al usuario. Todas pasan por el gateway local, que puede pedir \
                 confirmación al usuario o rechazar la acción; no hay una herramienta de shell \
                 genérica a propósito."
                    .to_string(),
            )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::desktop::mock::{Call, MockDesktop};
    use crate::service::{LocalService, ProjectSource};
    use eva_config::{GatewayConfig, Origin};
    use eva_gateway::mock::Scripted;
    use eva_gateway::Gateway;
    use eva_ipc::rpc::ProjectEntry;
    use eva_macos::RunningAppInfo;
    use eva_store::Store;
    use std::time::Duration;

    struct OneProject;
    impl ProjectSource for OneProject {
        fn list(&self, _title: Option<&str>) -> Vec<ProjectEntry> {
            vec![ProjectEntry { name: "eva01".into(), path: "/code/eva01".into(), active: true }]
        }
    }

    fn server(desktop: Arc<MockDesktop>, confirmer: Arc<Scripted>) -> EvaMcpServer {
        let gateway = Arc::new(Gateway::new(
            GatewayConfig::default(),
            Store::open_in_memory().expect("store"),
            confirmer.clone(),
        ));
        let service =
            LocalService::new(Origin::Agent, gateway, desktop, confirmer, Arc::new(OneProject), Duration::from_secs(5));
        EvaMcpServer::new(Arc::new(service))
    }

    fn plain() -> (EvaMcpServer, Arc<MockDesktop>, Arc<Scripted>) {
        let desktop = Arc::new(MockDesktop::new());
        let confirmer = Arc::new(Scripted::denying());
        (server(desktop.clone(), confirmer.clone()), desktop, confirmer)
    }

    fn text_of(result: &CallToolResult) -> String {
        result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join(" ")
    }

    fn is_error(result: &CallToolResult) -> bool {
        result.is_error.unwrap_or(false)
    }

    #[tokio::test]
    async fn open_app_calls_the_desktop_and_reports_success() {
        let (server, desktop, _) = plain();
        let result = server
            .open_app(Parameters(AppNameArgs { app_name: "Brave Browser".to_string() }))
            .await
            .expect("tool call must not error at the transport level");
        assert!(!is_error(&result));
        assert!(text_of(&result).contains("Brave Browser"));
        assert_eq!(desktop.calls(), vec![Call::OpenApp("Brave Browser".into())]);
    }

    #[tokio::test]
    async fn a_failing_desktop_produces_a_call_tool_error_not_a_transport_error() {
        let server = server(Arc::new(MockDesktop::failing()), Arc::new(Scripted::denying()));
        let result = server
            .open_app(Parameters(AppNameArgs { app_name: "Cualquiera".to_string() }))
            .await
            .expect("the tool call itself must succeed even though the action failed");
        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn a_costly_action_the_user_declines_is_a_readable_refusal_and_never_runs() {
        let (server, desktop, confirmer) = plain();
        let result = server
            .close_app(Parameters(AppNameArgs { app_name: "Spotify".to_string() }))
            .await
            .expect("must not transport-error");
        assert!(is_error(&result));
        assert!(text_of(&result).starts_with("Rechazado:"), "{}", text_of(&result));
        assert!(desktop.calls().is_empty());
        assert_eq!(confirmer.questions().len(), 1);
    }

    #[tokio::test]
    async fn insert_text_asks_first_and_forwards_the_exact_text_once_confirmed() {
        let desktop = Arc::new(MockDesktop::new());
        let server = server(desktop.clone(), Arc::new(Scripted::approving()));
        server.insert_text(Parameters(TextArgs { text: "Hola, mundo.".to_string() })).await.expect("ok");
        assert_eq!(desktop.calls(), vec![Call::InsertText("Hola, mundo.".to_string())]);
    }

    #[tokio::test]
    async fn get_active_window_reports_the_app_and_its_title() {
        let desktop = Arc::new(MockDesktop::new().with_active_window(RunningAppInfo {
            localized_name: Some("Visual Studio Code".to_string()),
            bundle_identifier: Some("com.microsoft.VSCode".to_string()),
            pid: 9,
            window_title: Some("main.rs — eva01".to_string()),
        }));
        let server = server(desktop, Arc::new(Scripted::denying()));
        let text = text_of(&server.get_active_window().await.expect("ok"));
        assert!(text.contains("Visual Studio Code"));
        assert!(text.contains("com.microsoft.VSCode"));
        assert!(text.contains("main.rs — eva01"));
    }

    #[tokio::test]
    async fn get_active_window_with_no_frontmost_app_still_succeeds() {
        let (server, _, _) = plain();
        assert!(!is_error(&server.get_active_window().await.expect("ok")));
    }

    #[tokio::test]
    async fn get_selection_needs_the_users_yes_and_returns_the_text() {
        let desktop = Arc::new(MockDesktop::new().with_selection("texto elegido"));
        let server = server(desktop, Arc::new(Scripted::approving()));
        assert_eq!(text_of(&server.get_selection().await.expect("ok")), "texto elegido");
    }

    #[tokio::test]
    async fn get_selection_is_refused_when_the_user_says_no() {
        let desktop = Arc::new(MockDesktop::new().with_selection("secreto"));
        let server = server(desktop, Arc::new(Scripted::denying()));
        let result = server.get_selection().await.expect("ok");
        assert!(is_error(&result));
        assert!(!text_of(&result).contains("secreto"), "a refused read must not leak the text");
    }

    #[tokio::test]
    async fn notify_and_speak_forward_their_arguments() {
        let (server, desktop, _) = plain();
        server
            .notify(Parameters(NotifyArgs { title: "EVA01".to_string(), body: "listo".to_string() }))
            .await
            .expect("ok");
        server.speak(Parameters(TextArgs { text: "Codex terminó.".to_string() })).await.expect("ok");
        assert_eq!(
            desktop.calls(),
            vec![Call::Notify("EVA01".to_string(), "listo".to_string()), Call::Speak("Codex terminó.".to_string())]
        );
    }

    #[tokio::test]
    async fn ask_user_confirmation_relays_the_answer_in_words_the_agent_can_act_on() {
        let confirmer = Arc::new(Scripted::new(vec![true, false]));
        let server = server(Arc::new(MockDesktop::new()), confirmer);
        let args = || Parameters(ConfirmationArgs { question: "¿Borro la rama?".to_string(), detail: String::new() });
        assert!(text_of(&server.ask_user_confirmation(args()).await.expect("ok")).contains("sí"));
        assert!(text_of(&server.ask_user_confirmation(args()).await.expect("ok")).contains("no"));
    }

    #[tokio::test]
    async fn list_projects_marks_the_active_one() {
        let (server, _, _) = plain();
        let text = text_of(&server.list_projects().await.expect("ok"));
        assert_eq!(text, "* eva01 — /code/eva01");
    }

    #[test]
    fn there_are_exactly_the_ten_planned_tools_and_no_shell() {
        let (server, _, _) = plain();
        let mut names: Vec<String> = server.tool_router.list_all().iter().map(|t| t.name.to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "ask_user_confirmation",
                "close_app",
                "get_active_window",
                "get_selection",
                "insert_text",
                "list_projects",
                "notify",
                "open_app",
                "open_url",
                "speak",
            ]
        );
    }

    #[test]
    fn get_info_advertises_tools_and_no_resources_or_prompts() {
        let (server, _, _) = plain();
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_none());
        assert!(info.capabilities.prompts.is_none());
    }
}
