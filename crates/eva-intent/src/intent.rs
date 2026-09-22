//! The level-0 rule parser: turns already wake-word-stripped text into a
//! typed [`Intent`]. Per `docs/PLAN.md` §3.2, there is no level-1 LLM
//! router — anything that does not match a rule here falls through to
//! [`Intent::AgentTask`], because the agent (with EVA's MCP tools in hand)
//! already is a capable, context-aware fallback. Nothing here executes
//! anything; parsing text into an `Intent` is this crate's entire job, per
//! `docs/PLAN.md` fase 4 ("no ejecuta nada").

use crate::apps::AppIndex;
use crate::risk::{self, Risk};
use serde::Serialize;

/// A parsed voice command, ready for the (not-yet-built) gateway to act on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intent {
    /// Not a command at all — no wake word matched. The caller should treat
    /// the original text as ordinary dictation, not reach `eva-intent` at all
    /// in practice, but this variant exists so `interpret` has a total
    /// result even if it is called on unstripped text by mistake.
    Dictation,

    /// A destructive verb was found in the command text, so no rule below
    /// was even attempted — see `docs/PLAN.md` fase 5: "se bloquea aunque
    /// una regla hiciera match." The (future) gateway decides what to do
    /// with this; `eva-intent`'s job stops at flagging it.
    Blocked {
        /// The verb stem that triggered the block, for the audit log.
        matched_stem: String,
        /// The full text that was blocked, unmodified.
        text: String,
    },

    /// "Adán, abre Brave" — open an installed application.
    OpenApp {
        /// The application's canonical name, resolved via the [`AppIndex`].
        app: String,
    },

    /// "Adán, cierra Spotify" — close a running application.
    CloseApp {
        /// The application's canonical name, resolved via the [`AppIndex`].
        app: String,
    },

    /// "Adán, abre github.com/foo" — open a URL directly, no app resolution needed.
    OpenUrl {
        /// The URL as dictated (scheme added by the caller if missing).
        url: String,
    },

    /// "Adán, busca gatos" — a web search.
    WebSearch {
        /// The search query.
        query: String,
    },

    /// Nothing above matched: hand the whole remaining text to an agent as a
    /// task prompt. This is the fallback from `docs/PLAN.md` §3.2 — not a
    /// missing feature, the intended design.
    AgentTask {
        /// What to ask the agent to do.
        prompt: String,
    },

    /// "Adán, continúa" (or "…y agrega también X") — resume the last agent
    /// session for the active project, per `docs/PLAN.md` fase 7. Which
    /// session that is, and whether one even exists, is `eva-worker`'s job
    /// (it needs the store); this crate only recognizes that the user asked
    /// to continue, and carries whatever extra instruction followed.
    ContinueAgentTask {
        /// Anything said after "continúa" ("continúa y agrega tests" → "y
        /// agrega tests"), or empty if nothing more was said.
        extra_prompt: String,
    },
}

/// Parses `command_text` (text with the wake word already stripped by
/// [`crate::wake::strip_wake_word`]) into an [`Intent`], using `app_index` to
/// resolve application names.
///
/// Order of operations, matching `docs/PLAN.md` fase 5: the destructive-verb
/// check runs before any rule, so a blocklisted verb short-circuits to
/// [`Intent::Blocked`] even if the text would otherwise have matched
/// `OpenApp`/`CloseApp`/etc.
pub fn parse(command_text: &str, app_index: &AppIndex) -> Intent {
    let trimmed = command_text.trim();

    if let Risk::Destructive { matched_stem } = risk::classify(trimmed) {
        return Intent::Blocked {
            matched_stem,
            text: trimmed.to_string(),
        };
    }

    if trimmed.is_empty() {
        // A bare wake word with nothing after it ("Adán" and nothing else)
        // is not a task worth handing to an agent; treat it as an empty
        // agent task rather than inventing a new variant just for this edge
        // case — the gateway/UI layer can decide to no-op on an empty prompt.
        return Intent::AgentTask { prompt: String::new() };
    }

    for (verb, build) in RULES {
        if let Some(rest) = strip_verb(trimmed, verb) {
            return build(rest, app_index);
        }
    }

    Intent::AgentTask { prompt: trimmed.to_string() }
}

/// One rule: a verb (and its common variants) to recognize at the start of
/// the command, and what to build from the rest of the text.
type RuleBuilder = fn(&str, &AppIndex) -> Intent;
const RULES: &[(&[&str], RuleBuilder)] = &[
    (&["abre", "abrir"], build_open),
    (&["cierra", "cerrar"], build_close),
    (&["busca", "buscar"], build_search),
    // Both spellings, not just the accented one: `strip_verb` (unlike the
    // wake-word gate) does not accent-fold, and this session's own testing
    // found an STT engine drop a tilde on a much more common word — no
    // reason to assume "continúa" survives every engine unaccented-safe.
    (&["continúa", "continua", "continuar"], build_continue),
];

/// If `text` starts with one of `verbs` as a whole word, returns the rest of
/// the text with that verb and the whitespace after it removed.
fn strip_verb<'a>(text: &'a str, verbs: &[&str]) -> Option<&'a str> {
    let lower = text.to_lowercase();
    for verb in verbs {
        if lower.starts_with(*verb) {
            let after = &lower[verb.len()..];
            let boundary_ok = after.chars().next().is_none_or(|c| !c.is_alphanumeric());
            if boundary_ok {
                return Some(text[verb.len()..].trim_start());
            }
        }
    }
    None
}

