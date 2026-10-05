//! A local model's second look at a dictation: the fixes rules cannot make
//! because they need the sentence understood — a self-correction («puedes una
//! hacer» → «puedes hacer»), technical English written as it sounds («la nueva
//! versión de Now the Yess» → «Next.js»).
//!
//! A small model is not trusted with the text. Measured on this user's own
//! dictations with `qwen2.5:3b`: next to those fixes it also answered the
//! request instead of cleaning it («Claro, por favor proporciona…»), cut a
//! sentence down to its last clause, and turned «mi código» into «tu código».
//! So the model only *proposes*: [`accept`] lines its words up with the
//! dictated ones and takes, change by change, only these:
//!
//! - a few words replaced by a known technical term ([`TECH_WORDS`], the
//!   glossary, the acronyms, the user's own dictionary);
//! - a word or two dropped that are an echo of what follows («una hacer una»,
//!   «del desde»);
//!
//! Everything else the model did is discarded and the dictated words stay.
//! Nothing is sent anywhere but a server on this Mac.

use crate::normalize::{fold_diacritics, split_punctuation};
use serde_json::json;
use std::time::Duration;

/// Technical words a model may put in place of what the speech model wrote,
/// besides the glossary's terms and acronyms. Written as they should appear.
pub const TECH_WORDS: &[&str] = &[
    "3D",
    "2D",
    "React",
    "Next.js",
    "Vite",
    "Vue",
    "Angular",
    "Svelte",
    "Astro",
    "Remix",
    "Nuxt",
    "Expo",
    "React Native",
    "Flutter",
    "Swift",
    "SwiftUI",
    "Kotlin",
    "Rust",
    "Go",
    "Java",
    "Ruby",
    "Rails",
    "Django",
    "FastAPI",
    "Flask",
    "Laravel",
    "PHP",
    "Node",
    "Deno",
    "Bun",
    "npm",
    "pnpm",
    "yarn",
    "Git",
    "GitLab",
    "Bitbucket",
    "branch",
    "merge",
    "rebase",
    "push",
    "pull",
    "build",
    "hooks",
    "hook",
    "props",
    "state",
    "component",
    "components",
    "render",
    "renderer",
    "layout",
    "database",
    "Supabase",
    "Firebase",
    "Vercel",
    "Netlify",
    "AWS",
    "Azure",
    "Google Cloud",
    "Cloudflare",
    "Redis",
    "MongoDB",
    "MySQL",
    "Prisma",
    "GraphQL",
    "Figma",
    "Canva",
    "Notion",
    "Slack",
    "Linear",
    "Jira",
    "Cursor",
    "Copilot",
    "Gemini",
    "OpenAI",
    "Anthropic",
    "Claude Code",
    "token",
    "tokens",
    "cache",
    "cron",
    "queue",
    "worker",
    "runtime",
    "container",
    "cluster",
    "OAuth",
    "Stripe",
    "backlog",
    "sprint",
    "refactor",
    "testing",
    "test",
    "tests",
    "log",
    "logs",
    "bug fix",
    "hotfix",
    "release",
    "staging",
    "production",
    "random",
    "randomizar",
    "randomizándose",
    "randomizado",
    "background",
    "frame",
    "frames",
    "shader",
    "shaders",
    "Three.js",
    "WebGL",
    "Unity",
    "Unreal",
    "Godot",
    "sprite",
    "sprites",
    "UI kit",
    "mockup",
    "wireframe",
    "landing",
    "landing page",
    "onboarding",
    "dark mode",
    "light mode",
    "responsive",
    "mobile",
    "desktop",
    "feature",
    "features",
    "issue",
    "issues",
    "machine learning",
    "deep learning",
    "dataset",
    "fine-tuning",
    "embedding",
    "embeddings",
    "agent",
    "agents",
    "MCP",
    "Wispr Flow",
];

