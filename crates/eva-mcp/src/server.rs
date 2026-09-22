//! The MCP tool router: exposes [`crate::desktop::Desktop`] actions as MCP
//! tools, per `docs/PLAN.md` fase 8. Every tool call goes through
//! [`Desktop`] — there is no `run_shell`-style escape hatch here, by design
//! (`docs/PLAN.md` fase 8: "para eso ya está el sandbox del propio agente").
//!
//! **Scope, stated plainly:** `docs/PLAN.md` fase 8 also lists
//! `get_selection`, `ask_user_confirmation`, and `list_projects`. Those need
//! pieces that do not exist yet — the Accessibility API text-selection
//! bridge (fase 9), a live round trip to a running `eva-shell` for the
//! confirmation UI, and the project-directory scanner (fase 6, deferred for
//! the MVP per `docs/PLAN.md` §10 decision 8) — so they are not implemented
//! here. Adding them is a small, mechanical extension of this same pattern
//! once those pieces exist; faking them now would mean either a
//! `ask_user_confirmation` that always says yes (dangerous) or a
//! `list_projects` that always returns nothing (misleading), neither of
//! which this project settles for over stating the gap outright.

use crate::desktop::Desktop;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// The MCP server itself: a thin router over an [`Arc<dyn Desktop>`].
#[derive(Clone)]
pub struct EvaMcpServer {
    desktop: Arc<dyn Desktop>,
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

#[tool_router]
impl EvaMcpServer {
    /// Builds a server over the given [`Desktop`] implementation.
    pub fn new(desktop: Arc<dyn Desktop>) -> Self {
        EvaMcpServer { desktop, tool_router: Self::tool_router() }
    }

    #[tool(description = "Abre (o trae al frente) una aplicación por su nombre.")]
    async fn open_app(&self, Parameters(args): Parameters<AppNameArgs>) -> Result<CallToolResult, McpError> {
        match self.desktop.open_app(&args.app_name) {
            Ok(()) => Ok(success(format!("Abierto: {}", args.app_name))),
            Err(e) => Ok(failure(e.to_string())),
        }
    }

    #[tool(description = "Cierra una aplicación en ejecución por su nombre.")]
    async fn close_app(&self, Parameters(args): Parameters<AppNameArgs>) -> Result<CallToolResult, McpError> {
        match self.desktop.close_app(&args.app_name) {
            Ok(()) => Ok(success(format!("Cerrado: {}", args.app_name))),
            Err(e) => Ok(failure(e.to_string())),
        }
    }

    #[tool(description = "Abre una URL en el manejador por defecto del sistema.")]
    async fn open_url(&self, Parameters(args): Parameters<UrlArgs>) -> Result<CallToolResult, McpError> {
        match self.desktop.open_url(&args.url) {
            Ok(()) => Ok(success(format!("Abierto: {}", args.url))),
            Err(e) => Ok(failure(e.to_string())),
        }
    }

    #[tool(description = "Pega texto en la posición actual del cursor, en la aplicación que tenga el foco.")]
    async fn insert_text(&self, Parameters(args): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        match self.desktop.insert_text(&args.text) {
            Ok(()) => Ok(success("Texto insertado.".to_string())),
            Err(e) => Ok(failure(e.to_string())),
        }
    }

    #[tool(description = "Devuelve el nombre y el identificador de paquete de la aplicación en primer plano.")]
    async fn get_active_window(&self) -> Result<CallToolResult, McpError> {
        match self.desktop.active_window() {
            Some(info) => {
                let name = info.localized_name.unwrap_or_else(|| "(sin nombre)".to_string());
                let bundle = info.bundle_identifier.unwrap_or_else(|| "(desconocido)".to_string());
                Ok(success(format!("{name} ({bundle})")))
            }
            None => Ok(success("No se pudo determinar la aplicación en primer plano.".to_string())),
        }
    }

    #[tool(description = "Muestra una notificación del sistema con un título y un cuerpo.")]
    async fn notify(&self, Parameters(args): Parameters<NotifyArgs>) -> Result<CallToolResult, McpError> {
        match self.desktop.notify(&args.title, &args.body) {
            Ok(()) => Ok(success("Notificación mostrada.".to_string())),
            Err(e) => Ok(failure(e.to_string())),
        }
    }

