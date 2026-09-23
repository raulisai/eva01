//! The wire protocol between an agent's MCP server (`eva-mcp`, a child of the
//! agent CLI) and the running `eva-worker`, over a Unix socket, one JSON
//! object per line (`docs/PLAN.md` fase 8: "todas las herramientas pasan por
//! el gateway"). The MCP server holds no privileges of its own — it forwards
//! every tool call here, and the worker rules on it (policy, confirmation on
//! the overlay, audit) and performs it. Kept in this crate, next to the
//! shell↔worker protocol, so both ends of the socket share one definition.

use serde::{Deserialize, Serialize};

/// One request line: a per-run secret plus the operation.
///
/// The token is what stops another process running as the same user from
/// speaking to the socket: it is generated when the worker starts and handed
/// only to the agents the worker launches (in the MCP server's environment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCall {
    /// The worker's per-run secret.
    pub token: String,
    /// What to do.
    #[serde(flatten)]
    pub op: DesktopOp,
}

/// Every operation an agent can ask of the desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DesktopOp {
    /// Open or focus an application.
    OpenApp {
        /// The application's name.
        name: String,
    },
    /// Quit an application.
    CloseApp {
        /// The application's name.
        name: String,
    },
    /// Open a URL.
    OpenUrl {
        /// The URL.
        url: String,
    },
    /// Paste text at the cursor.
    InsertText {
        /// The text.
        text: String,
    },
    /// The frontmost application and its window.
    ActiveWindow,
    /// Show a notification.
    Notify {
        /// The title.
        title: String,
        /// The body.
        body: String,
    },
    /// Speak text aloud.
    Speak {
        /// The text.
        text: String,
    },
    /// The text currently selected in the frontmost app.
    SelectedText,
    /// Ask the user a yes/no question on the overlay.
    AskConfirmation {
        /// The question.
        question: String,
        /// A second line of context.
        detail: String,
    },
    /// The projects EVA knows about.
    ListProjects,
}

/// One reply line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum DesktopReply {
    /// The operation ran and has nothing to return.
    Done,
    /// The frontmost window, if there is one.
    Window {
        /// What is in front.
        window: Option<WindowInfo>,
    },
    /// Text, or `None` when there was none (nothing selected).
    Text {
        /// The text.
        text: Option<String>,
    },
    /// The user's answer to a confirmation.
    Confirmed {
        /// `true` only for an explicit yes.
        approved: bool,
    },
    /// The known projects.
    Projects {
        /// Every project.
        projects: Vec<ProjectEntry>,
    },
    /// The gateway refused the operation.
    Refused {
        /// Why, in words fit for the agent and the user.
        reason: String,
    },
    /// The operation was allowed and failed.
    Failed {
        /// What went wrong.
        message: String,
    },
}

/// The frontmost application and its focused window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// The app's display name.
    pub name: Option<String>,
    /// The app's bundle identifier.
    pub bundle_id: Option<String>,
    /// The app's process id.
    pub pid: i32,
    /// The focused window's title, when readable.
    pub title: Option<String>,
}

/// A project EVA knows about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// The folder's name.
    pub name: String,
    /// The folder's absolute path.
    pub path: String,
    /// Whether it is the project a voice task would run in right now.
    pub active: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::{decode_line, encode_line};

    #[test]
    fn every_operation_round_trips_with_its_token() {
        let ops = vec![
            DesktopOp::OpenApp { name: "Brave Browser".into() },
            DesktopOp::CloseApp { name: "Spotify".into() },
            DesktopOp::OpenUrl { url: "https://github.com".into() },
            DesktopOp::InsertText { text: "hola\nmundo".into() },
            DesktopOp::ActiveWindow,
            DesktopOp::Notify { title: "Listo".into(), body: "3 archivos".into() },
            DesktopOp::Speak { text: "terminé".into() },
            DesktopOp::SelectedText,
            DesktopOp::AskConfirmation { question: "¿Borrar la rama?".into(), detail: "eva/ab12".into() },
            DesktopOp::ListProjects,
        ];
        for op in ops {
            let call = DesktopCall { token: "secreto".into(), op };
            let line = encode_line(&call).expect("encodes");
            assert_eq!(decode_line::<DesktopCall>(&line).expect("decodes"), call, "{line}");
        }
    }

    #[test]
    fn the_wire_shape_is_flat_so_other_clients_are_easy_to_write() {
        let call = DesktopCall { token: "t".into(), op: DesktopOp::OpenApp { name: "Safari".into() } };
        let json: serde_json::Value = serde_json::from_str(&encode_line(&call).expect("encodes")).expect("json");
        assert_eq!(json, serde_json::json!({"token": "t", "op": "open_app", "name": "Safari"}));
    }

    #[test]
    fn every_reply_round_trips() {
        let replies = vec![
            DesktopReply::Done,
            DesktopReply::Window {
                window: Some(WindowInfo {
                    name: Some("Visual Studio Code".into()),
                    bundle_id: Some("com.microsoft.VSCode".into()),
                    pid: 4242,
                    title: Some("main.rs — eva01".into()),
                }),
            },
            DesktopReply::Window { window: None },
            DesktopReply::Text { text: Some("seleccionado".into()) },
            DesktopReply::Text { text: None },
            DesktopReply::Confirmed { approved: false },
            DesktopReply::Projects {
                projects: vec![ProjectEntry { name: "eva01".into(), path: "/code/eva01".into(), active: true }],
            },
            DesktopReply::Refused { reason: "no lo hago".into() },
            DesktopReply::Failed { message: "no existe".into() },
        ];
        for reply in replies {
            let line = encode_line(&reply).expect("encodes");
            assert_eq!(decode_line::<DesktopReply>(&line).expect("decodes"), reply, "{line}");
        }
    }

    #[test]
    fn an_unknown_operation_is_a_decode_error_not_a_panic() {
        let result = decode_line::<DesktopCall>(r#"{"token":"t","op":"run_shell","command":"rm -rf ~"}"#);
        assert!(result.is_err(), "there is no run_shell operation, by design (docs/PLAN.md fase 8)");
    }
}