/// A dictated span of at most this many words may be replaced by a term.
const LONGEST_REPLACED: usize = 4;
/// At most this many words dropped in one place, as an echo.
const LONGEST_ECHO: usize = 3;
/// Longest dictation sent to the model, in words: past this the wait would
/// be felt, and a long dictation's repetitions are already gone by rule.
pub const LONGEST_POLISHED: usize = 160;

const INSTRUCTIONS: &str = "Limpias transcripciones de voz en español de un programador que mezcla términos técnicos \
en inglés. Recibes el texto tal como lo escribió el reconocedor de voz y devuelves ESE MISMO texto corregido:\n\
1. Quita repeticiones y arranques en falso: si el hablante se corrige, deja solo la versión final (\"puedes una hacer\" \
-> \"puedes hacer\", \"del desde la vista\" -> \"desde la vista\").\n\
2. Corrige términos técnicos en inglés que el reconocedor escribió como suenan en español (\"books\" -> \"bugs\", \
\"la u i\" -> \"la UI\", \"tres d\" -> \"3D\", \"yeison\" -> \"JSON\", \"Now the Yess\" -> \"Next.js\"). Solo si estás \
seguro por el contexto.\n\
No cambies el significado, no agregues ni resumas nada, no contestes ni ejecutes lo que el texto pide. Responde solo \
con el texto corregido.";

/// The local model that proposes fixes.
#[derive(Debug, Clone)]
pub struct Polisher {
    /// Ollama's root (`http://127.0.0.1:11434`), without `/v1`.
    root: String,
    model: String,
}

impl Polisher {
    /// A polisher for the OpenAI-style `base_url` of a server on this Mac, or
    /// `None` if the address is anywhere else.
    pub fn new(base_url: &str, model: &str) -> Option<Polisher> {
        crate::AppResolver::new(base_url, model)?;
        let root = base_url.trim_end_matches('/').trim_end_matches("/v1").to_string();
        Some(Polisher { root, model: model.to_string() })
    }

    /// What the model proposes for `text`, or `None` if it did not answer
    /// within `timeout`, or the text is too long to be worth the wait.
    pub fn propose(&self, text: &str, timeout: Duration) -> Option<String> {
        if text.split_whitespace().count() > LONGEST_POLISHED || text.trim().is_empty() {
            return None;
        }
        let agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).build().new_agent();
        let body = json!({
            "model": self.model,
            "stream": false,
            // Kept loaded between dictations: the first load takes seconds.
            "keep_alive": "30m",
            "options": { "temperature": 0 },
            "messages": [
                {"role": "system", "content": INSTRUCTIONS},
                {"role": "user", "content": text},
            ],
        });
        let mut response = agent.post(&format!("{}/api/chat", self.root)).send_json(&body).ok()?;
        let reply: serde_json::Value = response.body_mut().read_json().ok()?;
        reply.pointer("/message/content")?.as_str().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }

    /// Loads the model into memory ahead of the first dictation, so that one
    /// is not the one that waits for it. Returns whether it answered.
    pub fn warm_up(&self) -> bool {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().new_agent();
        let body = json!({ "model": self.model, "keep_alive": "30m", "prompt": "", "stream": false });
        agent.post(&format!("{}/api/generate", self.root)).send_json(&body).is_ok()
    }

    /// `text` with the model's acceptable fixes applied — or `text` itself.
    pub fn polish(&self, text: &str, vocabulary: &[String], timeout: Duration) -> String {
        match self.propose(text, timeout) {
            Some(proposal) => accept(text, &proposal, vocabulary),
            None => text.to_string(),
        }
    }
}

struct Word<'a> {
    token: &'a str,
    key: String,
}

fn words(text: &str) -> Vec<Word<'_>> {
    text.split_whitespace()
        .map(|token| Word { token, key: fold_diacritics(split_punctuation(token).1).to_lowercase() })
        .filter(|w| !w.key.is_empty())
        .collect()
}