    #[tool(description = "Dice un texto en voz alta, en español.")]
    async fn speak(&self, Parameters(args): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        match self.desktop.speak(&args.text) {
            Ok(()) => Ok(success("Reproduciendo.".to_string())),
            Err(e) => Ok(failure(e.to_string())),
        }
    }
}

fn success(message: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message)])
}

fn failure(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

#[tool_handler]
impl ServerHandler for EvaMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Herramientas de escritorio de EVA01: abrir/cerrar apps, abrir URLs, pegar texto, \
                 ver la app en primer plano, notificar y hablar. Todas pasan por el gateway local; \
                 no hay una herramienta de shell genérica a propósito."
                    .to_string(),
            )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::desktop::mock::{Call, MockDesktop};
    use eva_macos::RunningAppInfo;

    fn server_with(desktop: MockDesktop) -> EvaMcpServer {
        EvaMcpServer::new(Arc::new(desktop))
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[tokio::test]
    async fn open_app_calls_the_desktop_and_reports_success() {
        let desktop = MockDesktop::new();
        let server = server_with(desktop);

        let result = server
            .open_app(Parameters(AppNameArgs { app_name: "Brave Browser".to_string() }))
            .await
            .expect("tool call must not error at the transport level");

        assert!(!result.is_error.unwrap_or(false));
        assert!(text_of(&result).contains("Brave Browser"));
    }

    #[tokio::test]
    async fn a_failing_desktop_produces_a_call_tool_error_not_a_transport_error() {
        let server = server_with(MockDesktop::failing());

        let result = server
            .open_app(Parameters(AppNameArgs { app_name: "Cualquiera".to_string() }))
            .await
            .expect("the tool call itself must succeed even though the action failed");

        assert!(result.is_error.unwrap_or(false));
    }

    #[tokio::test]
    async fn insert_text_forwards_the_exact_text_to_the_desktop() {
        let desktop = MockDesktop::new();
        let server = EvaMcpServer::new(Arc::new(desktop));

        server
            .insert_text(Parameters(TextArgs { text: "Hola, mundo.".to_string() }))
            .await
            .expect("must not transport-error");

        // The mock was moved into the server; recreate the pattern with a
        // shared handle so the calls can be inspected after the fact.
        let desktop = Arc::new(MockDesktop::new());
        let server = EvaMcpServer::new(desktop.clone());
        server
            .insert_text(Parameters(TextArgs { text: "Hola, mundo.".to_string() }))
            .await
            .expect("must not transport-error");
        assert_eq!(desktop.calls(), vec![Call::InsertText("Hola, mundo.".to_string())]);
    }

    #[tokio::test]
    async fn get_active_window_reports_the_mocked_window() {
        let desktop = MockDesktop::new().with_active_window(RunningAppInfo {
            localized_name: Some("Visual Studio Code".to_string()),
            bundle_identifier: Some("com.microsoft.VSCode".to_string()),
        });
        let server = server_with(desktop);

        let result = server.get_active_window().await.expect("must not transport-error");
        let text = text_of(&result);
        assert!(text.contains("Visual Studio Code"));
        assert!(text.contains("com.microsoft.VSCode"));
    }

    #[tokio::test]
    async fn get_active_window_with_no_frontmost_app_still_succeeds() {
        let server = server_with(MockDesktop::new());
        let result = server.get_active_window().await.expect("must not transport-error");
        assert!(!result.is_error.unwrap_or(false));
    }

    #[tokio::test]
    async fn notify_and_speak_forward_their_arguments() {
        let desktop = Arc::new(MockDesktop::new());
        let server = EvaMcpServer::new(desktop.clone());

        server
            .notify(Parameters(NotifyArgs { title: "EVA01".to_string(), body: "listo".to_string() }))
            .await
            .expect("must not transport-error");
        server
            .speak(Parameters(TextArgs { text: "Codex terminó.".to_string() }))
            .await
            .expect("must not transport-error");

        assert_eq!(
            desktop.calls(),
            vec![
                Call::Notify("EVA01".to_string(), "listo".to_string()),
                Call::Speak("Codex terminó.".to_string()),
            ]
        );
    }

    #[test]
    fn get_info_advertises_tools_and_no_resources_or_prompts() {
        let server = server_with(MockDesktop::new());
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_none());
        assert!(info.capabilities.prompts.is_none());
    }
}
