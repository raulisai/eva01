//! Technical English in Spanish speech. The speech model is Spanish-first, so
//! when someone says "bug", "dashboard" or "UI" in the middle of a sentence it
//! writes what a Spanish ear hears: "book", "dash board", "u i" (real cases from
//! this user's own dictations: «arregla ese book», «la u i de», «Dash Board»).
//!
//! Two corrections that need no model and cannot turn an ordinary Spanish
//! sentence into something else:
//!
//! - [`TERMS`]: what the model writes for a term, mapped to the term. Only
//!   spellings that are not Spanish words are listed, or phrases that are not
//!   said in Spanish ("git hub", "back end"): «book» becomes «bug»; «bien» or
//!   «comic» never become anything.
//! - [`expand_spelled_acronyms`]: letters said one by one ("u i", "a p i", "c s s")
//!   become the acronym, but only when the letters spell one on [`ACRONYMS`].
//!
//! What this cannot know (a product name only this user says, a friend's name)
//! is taught, not shipped: reviewing a dictation in the panel teaches it.

use crate::normalize::{fold_diacritics, split_punctuation};

/// A term and the ways the model writes it when it is said aloud.
pub struct Term {
    /// The term, as it should be written.
    pub term: &'static str,
    /// What the model writes instead: folded (no accents, lowercase), one to
    /// five words.
    pub heard: &'static [&'static str],
}

/// The shipped glossary. Every `heard` entry is a spelling that is not a
/// Spanish word (or a phrase nobody says in Spanish); the test below checks
/// that none of them appears in ordinary Spanish sentences.
pub const TERMS: &[Term] = &[
    Term { term: "bug", heard: &["book", "bag"] },
    Term { term: "bugs", heard: &["books", "bags"] },
    Term { term: "commit", heard: &["comit", "comiit"] },
    Term { term: "pull request", heard: &["pul request", "pull rekuest", "pul rekuest", "pool request"] },
    Term { term: "deploy", heard: &["deploi", "diploy", "diploi", "deplói"] },
    Term { term: "frontend", heard: &["front end", "fron end", "frond end", "fron en"] },
    Term { term: "backend", heard: &["back end", "bac end", "bak end", "bakend"] },
    Term { term: "dashboard", heard: &["dash board", "dash bord", "dashbor", "dach board"] },
    Term { term: "framework", heard: &["frame work", "fraimwork", "fream work"] },
    Term { term: "webhook", heard: &["web hook", "web jook", "wed hook"] },
    Term { term: "endpoint", heard: &["end point", "end poin", "en point"] },
    Term { term: "localhost", heard: &["local host", "local jost"] },
    Term { term: "GitHub", heard: &["git hub", "guit hub", "guit jub", "git jub", "guithub"] },
    Term { term: "Docker", heard: &["doker", "dócker"] },
    Term { term: "Kubernetes", heard: &["cubernetes", "kuber netes", "cuber netes", "kubernetis", "cubernetis"] },
    Term { term: "Python", heard: &["paiton", "pai zon", "paizon", "paison"] },
    Term { term: "JavaScript", heard: &["java script", "yava script", "jaba script"] },
    Term { term: "TypeScript", heard: &["type script", "taip script", "tipe script"] },
    Term { term: "Tailwind", heard: &["tail wind", "teil wind"] },
    Term { term: "Postgres", heard: &["post gres", "postgress", "post gress"] },
    Term { term: "SQLite", heard: &["sequel lite", "es ku el lite", "sqlait"] },
    Term { term: "macOS", heard: &["mac o s", "mac os", "mak os", "macos"] },
    Term { term: "iOS", heard: &["i o s", "ai o es", "ai os"] },
    Term { term: "Xcode", heard: &["x code", "ex code", "equis code", "ecs code"] },
    Term { term: "VS Code", heard: &["v s code", "ve ese code", "vi es code", "vscode"] },
    Term { term: "ChatGPT", heard: &["chat g p t", "chat gpt", "chat ge pe te", "chat yepetei", "chat yepete"] },
    Term { term: "Claude", heard: &["claud", "clod", "clode", "claude"] },
    Term { term: "Qwen", heard: &["quentum", "quen", "cuen", "kuen", "guen", "chuen", "quwen", "cwen"] },
    Term { term: "Codex", heard: &["kodex", "codex"] },
    Term { term: "Ollama", heard: &["olama", "ólama"] },
    Term { term: "LLM", heard: &["ele ele eme", "el el em", "l l m", "ele ele em"] },
    Term { term: "Whisper", heard: &["wisper", "huisper", "guisper", "whisper"] },
    Term { term: "Wispr Flow", heard: &["whisper flow", "whisper flaw", "wisper flow", "wisper flaw", "wispr flaw"] },
    Term { term: "Brave", heard: &["brib", "brail", "brayv", "breiv"] },
    Term { term: "Node.js", heard: &["node js", "nod js", "noud js"] },
    Term { term: "Next.js", heard: &["next js", "nex js", "nest js"] },
    Term { term: "README", heard: &["read me", "rid mi", "ridmi"] },
    Term { term: "timeout", heard: &["time out", "taim aut"] },
    Term { term: "workflow", heard: &["work flow", "werk flow"] },
    Term { term: "pipeline", heard: &["pipe line", "paip lain"] },
    Term { term: "feedback", heard: &["fid bak", "fit back", "fid back"] },
    Term { term: "streaming", heard: &["estriming", "es triming"] },
    Term { term: "prompt", heard: &["promt", "prompt"] },
    Term { term: "script", heard: &["escript", "es cript"] },
    Term { term: "debug", heard: &["di bug", "de bug", "dibug"] },
    Term { term: "login", heard: &["log in", "logueo"] },
    Term { term: "logout", heard: &["log out"] },
    Term { term: "overlay", heard: &["over lay", "overlei", "ober lay"] },
    Term { term: "widget", heard: &["wiyet", "wichet"] },
    Term { term: "plugin", heard: &["plug in", "plagin"] },
    Term { term: "hardware", heard: &["hard ware", "jar guer"] },
    Term { term: "software", heard: &["soft ware", "sof guer"] },
];

