//! What the gateway rules on.

use eva_config::{ActionKind, Origin};

/// One thing EVA is about to do.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// What kind of action it is.
    pub kind: ActionKind,
    /// Who asked for it.
    pub origin: Origin,
    /// What it acts on: an app name, a URL, the text to paste…
    pub subject: String,
    /// The parsed voice intent this came from, kept in the audit trail.
    pub intent: Option<serde_json::Value>,
}

impl Action {
    /// A new action with no originating intent.
    pub fn new(kind: ActionKind, origin: Origin, subject: impl Into<String>) -> Action {
        Action { kind, origin, subject: subject.into(), intent: None }
    }

    /// Attaches the voice intent this action came from.
    #[must_use]
    pub fn with_intent(mut self, intent: serde_json::Value) -> Action {
        self.intent = Some(intent);
        self
    }

    /// A one-line description for a confirmation prompt, in Spanish, like
    /// everything the user reads.
    pub fn describe(&self) -> String {
        let subject = preview(&self.subject, 80);
        match self.kind {
            ActionKind::OpenApp => format!("Abrir la aplicación {subject}"),
            ActionKind::CloseApp => format!("Cerrar la aplicación {subject}"),
            ActionKind::OpenUrl => format!("Abrir {subject}"),
            ActionKind::WebSearch => format!("Buscar «{subject}»"),
            ActionKind::InsertText => format!("Escribir «{subject}» donde está el cursor"),
            ActionKind::AgentTask => format!("Encargar al agente: {subject}"),
            ActionKind::EditSelection => format!("Reescribir el texto seleccionado: {subject}"),
            ActionKind::ReadSelection => "Leer el texto que tienes seleccionado".to_string(),
            ActionKind::Notify => format!("Mostrar la notificación «{subject}»"),
            ActionKind::Speak => format!("Decir en voz alta «{subject}»"),
        }
    }

    /// Who asked, in words, for the confirmation prompt's detail line.
    pub fn origin_label(&self) -> &'static str {
        match self.origin {
            Origin::Voice => "lo pediste por voz",
            Origin::Agent => "lo pide un agente",
        }
    }

    /// The audit-trail form of this action.
    pub fn audit_json(&self) -> serde_json::Value {
        serde_json::json!({
            "action": self.kind.name(),
            "origin": match self.origin { Origin::Voice => "voice", Origin::Agent => "agent" },
            "subject": preview(&self.subject, 200),
            "intent": self.intent,
        })
    }
}

/// `text` cut to `max` characters with an ellipsis, on one line.
fn preview(text: &str, max: usize) -> String {
    let single_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() > max {
        let cut: String = single_line.chars().take(max).collect();
        format!("{cut}…")
    } else {
        single_line
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn descriptions_are_readable_spanish_sentences() {
        let action = Action::new(ActionKind::CloseApp, Origin::Agent, "Spotify");
        assert_eq!(action.describe(), "Cerrar la aplicación Spotify");
        assert_eq!(action.origin_label(), "lo pide un agente");
    }

    #[test]
    fn a_long_multiline_subject_is_flattened_and_truncated_in_the_prompt() {
        let text = format!("línea uno\nlínea dos {}", "x".repeat(200));
        let described = Action::new(ActionKind::InsertText, Origin::Agent, text).describe();
        assert!(!described.contains('\n'));
        assert!(described.contains('…'));
        assert!(described.chars().count() < 140);
    }

    #[test]
    fn the_audit_json_names_the_action_and_who_asked() {
        let action = Action::new(ActionKind::OpenUrl, Origin::Voice, "github.com")
            .with_intent(serde_json::json!({"kind": "open_url"}));
        let json = action.audit_json();
        assert_eq!(json["action"], "open_url");
        assert_eq!(json["origin"], "voice");
        assert_eq!(json["subject"], "github.com");
        assert_eq!(json["intent"]["kind"], "open_url");
    }
}
