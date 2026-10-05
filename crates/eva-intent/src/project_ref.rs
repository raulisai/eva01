//! Naming the project out loud: «en BarberiaSaas, agrega tests», «para el
//! proyecto eva cero dos, arregla el login». The names are the user's own
//! folders, so the match is against the list of projects EVA knows — never
//! a guess at what a project name looks like — which is also what keeps an
//! ordinary task that starts with «en» («en el login agrega validación»)
//! from being mistaken for one: it only counts when the words after «en»
//! really are one of the projects.
//!
//! A project is recognized by its folder name written the way it is spoken:
//! the words joined («barberia saas» → `barberiassaas`), numbers said as
//! words («eva cero dos» → `eva02`), and — for what the speech model gets
//! wrong — the same Spanish-ear sound match used for app names
//! ([`crate::sound`]). Two projects that sound alike are never picked for
//! the user: the answer is "no project named", and the normal resolution
//! (window, default, last used) takes over.

use crate::sound::{sound_of, sounds_like};
use eva_text::{fold_diacritics, split_punctuation};

/// A project named at the start of a task, and what is left of the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    /// Position of the project in the list of names given.
    pub index: usize,
    /// The task without «en el proyecto X,».
    pub rest: String,
}

/// Words that introduce a project, longest first, folded. «en» alone is
/// allowed because the name that must follow is checked against real projects.
const TRIGGERS: &[&[&str]] = &[
    &["en", "el", "proyecto", "de"],
    &["en", "el", "proyecto"],
    &["en", "el", "repositorio"],
    &["en", "el", "repo"],
    &["en", "la", "carpeta"],
    &["para", "el", "proyecto"],
    &["del", "proyecto"],
    &["dentro", "de"],
    &["para"],
    &["en"],
];

/// Numbers said as words that appear in project names («eva cero dos»).
const NUMBER_WORDS: &[(&str, &str)] = &[
    ("cero", "0"),
    ("uno", "1"),
    ("dos", "2"),
    ("tres", "3"),
    ("cuatro", "4"),
    ("cinco", "5"),
    ("seis", "6"),
    ("siete", "7"),
    ("ocho", "8"),
    ("nueve", "9"),
];

/// The longest name, in spoken words, looked for after the trigger.
const LONGEST_NAME_WORDS: usize = 5;
/// Shortest project name that can be named by sound alone (shorter ones must
/// match exactly): «app» sounds like too many things.
const SOUND_MIN_LETTERS: usize = 5;
/// Words a task starts with after the name, dropped: «…, y agrega tests».
const LEADING_CONNECTORS: &[&str] = &["y", "e", "que", "pues"];

/// A name as it is compared: no accents, lowercase, only letters and digits.
fn key_of(text: &str) -> String {
    fold_diacritics(text).to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The digits a spoken number stands for, when the word is one.
fn digit_of(word: &str) -> Option<&'static str> {
    NUMBER_WORDS.iter().find(|(w, _)| *w == word).map(|(_, d)| *d)
}

/// The words of `text` joined into one comparable key, with spoken numbers
/// turned into digits.
fn spoken_key(words: &[String]) -> String {
    words.iter().map(|w| digit_of(w).map_or_else(|| w.clone(), str::to_string)).collect()
}