fn build_open(rest: &str, app_index: &AppIndex) -> Intent {
    if looks_like_url(rest) {
        return Intent::OpenUrl { url: rest.to_string() };
    }
    match app_index.find(rest) {
        Some(app) => Intent::OpenApp { app: app.canonical_name.clone() },
        // No confident app match — hand it to the agent rather than fail
        // silently or open the wrong app. "Adán, abre mi proyecto de X" is
        // exactly this case, and an agent with project context is a better
        // fallback than an intent classifier guessing.
        None => Intent::AgentTask { prompt: format!("abre {rest}") },
    }
}

fn build_close(rest: &str, app_index: &AppIndex) -> Intent {
    match app_index.find(rest) {
        Some(app) => Intent::CloseApp { app: app.canonical_name.clone() },
        None => Intent::AgentTask { prompt: format!("cierra {rest}") },
    }
}

fn build_search(rest: &str, _app_index: &AppIndex) -> Intent {
    Intent::WebSearch { query: rest.to_string() }
}

fn build_continue(rest: &str, _app_index: &AppIndex) -> Intent {
    Intent::ContinueAgentTask { extra_prompt: rest.to_string() }
}

/// A conservative heuristic for "this looks like a URL, not an app name":
/// starts with a scheme, or contains a dot with no spaces (a domain).
fn looks_like_url(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with("http://") || text.starts_with("https://") || text.starts_with("www.") {
        return true;
    }
    !text.contains(' ') && text.contains('.') && !text.ends_with('.')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::apps::AppEntry;

    fn sample_index() -> AppIndex {
        AppIndex::new(vec![
            AppEntry::new("Brave Browser").with_aliases(["brave"]),
            AppEntry::new("Spotify"),
        ])
    }

    #[test]
    fn parses_open_app() {
        let intent = parse("abre brave", &sample_index());
        assert_eq!(intent, Intent::OpenApp { app: "Brave Browser".to_string() });
    }

    #[test]
    fn parses_close_app() {
        let intent = parse("cierra spotify", &sample_index());
        assert_eq!(intent, Intent::CloseApp { app: "Spotify".to_string() });
    }

    #[test]
    fn parses_open_url_instead_of_an_app_when_it_looks_like_a_domain() {
        let intent = parse("abre github.com/foo", &sample_index());
        assert_eq!(intent, Intent::OpenUrl { url: "github.com/foo".to_string() });
    }

    #[test]
    fn parses_web_search() {
        let intent = parse("busca gatos en internet", &sample_index());
        assert_eq!(intent, Intent::WebSearch { query: "gatos en internet".to_string() });
    }

    #[test]
    fn parses_continue_with_no_extra_instruction() {
        let intent = parse("continúa", &sample_index());
        assert_eq!(intent, Intent::ContinueAgentTask { extra_prompt: String::new() });
    }

    #[test]
    fn parses_continue_without_the_accent_too() {
        // strip_verb itself does not accent-fold — both spellings are
        // listed in RULES precisely so a dropped tilde still resolves here.
        let intent = parse("continua", &sample_index());
        assert_eq!(intent, Intent::ContinueAgentTask { extra_prompt: String::new() });
    }

    #[test]
    fn parses_continue_with_an_extra_instruction() {
        let intent = parse("continúa y agrega también tests", &sample_index());
        assert_eq!(intent, Intent::ContinueAgentTask { extra_prompt: "y agrega también tests".to_string() });
    }

    #[test]
    fn falls_back_to_agent_task_when_no_rule_matches() {
        let intent = parse("agrega tests al login", &sample_index());
        assert_eq!(intent, Intent::AgentTask { prompt: "agrega tests al login".to_string() });
    }

    #[test]
    fn falls_back_to_agent_task_when_the_app_is_not_in_the_index() {
        let intent = parse("abre mi proyecto de svelte", &sample_index());
        assert_eq!(
            intent,
            Intent::AgentTask { prompt: "abre mi proyecto de svelte".to_string() }
        );
    }

    #[test]
    fn destructive_verbs_are_blocked_even_though_no_rule_would_have_matched() {
        let intent = parse("borra el repositorio de iam", &sample_index());
        assert!(matches!(intent, Intent::Blocked { .. }));
    }

    #[test]
    fn destructive_verbs_are_blocked_even_when_a_rule_would_otherwise_match() {
        // "cierra" would normally build CloseApp, but the presence of a
        // destructive verb anywhere in the text short-circuits first.
        let intent = parse("cierra spotify y borra mi música", &sample_index());
        assert!(matches!(intent, Intent::Blocked { .. }));
    }

    #[test]
    fn empty_command_text_is_an_empty_agent_task_not_a_panic() {
        assert_eq!(parse("", &sample_index()), Intent::AgentTask { prompt: String::new() });
        assert_eq!(parse("   ", &sample_index()), Intent::AgentTask { prompt: String::new() });
    }

    #[test]
    fn serializes_to_the_tagged_json_shape_eva_ipc_expects() {
        let intent = Intent::OpenApp { app: "Brave Browser".to_string() };
        let json = serde_json::to_value(&intent).expect("Intent must always serialize");
        assert_eq!(json, serde_json::json!({"kind": "open_app", "app": "Brave Browser"}));
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics_on_arbitrary_command_text(text in ".*") {
            let _ = parse(&text, &sample_index());
        }
    }
}