/// Letters said one by one that become an acronym: what people spell out
/// when they talk about software. Only these: "a p" or "e s" are not anything.
/// "AI" is deliberately not here: "a i" is the Spanish "a" and a letter.
pub const ACRONYMS: &[&str] = &[
    "UI", "UX", "API", "CSS", "HTML", "SQL", "JSON", "URL", "CLI", "SDK", "CPU", "GPU", "RAM", "SSD", "USB", "PDF",
    "IDE", "LLM", "GPT", "MCP", "CI", "CD", "QA", "OS", "JS", "TS", "PR", "IA", "NPM", "SSH", "VPN", "DNS", "HTTP",
    "HTTPS", "REST", "XML", "CSV", "PNG", "JPG", "GUI", "MVP", "SaaS", "ORM", "JWT", "TTS", "STT", "ASR",
];

/// The glossary as `(heard, meant)` pairs, for a dictionary's replacements.
pub fn replacements() -> Vec<(String, String)> {
    TERMS
        .iter()
        .flat_map(|t| t.heard.iter().map(move |heard| ((*heard).to_string(), t.term.to_string())))
        .filter(|(heard, meant)| fold_diacritics(heard).to_lowercase() != fold_diacritics(meant).to_lowercase())
        .collect()
}

/// The longest run of spelled letters looked for ("h t t p s").
const MAX_LETTERS: usize = 5;

/// The acronym spelled by the single-letter words starting at `start`, as
/// `(words used, acronym, punctuation that ended it)`. The run stops at the
/// first token that is not a lone letter, or after one that carries
/// punctuation (a comma or full stop: the letters are in different phrases).
fn acronym_at(tokens: &[&str], start: usize) -> Option<(usize, &'static str, String)> {
    let mut spelled = String::new();
    let mut best = None;
    for (n, token) in tokens[start..].iter().enumerate().take(MAX_LETTERS) {
        let (_, core, suffix) = split_punctuation(token);
        let mut chars = core.chars();
        let Some(letter) = chars.next().filter(|c| c.is_alphabetic()).filter(|_| chars.next().is_none()) else {
            break;
        };
        spelled.push_str(&fold_diacritics(&letter.to_string()).to_lowercase());
        if n >= 1 {
            if let Some(acronym) = ACRONYMS.iter().find(|a| a.eq_ignore_ascii_case(&spelled)) {
                best = Some((n + 1, *acronym, suffix.to_string()));
            }
        }
        if !suffix.is_empty() {
            break;
        }
    }
    best
}

/// `text` with letters said one by one turned into the acronym they spell
/// ("la u i de" → "la UI de", "una a p i" → "una API"): the longest run of
/// single-letter words that spells an entry of [`ACRONYMS`], punctuation before
/// the first and after the last kept.
pub fn expand_spelled_acronyms(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        match acronym_at(&tokens, i) {
            Some((run, acronym, suffix)) => {
                let (prefix, _, _) = split_punctuation(tokens[i]);
                out.push(format!("{prefix}{acronym}{suffix}"));
                i += run;
            }
            None => {
                out.push(tokens[i].to_string());
                i += 1;
            }
        }
    }
    out.join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn letters_said_one_by_one_become_the_acronym_they_spell() {
        assert_eq!(expand_spelled_acronyms("ayuda a mejorar la u i de la app"), "ayuda a mejorar la UI de la app");
        assert_eq!(expand_spelled_acronyms("una a p i rest"), "una API rest");
        assert_eq!(expand_spelled_acronyms("los estilos c s s"), "los estilos CSS");
        assert_eq!(expand_spelled_acronyms("usa h t t p s siempre"), "usa HTTPS siempre");
        assert_eq!(expand_spelled_acronyms("Pon la u i."), "Pon la UI.");
        assert_eq!(expand_spelled_acronyms("¿cómo va la u i?"), "¿cómo va la UI?");
    }

    #[test]
    fn spanish_single_letter_words_are_not_acronyms() {
        for text in ["voy a i ver", "de a y por", "o sea a b", "ni a ni e", "el plan a y el plan b", "una a y una e"] {
            assert_eq!(expand_spelled_acronyms(text), text, "{text}");
        }
    }

    #[test]
    fn no_shipped_spelling_is_an_ordinary_spanish_word_or_a_term_already_written_right() {
        // Words a Spanish speaker really writes: none may be in the glossary.
        let heard: Vec<String> = replacements().into_iter().map(|(h, _)| fold_diacritics(&h).to_lowercase()).collect();
        for word in [
            "bien", "comic", "comite", "quien", "clase", "codigo", "libro", "local", "tipo", "cuento", "loco", "que",
            "con", "breve",
        ] {
            assert!(!heard.iter().any(|h| h == word), "«{word}» is Spanish and must not be rewritten");
        }
    }

    #[test]
    fn every_heard_form_is_one_to_five_words_and_differs_from_its_term() {
        for term in TERMS {
            for heard in term.heard {
                let words = heard.split_whitespace().count();
                assert!((1..=5).contains(&words), "{heard}");
            }
        }
        assert!(replacements()
            .iter()
            .all(|(h, m)| fold_diacritics(h).to_lowercase() != fold_diacritics(m).to_lowercase()));
    }
}