/// If `text` begins by naming one of `names` («en BarberiaSaas, agrega
/// tests»), which one and what is left of the task.
pub fn split_project_prefix(text: &str, names: &[String]) -> Option<ProjectRef> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = tokens.iter().map(|t| key_of(split_punctuation(t).1)).collect();
    let trigger_len = TRIGGERS
        .iter()
        .find(|trigger| words.len() > trigger.len() && trigger.iter().zip(&words).all(|(t, w)| t == w))
        .map(|t| t.len())?;

    let after = &words[trigger_len..];
    let mut best: Option<(usize, usize, u8)> = None; // (project, words used, quality: 2 exact, 1 sound)
    for (index, name) in names.iter().enumerate() {
        let name_key = key_of(name);
        if name_key.chars().count() < 3 {
            continue;
        }
        for used in 1..=LONGEST_NAME_WORDS.min(after.len()) {
            let heard = spoken_key(&after[..used]);
            let quality = if heard == name_key {
                2
            } else if name_key.chars().count() >= SOUND_MIN_LETTERS
                && sounds_like(&sound_of(&heard), &sound_of(&name_key)).is_some()
            {
                1
            } else {
                continue;
            };
            let better = match best {
                None => true,
                Some((_, best_used, best_quality)) => {
                    quality > best_quality || (quality == best_quality && used > best_used)
                }
            };
            if better {
                best = Some((index, used, quality));
            }
        }
    }
    let (index, used, quality) = best?;

    // Two projects that sound the same as what was said: not ours to pick.
    if quality == 1 {
        let heard = sound_of(&spoken_key(&after[..used]));
        let rivals = names
            .iter()
            .enumerate()
            .filter(|(i, n)| *i != index && sounds_like(&heard, &sound_of(&key_of(n))).is_some())
            .count();
        if rivals > 0 {
            return None;
        }
    }

    let rest_tokens = &tokens[trigger_len + used..];
    let mut rest = rest_tokens.join(" ");
    rest = rest.trim_start_matches(|c: char| !c.is_alphanumeric() && !matches!(c, '¿' | '¡')).to_string();
    while let Some((first, tail)) = rest.split_once(' ') {
        if LEADING_CONNECTORS.contains(&key_of(first).as_str()) {
            rest = tail.trim_start().to_string();
        } else {
            break;
        }
    }
    (!rest.trim().is_empty()).then_some(ProjectRef { index, rest })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        ["eva01", "eva02", "novoastar", "barberias-saas", "guitar-hero-app", "cleaner", "app"]
            .map(String::from)
            .to_vec()
    }

    fn named(text: &str) -> Option<(String, String)> {
        let list = names();
        split_project_prefix(text, &list).map(|r| (list[r.index].clone(), r.rest))
    }

    #[test]
    fn a_project_named_at_the_start_is_found_and_taken_out_of_the_task() {
        assert_eq!(
            named("en barberias saas, agrega tests al login"),
            Some(("barberias-saas".into(), "agrega tests al login".into()))
        );
        assert_eq!(
            named("en el proyecto novoastar arregla el header"),
            Some(("novoastar".into(), "arregla el header".into()))
        );
        assert_eq!(
            named("para el proyecto guitar hero app y agrega un menú"),
            Some(("guitar-hero-app".into(), "agrega un menú".into()))
        );
    }

    #[test]
    fn numbers_said_as_words_and_the_speech_models_spelling_both_work() {
        assert_eq!(named("en eva cero dos, revisa el panel"), Some(("eva02".into(), "revisa el panel".into())));
        assert_eq!(named("en eva cero uno arregla el dictado"), Some(("eva01".into(), "arregla el dictado".into())));
        // "novo astar", "nobo astar": what the speech model writes for it.
        assert_eq!(named("en novo astar, agrega tests"), Some(("novoastar".into(), "agrega tests".into())));
        assert_eq!(named("en nobo astar agrega tests"), Some(("novoastar".into(), "agrega tests".into())));
    }

    #[test]
    fn an_ordinary_task_that_starts_with_en_is_not_mistaken_for_a_project() {
        assert_eq!(named("en el login agrega validación"), None);
        assert_eq!(named("agrega tests en novoastar"), None, "only a project at the start counts");
        assert_eq!(named("en novoastar"), None, "a project with no task is not a task");
    }

    #[test]
    fn projects_that_sound_the_same_are_never_picked_for_the_user() {
        let list = vec!["cleaner".to_string(), "clener".to_string()];
        assert_eq!(split_project_prefix("en cliner arregla eso", &list), None);
    }

    #[test]
    fn a_short_name_must_match_exactly_never_by_sound() {
        assert_eq!(named("en apps agrega tests"), None);
        assert_eq!(named("en app agrega tests"), Some(("app".into(), "agrega tests".into())));
    }
}
