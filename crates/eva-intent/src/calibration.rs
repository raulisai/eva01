//! What a guided calibration teaches: the user says a few commands, the speech
//! recognizer writes down what it heard, and the difference between the two
//! is what to remember — the way *this* person's wake word and app names come
//! out. Pure and testable; recording and storing are the worker's.

use crate::apps::AppIndex;
use crate::intent::{self, Intent};
use eva_text::fold_diacritics;
use std::collections::HashMap;

/// One thing the user was asked to say, and what came out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The app the command was about, if it was about one.
    pub expected_app: Option<String>,
    /// What the recognizer heard, wake word included.
    pub heard: String,
}

/// What to remember.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lessons {
    /// Spellings of the wake word heard, with how many samples produced each.
    pub wake: Vec<(String, usize)>,
    /// App names heard for an app: `(heard, app)`.
    pub apps: Vec<(String, String)>,
    /// Samples that taught nothing: nothing recognizable was heard.
    pub unheard: usize,
}

/// How close, by spelling, a heard name must be to the app's to be learned as another name for it.
const MIN_ALIKE: f64 = 0.6;

/// A spelling heard this many times in one calibration is the user's way of
/// saying the wake word, not a fluke.
pub const CERTAIN_AFTER: usize = 2;

/// Works out what `samples` teach, given the `wake_word` and the installed apps.
///
/// A first word that is not the wake word counts as a spelling of it only
/// when what follows is clearly a command: someone who said "Eva, abre
/// Spotify" and got "Ava abre Spotify" said the wake word as "Ava", but "Ava
/// es Potty Pye" tells nothing.
pub fn lessons(wake_word: &str, index: &AppIndex, samples: &[Sample]) -> Lessons {
    let folded_wake = fold_diacritics(wake_word.trim()).to_lowercase();
    let mut wake: HashMap<String, usize> = HashMap::new();
    let mut apps: Vec<(String, String)> = Vec::new();
    let mut unheard = 0;

    for sample in samples {
        let text = sample.heard.trim();
        let first_end = text.find(|c: char| !c.is_alphanumeric()).unwrap_or(text.len());
        let first = fold_diacritics(&text[..first_end]).to_lowercase();
        let rest = text[first_end..].trim_start_matches(|c: char| !c.is_alphanumeric());
        let command = intent::parse(rest, index);
        if first.is_empty() || !command.is_clear_command() {
            unheard += 1;
            continue;
        }
        if first != folded_wake {
            *wake.entry(first).or_default() += 1;
        }
        let Some(expected) = &sample.expected_app else { continue };
        let heard_name = match &command {
            Intent::ConfirmApp { heard, app, .. } if app == expected => Some(heard),
            Intent::AppNotFound { name, .. } => Some(name),
            _ => None,
        };
        if let Some(name) = heard_name {
            let name = fold_diacritics(name).to_lowercase();
            // A slip of the recognizer is not a name for the app: something
            // wholly different ("Photoshop" for "Spotify") would break the
            // other app the day it is installed.
            let alike = strsim::jaro_winkler(&name, &fold_diacritics(expected).to_lowercase()) >= MIN_ALIKE;
            if alike && !apps.iter().any(|(known, _)| *known == name) {
                apps.push((name, expected.clone()));
            }
        }
    }

    let mut wake: Vec<(String, usize)> = wake.into_iter().collect();
    wake.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Lessons { wake, apps, unheard }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::apps::AppEntry;

    fn heard(app: Option<&str>, text: &str) -> Sample {
        Sample { expected_app: app.map(str::to_string), heard: text.to_string() }
    }

    fn index() -> AppIndex {
        AppIndex::new(vec![AppEntry::new("Spotify"), AppEntry::new("Brave Browser").with_aliases(["brave"])])
    }

    #[test]
    fn a_way_of_saying_the_wake_word_that_repeats_is_learned_and_the_right_one_is_not() {
        let samples = [
            heard(Some("Spotify"), "Ava, abre Spotify."),
            heard(Some("Spotify"), "Eva, abre Spotify."),
            heard(None, "Ava busca el clima de mañana."),
            heard(Some("Brave Browser"), "Eba abre Brave."),
        ];
        let lessons = lessons("Eva", &index(), &samples);
        assert_eq!(lessons.wake, vec![("ava".to_string(), 2), ("eba".to_string(), 1)]);
        assert_eq!(lessons.unheard, 0);
    }

    #[test]
    fn garbage_after_the_first_word_teaches_nothing() {
        let samples = [heard(Some("Spotify"), "Ava es Potty Pye."), heard(None, "")];
        let lessons = lessons("Eva", &index(), &samples);
        assert_eq!(lessons.wake, Vec::new());
        assert_eq!(lessons.unheard, 2);
    }

    #[test]
    fn a_misheard_app_name_is_learned_only_for_the_app_that_was_asked() {
        let samples = [
            heard(Some("Brave Browser"), "Eva, abre breve."),
            heard(Some("Spotify"), "Eva, abre Spotifi."),
            heard(Some("Spotify"), "Eva, abre Photoshop."),
        ];
        let lessons = lessons("Eva", &index(), &samples);
        assert!(lessons.apps.contains(&("breve".to_string(), "Brave Browser".to_string())), "{lessons:?}");
        assert!(lessons.apps.contains(&("spotifi".to_string(), "Spotify".to_string())), "{lessons:?}");
        assert!(!lessons.apps.iter().any(|(heard, _)| heard == "photoshop"), "a wholly different word is a slip");
    }
}