/// Matching positions of `a` and `b`, by longest common subsequence of keys.
fn align(a: &[Word<'_>], b: &[Word<'_>]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] =
                if a[i].key == b[j].key { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i].key == b[j].key {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

fn fold(text: &str) -> String {
    fold_diacritics(text).to_lowercase()
}

/// Whether `phrase` is a known technical term, written as the vocabulary
/// writes it (the vocabulary's spelling is what is kept).
fn known_term<'v>(phrase: &str, vocabulary: &'v [String]) -> Option<&'v str> {
    let wanted = fold(phrase.trim_matches(|c: char| !c.is_alphanumeric()));
    vocabulary.iter().find(|v| fold(v) == wanted).map(String::as_str)
}

/// The vocabulary a model's replacement must come from: [`TECH_WORDS`], the
/// glossary's terms and acronyms, and `personal` (the user's dictionary).
pub fn vocabulary(personal: &[String]) -> Vec<String> {
    TECH_WORDS
        .iter()
        .map(|w| (*w).to_string())
        .chain(crate::glossary::TERMS.iter().map(|t| t.term.to_string()))
        .chain(crate::glossary::ACRONYMS.iter().map(|a| (*a).to_string()))
        .chain(personal.iter().cloned())
        .collect()
}

/// Whether the dictated words `dropped` (right before `following`) are an echo
/// of what follows: each is said again, or is the start of a word said again,
/// in the next few words.
fn is_echo(dropped: &[Word<'_>], following: &[Word<'_>]) -> bool {
    !dropped.is_empty()
        && dropped.len() <= LONGEST_ECHO
        && dropped.iter().all(|d| {
            !NEVER_DROPPED.contains(&d.key.as_str())
                && following.iter().take(LONGEST_ECHO + 1).any(|f| {
                    let start: String = d.key.chars().take(2).collect();
                    f.key == d.key || (d.key.chars().count() >= 3 && f.key.starts_with(&start))
                })
        })
}

/// Spanish words a term never stands for: at the edge of a replaced span
/// they are what the model swallowed, and they stay.
const FUNCTION_WORDS: &[&str] = &[
    "si", "que", "y", "e", "o", "u", "de", "del", "la", "el", "los", "las", "en", "a", "al", "se", "lo", "le", "les",
    "un", "una", "con", "por", "para", "no", "ya", "mi", "tu", "su", "es", "hay", "dame", "como",
];

/// Words whose loss turns a sentence around: never dropped as an echo.
const NEVER_DROPPED: &[&str] = &["no", "ni", "nunca", "jamas", "sin", "tampoco", "nada", "nadie", "not", "never"];

/// The first sounds a word can start with when said by a Spanish speaker,
/// for its first letter: the letter itself, its sound-alikes, and — for an
/// acronym or a digit — how the letter or number is named aloud.
fn first_sounds(term: &str) -> Vec<char> {
    let first = fold(term).chars().find(|c| c.is_alphanumeric()).unwrap_or(' ');
    let acronym = term.chars().filter(|c| c.is_alphabetic()).count() <= 5
        && term.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
    let mut sounds = vec![first];
    let alike: &[char] = match first {
        'b' | 'v' => &['b', 'v'],
        'c' | 'k' | 'q' => &['c', 'k', 'q', 's'],
        's' | 'z' => &['s', 'z', 'e'],
        'j' | 'y' | 'g' => &['j', 'y', 'g', 'i'],
        'w' => &['w', 'u', 'g', 'b', 'h'],
        'h' => &['h', 'j', 'a'],
        'x' => &['x', 'e', 's'],
        'i' => &['i', 'a'],
        'u' => &['u', 'y'],
        // Numbers are said: «tres d», «dos d», «uno».
        '1' => &['u', 'o'],
        '2' => &['d', 't'],
        '3' => &['t'],
        '4' => &['c', 'f'],
        '5' => &['c', 'f'],
        _ => &[],
    };
    sounds.extend_from_slice(alike);
    if acronym {
        // The letter's Spanish name: «efe», «ele», «eme», «ene», «erre», «ese», «equis», «hache», «ge».
        if matches!(first, 'f' | 'l' | 'm' | 'n' | 'r' | 's' | 'x') {
            sounds.push('e');
        }
        if first == 'h' {
            sounds.push('a');
        }
    }
    sounds
}

