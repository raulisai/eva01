#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! The intention layer: decides whether a transcript is plain dictation or a
//! command, and if it's a command, what it means. See `docs/PLAN.md` §3.2
//! (why there is no level-1 LLM router) and fase 4 (this crate's scope: it
//! classifies, it never executes).

pub mod apps;
pub mod intent;
pub mod risk;
pub mod wake;

pub use apps::{AppEntry, AppIndex};
pub use intent::Intent;
pub use risk::Risk;

/// The result of running the whole intention layer on a raw transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpretResult {
    /// No wake word matched: this is plain dictation, to be cleaned by
    /// `eva-text` and pasted, not touched by this crate any further.
    Dictation,
    /// The wake word matched and the remainder parsed into `Intent`.
    Command(Intent),
}

/// Runs the full intention pipeline on a raw transcript: strips the wake
/// word, and if present, parses the remainder into an [`Intent`]. This is
/// what both `eva-worker`'s production dictation path and the `eva intent`
/// CLI command call — see `docs/PLAN.md` §3.1, "esto es lo que hace testeable
/// la capa de intención sin grabar audio."
pub fn interpret(raw_text: &str, wake_word: &str, app_index: &AppIndex) -> InterpretResult {
    match wake::strip_wake_word(raw_text, wake_word) {
        Some(command_text) => InterpretResult::Command(intent::parse(command_text, app_index)),
        None => InterpretResult::Dictation,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::apps::AppEntry;

    fn sample_index() -> AppIndex {
        AppIndex::new(vec![AppEntry::new("Brave Browser").with_aliases(["brave"])])
    }

    #[test]
    fn text_without_the_wake_word_is_plain_dictation() {
        let result = interpret("mañana voy a abrir Brave", "Adán", &sample_index());
        assert_eq!(result, InterpretResult::Dictation);
    }

    #[test]
    fn text_with_the_wake_word_becomes_a_command() {
        let result = interpret("Adán, abre Brave", "Adán", &sample_index());
        assert_eq!(
            result,
            InterpretResult::Command(Intent::OpenApp { app: "Brave Browser".to_string() })
        );
    }

    #[test]
    fn the_end_to_end_headline_fix_the_wake_word_works_without_its_accent() {
        // This is the scenario from docs/PLAN.md §2A Hallazgo 4: the STT
        // engine renders the wake word without its tilde. It must still
        // gate as a command, not get pasted as dictation.
        let result = interpret("adan abre brave", "Adán", &sample_index());
        assert_eq!(
            result,
            InterpretResult::Command(Intent::OpenApp { app: "Brave Browser".to_string() })
        );
    }

    #[test]
    fn a_command_that_would_be_destructive_is_blocked_end_to_end() {
        let result = interpret("Adán, borra el proyecto", "Adán", &sample_index());
        assert!(matches!(result, InterpretResult::Command(Intent::Blocked { .. })));
    }
}
