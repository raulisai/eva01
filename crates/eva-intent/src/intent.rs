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
        /// The agent the user named ("usa Claude y…"): `"claude_code"` or
        /// `"codex"`. `None` lets the registry pick by priority.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },

    /// "Adán, hazlo más formal" — rewrite the text the user has selected,
    /// following the spoken instruction (`docs/PLAN.md` fase 9's edit mode).
    /// Which text that is, and whether the on-device model can do it, is
    /// `eva-worker`'s business; this crate only recognizes the request.
    EditSelection {
        /// The whole spoken instruction, verb included ("hazlo más formal"),
        /// which is exactly what the rewriting model is given.
        instruction: String,
    },

    /// "Adán, abre Photoshop" when there is no such app on this Mac: not a task
    /// for an agent (which would spend time and quota looking for it), but a
    /// plain "it is not installed" — and, for opening, an offer to look for it
    /// in the App Store.
    AppNotFound {
        /// What the user asked for, as said ("photoshop").
        name: String,
        /// Whether they wanted it opened (the App Store can help) or closed.
        opening: bool,
    },

    /// One of the user's own phrases (`[[commands]]` in the config): "Adán,
    /// mi correo". What it does is `eva-worker`'s business — it holds the
    /// config — so this carries only which phrase matched.
    Custom {
        /// The phrase as written in the config.
        phrase: String,
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

/// Whether `spoken` is the user's `phrase`: the same words, whatever the
/// accents, case or punctuation the speech model happened to give them.
pub fn is_phrase(spoken: &str, phrase: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        eva_text::fold_diacritics(text)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let wanted = words(phrase);
    !wanted.is_empty() && words(spoken) == wanted
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
        return Intent::Blocked { matched_stem, text: trimmed.to_string() };
    }

    if trimmed.is_empty() {
        // A bare wake word with nothing after it ("Adán" and nothing else)
        // is not a task worth handing to an agent; treat it as an empty
        // agent task rather than inventing a new variant just for this edge
        // case — the gateway/UI layer can decide to no-op on an empty prompt.
        return Intent::AgentTask { prompt: String::new(), provider: None };
    }

    if is_edit_request(trimmed) {
        return Intent::EditSelection { instruction: trimmed.to_string() };
    }

    if let Some((provider, task)) = split_provider_prefix(trimmed) {
        return Intent::AgentTask { prompt: task.to_string(), provider: Some(provider.to_string()) };
    }

    for (verb, build) in RULES {
        if let Some(rest) = strip_verb(trimmed, verb) {
            return build(rest, app_index);
        }
    }

    Intent::AgentTask { prompt: trimmed.to_string(), provider: None }
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

/// `text` without the full stop (or other closing mark) the speech model puts
/// at the end of almost every utterance — "abre github.com." must not read as
/// an app called "github.com.".
fn without_closing_punctuation(text: &str) -> &str {
    text.trim().trim_end_matches(['.', ',', ';', ':', '!', '?', '…']).trim_end()
}

/// Words said before an app's name that are not part of it: "abre **el**
/// calendario", "cierra **la app de** notas", "abre **mi** correo".
const NAME_LEADS: &[&str] = &[
    "la aplicacion de ",
    "la aplicacion ",
    "la app de ",
    "la app ",
    "el programa de ",
    "el programa ",
    "el ",
    "la ",
    "los ",
    "las ",
    "mi ",
    "mis ",
    "un ",
    "una ",
];

/// `name` without what [`NAME_LEADS`] lists, compared accent-folded ("la
/// aplicación de" and "la aplicacion de" alike).
fn app_name(name: &str) -> &str {
    let folded = eva_text::fold_diacritics(name);
    for lead in NAME_LEADS {
        if folded.starts_with(lead) {
            // Folding keeps one character per character, so the lead covers
            // the same number of characters in `name`.
            let cut = name.char_indices().nth(lead.chars().count()).map_or(name.len(), |(i, _)| i);
            return name[cut..].trim_start();
        }
    }
    name
}

/// Words (folded: no accents) that make "abre …" about something in a
/// project or on disk, not an app: "abre el proyecto de facturación", "abre
/// mi carpeta de descargas".
const NOT_AN_APP: &[&str] = &[
    "proyecto",
    "proyectos",
    "archivo",
    "archivos",
    "carpeta",
    "carpetas",
    "documento",
    "documentos",
    "repositorio",
    "repo",
    "pestana",
    "pestanas",
    "ventana",
    "ventanas",
    "pagina",
    "sitio",
    "enlace",
    "link",
    "rama",
    "issue",
    "pr",
    "codigo",
    "sesion",
];

/// The longest name that can still be an app ("Adobe Photoshop 2025", "Final
/// Cut Pro"); anything longer is a request, not a name.
const LONGEST_APP_NAME_WORDS: usize = 4;

/// Whether `rest`, which matched no installed app, is nonetheless probably the
/// name of one: a few words, with none that point at a project or a file.
fn looks_like_an_app_name(rest: &str) -> bool {
    let words: Vec<String> = eva_text::fold_diacritics(app_name(rest))
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    !words.is_empty()
        && words.len() <= LONGEST_APP_NAME_WORDS
        && !words.iter().any(|w| NOT_AN_APP.contains(&w.as_str()))
}

fn build_open(rest: &str, app_index: &AppIndex) -> Intent {
    let rest = without_closing_punctuation(rest);
    if let Some(url) = crate::spoken::spoken_url(rest) {
        return Intent::OpenUrl { url };
    }
    if looks_like_url(rest) {
        return Intent::OpenUrl { url: rest.to_string() };
    }
    match app_index.find(rest).or_else(|| app_index.find(app_name(rest))) {
        Some(app) => Intent::OpenApp { app: app.canonical_name.clone() },
        // A short name that no installed app answers to: not installed. A
        // longer or project-shaped request ("abre mi proyecto de X") is what
        // an agent with project context is for, better than a classifier guessing.
        None if looks_like_an_app_name(rest) => Intent::AppNotFound { name: app_name(rest).to_string(), opening: true },
        None => Intent::AgentTask { prompt: format!("abre {rest}"), provider: None },
    }
}

fn build_close(rest: &str, app_index: &AppIndex) -> Intent {
    let rest = without_closing_punctuation(rest);
    match app_index.find(rest).or_else(|| app_index.find(app_name(rest))) {
        Some(app) => Intent::CloseApp { app: app.canonical_name.clone() },
        None if looks_like_an_app_name(rest) => {
            Intent::AppNotFound { name: app_name(rest).to_string(), opening: false }
        }
        None => Intent::AgentTask { prompt: format!("cierra {rest}"), provider: None },
    }
}

fn build_search(rest: &str, _app_index: &AppIndex) -> Intent {
    Intent::WebSearch { query: without_closing_punctuation(rest).to_string() }
}

fn build_continue(rest: &str, _app_index: &AppIndex) -> Intent {
    Intent::ContinueAgentTask { extra_prompt: rest.to_string() }
}

/// Verbs that, with a pronoun glued on ("hazlo", "acórtalo"), can only mean
/// "do that to the thing I have selected" — nothing an agent task starts
/// with. Compared accent-folded, since an STT engine may drop the tilde.
const EDIT_CLITIC_VERBS: &[&str] = &[
    "hazlo",
    "hazla",
    "acortalo",
    "acortala",
    "resumelo",
    "resumela",
    "traducelo",
    "traducela",
    "corrigelo",
    "corrigela",
    "mejoralo",
    "mejorala",
    "reescribelo",
    "reescribela",
    "simplificalo",
    "simplificala",
    "alargalo",
    "alargala",
    "formalizalo",
    "formalizala",
    "cambialo",
    "cambiala",
    "parafrasealo",
    "parafraseala",
];

/// Bare verbs ("traduce", "corrige") are also what you say to an agent about
/// a *project* ("corrige el bug del login"), so they only mean an edit when
/// pointed at the selection.
const EDIT_BARE_VERBS: &[&str] = &["reescribe", "traduce", "resume", "acorta", "corrige", "mejora", "simplifica"];

/// What "the selection" sounds like after a bare edit verb.
const SELECTION_REFERENCES: &[&str] =
    &["esto", "eso", "este texto", "ese texto", "el texto", "lo seleccionado", "la seleccion"];

fn is_edit_request(text: &str) -> bool {
    let folded = eva_text::fold_diacritics(text);
    let mut words = folded.split_whitespace();
    let Some(first) = words.next() else { return false };
    let first = first.trim_matches(|c: char| !c.is_alphanumeric());

    if EDIT_CLITIC_VERBS.contains(&first) {
        // A bare "hazlo" is what you tell an agent to go ahead ("Adán,
        // hazlo"); an edit says how: "hazlo más formal".
        let bare_do_it = matches!(first, "hazlo" | "hazla") && words.next().is_none();
        return !bare_do_it;
    }
    if EDIT_BARE_VERBS.contains(&first) {
        let rest = words.collect::<Vec<_>>().join(" ");
        return SELECTION_REFERENCES.iter().any(|reference| rest.starts_with(reference));
    }
    false
}

/// How each agent may be spoken: STT engines turn "Claude" into "cloud" or
/// "clod" all the time, so the aliases are what the user *says*, not only
/// what the CLI is called.
const PROVIDER_ALIASES: &[(&str, &[&str])] = &[
    ("claude_code", &["claude code", "claude", "clod", "clode", "cloud", "clau"]),
    ("codex", &["codex", "codecs", "kodex", "codex cli"]),
];

/// Words that join "usa Claude" to the task itself.
const TASK_CONNECTORS: &[&str] = &["y", "e", "para", "para que", "que", "a", "de"];

/// "usa claude y arregla el login" → `("claude_code", "arregla el login")`.
fn split_provider_prefix(text: &str) -> Option<(&'static str, &str)> {
    let rest = strip_verb(text, &["usa", "utiliza", "usar", "utilizar"])?;
    let rest = rest.strip_prefix("a ").or_else(|| rest.strip_prefix("el ")).unwrap_or(rest);

    let folded = eva_text::fold_diacritics(rest);
    for (provider, aliases) in PROVIDER_ALIASES {
        for alias in *aliases {
            let Some(after) = folded.strip_prefix(alias) else { continue };
            if after.chars().next().is_some_and(char::is_alphanumeric) {
                continue; // "claudia", "codexx": the alias is only part of a longer word
            }
            // `folded` only differs from `rest` by accents and case, and
            // folding keeps one character per character, so the alias covers
            // the same number of *characters* in `rest` (not necessarily the
            // same number of bytes — an accented letter is two).
            let consumed_bytes = rest.char_indices().nth(alias.chars().count()).map_or(rest.len(), |(index, _)| index);
            let tail = rest[consumed_bytes..].trim_start_matches(|c: char| c == ',' || c.is_whitespace());
            return Some((provider, strip_connector(tail)));
        }
    }
    None
}

fn strip_connector(text: &str) -> &str {
    for connector in TASK_CONNECTORS {
        if let Some(rest) = strip_verb(text, &[connector]) {
            return rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        }
    }
    text
}

/// `scheme://…` for any scheme — `file://`, `smb://`, `x-apple.…://` are
/// links too, and recognizing them here is what lets the gateway rule on
/// them instead of the parser quietly treating them as an app name.
fn has_scheme_separator(text: &str) -> bool {
    text.split_once("://").is_some_and(|(scheme, rest)| {
        !rest.is_empty()
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// A conservative heuristic for "this looks like a URL, not an app name":
/// starts with a scheme, or contains a dot with no spaces (a domain).
fn looks_like_url(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with("www.") || has_scheme_separator(text) {
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
        AppIndex::new(vec![AppEntry::new("Brave Browser").with_aliases(["brave"]), AppEntry::new("Spotify")])
    }

    #[test]
    fn parses_open_app() {
        let intent = parse("abre brave", &sample_index());
        assert_eq!(intent, Intent::OpenApp { app: "Brave Browser".to_string() });
    }

    #[test]
    fn the_closing_full_stop_the_speech_model_adds_does_not_change_the_command() {
        let index =
            AppIndex::new(vec![AppEntry::new("GitHub Desktop").with_aliases(["github"]), AppEntry::new("Spotify")]);
        assert_eq!(parse("abre github.com.", &index), Intent::OpenUrl { url: "github.com".to_string() });
        assert_eq!(parse("abre www.google.com.", &index), Intent::OpenUrl { url: "www.google.com".to_string() });
        assert_eq!(parse("cierra Spotify.", &index), Intent::CloseApp { app: "Spotify".to_string() });
        assert_eq!(
            parse("busca el clima de mañana.", &index),
            Intent::WebSearch { query: "el clima de mañana".to_string() }
        );
    }

    #[test]
    fn articles_and_app_de_before_the_name_are_not_part_of_it() {
        let index = AppIndex::new(vec![AppEntry::new("Calendar"), AppEntry::new("Notes"), AppEntry::new("Calculator")]);
        assert_eq!(parse("abre el calendario", &index), Intent::OpenApp { app: "Calendar".to_string() });
        assert_eq!(parse("abre la calculadora.", &index), Intent::OpenApp { app: "Calculator".to_string() });
        assert_eq!(parse("cierra la aplicación de notas", &index), Intent::CloseApp { app: "Notes".to_string() });
        assert_eq!(parse("abre las notas", &index), Intent::OpenApp { app: "Notes".to_string() });
        assert!(matches!(parse("abre el proyecto de facturación", &index), Intent::AgentTask { .. }));
    }

    #[test]
    fn a_short_name_no_installed_app_answers_to_is_reported_not_sent_to_an_agent() {
        let index = AppIndex::new(vec![AppEntry::new("Spotify")]);
        assert_eq!(parse("abre photoshop", &index), Intent::AppNotFound { name: "photoshop".into(), opening: true });
        assert_eq!(
            parse("abre la aplicación de Adobe Premiere Pro.", &index),
            Intent::AppNotFound { name: "Adobe Premiere Pro".into(), opening: true }
        );
        assert_eq!(parse("cierra notion", &index), Intent::AppNotFound { name: "notion".into(), opening: false });
    }

    #[test]
    fn requests_about_projects_files_or_long_sentences_still_go_to_an_agent() {
        let index = AppIndex::new(vec![AppEntry::new("Spotify")]);
        for text in [
            "abre el proyecto de facturación",
            "abre mi carpeta de descargas",
            "abre el archivo de configuración",
            "abre la pestaña de github",
            "abre la rama de arreglos del login y dime qué falta",
            "abre el repositorio de eva",
        ] {
            assert!(matches!(parse(text, &index), Intent::AgentTask { .. }), "{text}");
        }
    }

    #[test]
    fn spoken_addresses_open_the_browser_instead_of_an_app_or_an_agent() {
        let index = AppIndex::new(vec![AppEntry::new("GitHub Desktop").with_aliases(["github"])]);
        assert_eq!(parse("abre github punto com", &index), Intent::OpenUrl { url: "github.com".to_string() });
        assert_eq!(
            parse("abre google punto com punto mx.", &index),
            Intent::OpenUrl { url: "google.com.mx".to_string() }
        );
        assert_eq!(parse("abre localhost tres mil", &index), Intent::OpenUrl { url: "localhost:3000".to_string() });
        assert_eq!(parse("abre localhost:5173", &index), Intent::OpenUrl { url: "localhost:5173".to_string() });
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
    fn any_scheme_with_slashes_is_a_link_not_an_app_name() {
        for url in ["file:///etc/hosts", "smb://servidor/x", "ssh://host", "https://github.com"] {
            assert_eq!(
                parse(&format!("abre {url}"), &sample_index()),
                Intent::OpenUrl { url: url.to_string() },
                "{url}"
            );
        }
        assert!(matches!(parse("abre mi proyecto de ://", &sample_index()), Intent::AgentTask { .. }));
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
        assert_eq!(intent, Intent::AgentTask { prompt: "agrega tests al login".to_string(), provider: None });
    }

    #[test]
    fn falls_back_to_agent_task_when_the_app_is_not_in_the_index() {
        let intent = parse("abre mi proyecto de svelte", &sample_index());
        assert_eq!(intent, Intent::AgentTask { prompt: "abre mi proyecto de svelte".to_string(), provider: None });
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
        assert_eq!(parse("", &sample_index()), Intent::AgentTask { prompt: String::new(), provider: None });
        assert_eq!(parse("   ", &sample_index()), Intent::AgentTask { prompt: String::new(), provider: None });
    }

    fn agent(prompt: &str, provider: Option<&str>) -> Intent {
        Intent::AgentTask { prompt: prompt.to_string(), provider: provider.map(str::to_string) }
    }

    #[test]
    fn usa_claude_forces_that_agent_and_strips_the_connector() {
        assert_eq!(
            parse("usa claude y arregla el login", &sample_index()),
            agent("arregla el login", Some("claude_code"))
        );
        assert_eq!(parse("usa codex para agregar tests", &sample_index()), agent("agregar tests", Some("codex")));
        assert_eq!(
            parse("usa a Claude, revisa el build", &sample_index()),
            agent("revisa el build", Some("claude_code"))
        );
    }

    #[test]
    fn the_ways_an_stt_engine_mishears_claude_still_select_it() {
        for heard in
            ["usa cloud y arregla el login", "usa clod y arregla el login", "usa Claude Code y arregla el login"]
        {
            assert_eq!(parse(heard, &sample_index()), agent("arregla el login", Some("claude_code")), "{heard}");
        }
    }

    #[test]
    fn an_alias_that_is_only_the_start_of_another_word_is_not_a_provider() {
        assert_eq!(parse("usa claudia para el correo", &sample_index()), agent("usa claudia para el correo", None));
    }

    #[test]
    fn usa_without_a_provider_is_an_ordinary_agent_task() {
        assert_eq!(
            parse("usa typescript para el nuevo módulo", &sample_index()),
            agent("usa typescript para el nuevo módulo", None)
        );
    }

    #[test]
    fn a_bare_usa_claude_is_an_empty_task_for_that_agent() {
        assert_eq!(parse("usa claude", &sample_index()), agent("", Some("claude_code")));
    }

    #[test]
    fn a_pronoun_verb_is_an_edit_of_the_selection() {
        for text in
            ["hazlo más formal", "hazla más corta", "acórtalo", "acortalo un poco", "tradúcelo al inglés", "corrígelo"]
        {
            assert_eq!(parse(text, &sample_index()), Intent::EditSelection { instruction: text.to_string() }, "{text}");
        }
    }

    #[test]
    fn a_bare_hazlo_is_go_ahead_not_an_edit() {
        assert_eq!(parse("hazlo", &sample_index()), agent("hazlo", None));
        assert_eq!(parse("hazla", &sample_index()), agent("hazla", None));
    }

    #[test]
    fn a_bare_edit_verb_needs_to_point_at_the_selection() {
        assert!(matches!(parse("reescribe esto más formal", &sample_index()), Intent::EditSelection { .. }));
        assert!(matches!(parse("traduce este texto al inglés", &sample_index()), Intent::EditSelection { .. }));
        assert!(matches!(parse("corrige esto", &sample_index()), Intent::EditSelection { .. }));
    }

    #[test]
    fn a_bare_verb_about_a_project_is_still_an_agent_task() {
        assert_eq!(parse("corrige el bug del login", &sample_index()), agent("corrige el bug del login", None));
        assert_eq!(
            parse("resume los cambios de la rama", &sample_index()),
            agent("resume los cambios de la rama", None)
        );
        assert_eq!(
            parse("traduce la interfaz al inglés", &sample_index()),
            agent("traduce la interfaz al inglés", None)
        );
    }

    #[test]
    fn serializes_to_the_tagged_json_shape_eva_ipc_expects() {
        let intent = Intent::OpenApp { app: "Brave Browser".to_string() };
        let json = serde_json::to_value(&intent).expect("Intent must always serialize");
        assert_eq!(json, serde_json::json!({"kind": "open_app", "app": "Brave Browser"}));
    }

    #[test]
    fn an_agent_task_without_a_provider_serializes_without_the_field() {
        let json = serde_json::to_value(agent("hola", None)).expect("serializes");
        assert_eq!(json, serde_json::json!({"kind": "agent_task", "prompt": "hola"}));
        let json = serde_json::to_value(agent("hola", Some("codex"))).expect("serializes");
        assert_eq!(json["provider"], "codex");
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics_on_arbitrary_command_text(text in ".*") {
            let _ = parse(&text, &sample_index());
        }
    }
}