/// Whether what was heard begins the way `term` is said.
fn sounds_like_start_of(heard: &str, term: &str) -> bool {
    heard.chars().find(|c| c.is_alphanumeric()).is_some_and(|c| first_sounds(term).contains(&c))
}

/// `dictated` with the changes of `proposal` that can be trusted applied (see
/// the module doc), keeping the dictated punctuation around each change.
pub fn accept(dictated: &str, proposal: &str, vocabulary: &[String]) -> String {
    let said = words(dictated);
    let proposed = words(proposal);
    if said.is_empty() {
        return dictated.to_string();
    }
    let pairs = align(&said, &proposed);
    // Each dictated word's fate: kept, dropped, or replaced by a term.
    let mut replacement: Vec<Option<Option<String>>> = vec![None; said.len()];
    let mut hunks = Vec::new();
    let (mut si, mut pi) = (0, 0);
    for &(a, b) in pairs.iter().chain(std::iter::once(&(said.len(), proposed.len()))) {
        if a > si || b > pi {
            hunks.push((si, a, pi, b));
        }
        si = a + 1;
        pi = b + 1;
    }
    for (s0, s1, p0, p1) in hunks {
        let heard = &said[s0..s1];
        let meant: Vec<&str> = proposed[p0..p1].iter().map(|w| split_punctuation(w.token).1).collect();
        if heard.is_empty() {
            continue; // the model added words: never taken
        }
        if meant.is_empty() {
            if is_echo(heard, &said[s1..]) {
                for slot in &mut replacement[s0..s1] {
                    *slot = Some(None);
                }
            }
            continue;
        }
        // A term the model put in place of several words may have swallowed a
        // Spanish word next to them («en tres d si hay» → «en 3D hay»): the
        // span shrinks until its edges are not such words, which stay.
        let (mut s0, mut s1) = (s0, s1);
        while s1 - s0 > 1 && FUNCTION_WORDS.contains(&said[s1 - 1].key.as_str()) {
            s1 -= 1;
        }
        while s1 - s0 > 1 && FUNCTION_WORDS.contains(&said[s0].key.as_str()) {
            s0 += 1;
        }
        let heard = &said[s0..s1];
        let meant_text = meant.join(" ");
        let heard_text: String = heard.iter().map(|w| w.key.as_str()).collect::<Vec<_>>().join(" ");
        let term = (heard.len() <= LONGEST_REPLACED)
            .then(|| known_term(&meant_text, vocabulary))
            .flatten()
            .filter(|term| sounds_like_start_of(&heard_text, term))
            .filter(|_| heard_text.chars().filter(|c| c.is_alphanumeric()).count() >= 2)
            .filter(|_| known_term(&heard_text, vocabulary).is_none());
        // Only a term of the vocabulary: a model also "fixes" real words into
        // invented ones («disculpe» → «desculpe», «agarrando» → «agarrrando»).
        let Some(term) = term else { continue };
        let new = term.to_string();
        replacement[s0] = Some(Some(new));
        for slot in &mut replacement[s0 + 1..s1] {
            *slot = Some(None);
        }
    }

    // Rebuilt from the dictated tokens, so their punctuation stays.
    let mut out: Vec<String> = Vec::with_capacity(said.len());
    let mut carried_prefix = String::new();
    let mut capitalize_next = false;
    let mut i = 0;
    while i < said.len() {
        let (prefix, core, suffix) = split_punctuation(said[i].token);
        match &replacement[i] {
            None => {
                let mut core = core.to_string();
                if capitalize_next {
                    core = capitalized(&core);
                }
                out.push(format!("{carried_prefix}{prefix}{core}{suffix}"));
                carried_prefix.clear();
                capitalize_next = false;
                i += 1;
            }
            Some(new) => {
                // The span ends where the run of replaced/dropped words ends.
                let mut end = i + 1;
                while end < said.len() && matches!(replacement[end], Some(None)) {
                    end += 1;
                }
                let last_suffix = split_punctuation(said[end - 1].token).2;
                match new {
                    Some(term) => {
                        out.push(format!("{carried_prefix}{prefix}{term}{last_suffix}"));
                        carried_prefix.clear();
                        capitalize_next = false;
                    }
                    None => {
                        // Dropped: its opening punctuation and its capital
                        // go to the next word kept; its closing one is kept.
                        carried_prefix.push_str(prefix);
                        capitalize_next |= core.chars().next().is_some_and(char::is_uppercase) && out.is_empty()
                            || core.chars().next().is_some_and(char::is_uppercase)
                                && out.last().is_some_and(|l| l.ends_with(['.', '?', '!']));
                        if !last_suffix.is_empty() {
                            if let Some(last) = out.last_mut() {
                                last.push_str(last_suffix);
                            }
                        }
                    }
                }
                i = end;
            }
        }
    }
    out.join(" ")
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn vocab() -> Vec<String> {
        vocabulary(&["Poliana".to_string()])
    }

    // Every proposal below is what qwen2.5:3b really answered for this user's
    // dictations (eval of 25 sep 2026), and what is kept of it.

    #[test]
    fn a_self_correction_and_a_technical_term_are_taken() {
        assert_eq!(
            accept(
                "Puedes una hacer una versión desde la cámara del desde la vista.",
                "Puedes hacer una versión desde la cámara desde la vista.",
                &vocab()
            ),
            "Puedes hacer una versión desde la cámara desde la vista."
        );
        assert_eq!(
            accept(
                "Vas a ejecutar una actualización de la nueva versión de Now the Yess.",
                "Vas a ejecutar una actualización de la nueva versión de Next.js.",
                &vocab()
            ),
            "Vas a ejecutar una actualización de la nueva versión de Next.js."
        );
    }

    #[test]
    fn a_model_that_answers_or_summarizes_changes_nothing() {
        let dictated = "Ayúdame para que en verdad mejore y agregar si necesita.";
        let answered =
            "Claro, por favor proporciona el texto que necesitas que pueda ayudar a mejorar y añadir las instrucciones necesarias.";
        assert_eq!(accept(dictated, answered, &vocab()), dictated);

        let long =
            "Créame la imagen de fondo y voy a poner sobre esa imagen el tablero, entonces créame una imagen limpia.";
        assert_eq!(accept(long, "Créame una imagen limpia.", &vocab()), long, "a cut is not an echo");
    }

    #[test]
    fn a_change_of_person_or_meaning_is_not_taken() {
        let dictated = "Quiero que mi código esté funcionando mejor sin vox.";
        let proposed = "Quiero que tu código funcione mejor sin VOx.";
        assert_eq!(accept(dictated, proposed, &vocab()), dictated);
    }

    #[test]
    fn only_the_term_is_taken_from_a_proposal_that_also_rewrote_the_rest() {
        assert_eq!(
            accept(
                "Crea una rama que se llame y actualiza a Now the Yess.",
                "Crea una rama llamada `y` y actualiza a Next.js.",
                &vocab()
            ),
            "Crea una rama que se llame y actualiza a Next.js."
        );
    }

    #[test]
    fn a_real_word_the_model_rewrites_into_another_is_not_taken() {
        // Seen with qwen2.5:7b on this user's dictations.
        for (said, proposed) in [
            ("Es el único disculpe.", "Es el único desculpe."),
            ("Ellos comandan el jefe.", "Ellos commandan el jefe."),
            ("Porque no está agarrando el texto.", "Porque no está agarrrando el texto."),
        ] {
            assert_eq!(accept(said, proposed, &vocab()), said, "{proposed}");
        }
    }

    #[test]
    fn a_long_word_respelled_closely_is_taken_a_short_one_is_not() {
        assert_eq!(
            accept(
                "ya reproduciéndose randalizándose esa toma",
                "ya reproduciéndose randomizándose esa toma",
                &vocab()
            ),
            "ya reproduciéndose randomizándose esa toma"
        );
        assert_eq!(accept("tengo que usar", "tienes que usar", &vocab()), "tengo que usar");
        // Seen with qwen2.5:3b: the pronoun dropped is another sentence.
        assert_eq!(
            accept("antes pregúntale si ya llegó", "antes pregunta si ya llegó", &vocab()),
            "antes pregúntale si ya llegó"
        );
    }

    #[test]
    fn a_dropped_first_word_hands_its_capital_to_the_next() {
        assert_eq!(accept("Una hacer una prueba.", "Hacer una prueba.", &vocab()), "Hacer una prueba.");
    }

    #[test]
    fn a_term_keeps_the_punctuation_around_the_words_it_replaces() {
        assert_eq!(accept("¿Arreglas los books?", "¿Arreglas los bugs?", &vocab()), "¿Arreglas los bugs?");
        assert_eq!(accept("en tres d, desde arriba", "en 3D, desde arriba", &vocab()), "en 3D, desde arriba");
    }

    #[test]
    fn a_term_that_does_not_sound_like_what_was_said_is_not_taken() {
        // Each of these was proposed by qwen2.5:3b for this user's dictations.
        for (said, proposed) in [
            ("¿Qué se puede hacer?", "¿JSON se puede hacer?"),
            ("Y homologas las reglas.", "JSON homologas las reglas."),
            ("desde la vista de uno", "desde la UI de uno"),
            ("la guía de Qwen que ya tienes", "la guía de Next.js que ya tienes"),
        ] {
            assert_eq!(accept(said, proposed, &vocab()), said, "{proposed}");
        }
        // …while these sound like the term and are taken.
        assert_eq!(
            accept("la mejora de la uy quiero", "la mejora de la UI quiero", &vocab()),
            "la mejora de la UI quiero"
        );
        assert_eq!(accept("un ejemplo de la UA.", "un ejemplo de la UI.", &vocab()), "un ejemplo de la UI.");
        assert_eq!(accept("pásalo a yeison", "pásalo a JSON", &vocab()), "pásalo a JSON");
    }

    #[test]
    fn a_spanish_word_swallowed_by_a_term_is_given_back() {
        assert_eq!(
            accept("el renderizador en tres d si hay una forma", "el renderizador en 3D hay una forma", &vocab()),
            "el renderizador en 3D si hay una forma"
        );
    }

    #[test]
    fn another_form_of_the_same_word_is_not_a_spelling_fix() {
        for (said, proposed) in [
            ("hagamos que funcione perfectamente", "hagamos que funcione correctamente"),
            ("y ahorita la reconstruyes", "y ahorita la reconstruimos"),
            ("Ayúdame para que en verdad", "Ayúdale para que en verdad"),
        ] {
            assert_eq!(accept(said, proposed, &vocab()), said, "{proposed}");
        }
        // A grammar fix that is not a technical term is left to the user: too
        // many «fixes» of real words turned out to be invented ones.
        assert_eq!(accept("otro modelo que queras", "otro modelo que quieras", &vocab()), "otro modelo que queras");
    }

    #[test]
    fn a_negation_is_never_dropped_as_an_echo() {
        assert_eq!(accept("no noto nada raro", "noto nada raro", &vocab()), "no noto nada raro");
    }

    #[test]
    fn only_a_server_on_this_mac_is_used() {
        assert!(Polisher::new("http://127.0.0.1:11434/v1", "m").is_some());
        assert!(Polisher::new("https://api.openai.com/v1", "m").is_none());
        assert_eq!(Polisher::new("http://127.0.0.1:11434/v1", "m").unwrap().root, "http://127.0.0.1:11434");
    }

    #[test]
    fn nothing_answering_leaves_the_text_as_it_was() {
        let p = Polisher::new("http://127.0.0.1:1/v1", "m").unwrap();
        assert_eq!(p.polish("hola mundo", &vocab(), Duration::from_millis(300)), "hola mundo");
    }
}
