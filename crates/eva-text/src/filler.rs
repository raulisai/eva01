//! Removes pure interjection filler ("ehh", "mmm", "uhm"…) from a transcript.
//!
//! This is deliberately narrow. `docs/PLAN.md` §2A documents why: words like
//! Spanish "este", "pues" or "bueno" are also legitimate lexical words
//! ("este coche", "pues claro"), so a blind word-list removal corrupts real
//! sentences. Only sounds that are *never* a word in Spanish or English are
//! removed here; distinguishing "este" the filler from "este" the
//! demonstrative is left to the context-aware formatter (the `Formatter`
//! trait in [`crate::formatter`]), which sees the whole sentence.

use regex::Regex;
use std::sync::LazyLock;

/// Interjections that are not a lexical word in Spanish or English in any
/// position, so removing them can never change a sentence's meaning.
const UNIVERSAL_FILLERS: &[&str] =
    &["eh", "ehh", "ehm", "ehem", "ah", "ahh", "ahm", "uh", "uhh", "uhm", "umm", "hmm", "hm", "mmm", "mm"];

/// One compiled case-insensitive, whole-word regex per filler, built once.
/// Whole-word matching (`\b…\b`) is what stops "ah" from matching inside
/// "ahora"; trailing punctuation is folded into the match so "ehh," collapses
/// to nothing rather than leaving a stray comma.
static FILLER_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    UNIVERSAL_FILLERS
        .iter()
        .map(|word| {
            let pattern = format!(r"(?i)\b{}\b[,.]?\s*", regex::escape(word));
            #[allow(clippy::expect_used)] // the pattern is built from a fixed, known-valid literal
            Regex::new(&pattern).expect("static filler pattern is always valid regex")
        })
        .collect()
});

/// Strips universal interjection fillers from `text` and collapses the
/// whitespace left behind. Never removes anything that could be a real word.
pub fn remove_universal_fillers(text: &str) -> String {
    let mut result = text.to_string();
    for pattern in FILLER_PATTERNS.iter() {
        result = pattern.replace_all(&result, "").to_string();
    }
    crate::normalize::collapse_whitespace(&result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn removes_pure_interjections_in_spanish_and_english() {
        assert_eq!(remove_universal_fillers("eh mándale el archivo a Juan"), "mándale el archivo a Juan");
        assert_eq!(remove_universal_fillers("send it, uhm, tomorrow"), "send it, tomorrow");
    }

    #[test]
    fn does_not_touch_real_words_that_contain_a_filler_as_a_substring() {
        // "ah" must not match inside "ahora"; "eh" must not match inside "echo".
        assert_eq!(remove_universal_fillers("ahora mismo"), "ahora mismo");
        assert_eq!(remove_universal_fillers("hazlo con eco"), "hazlo con eco");
    }

    #[test]
    fn leaves_ambiguous_spanish_words_alone() {
        // "este", "pues", "bueno" are legitimate lexical words in Spanish and
        // are NOT in the universal list on purpose — see the module doc.
        let text = "este coche es bueno, pues claro que sí";
        assert_eq!(remove_universal_fillers(text), text);
    }

    #[test]
    fn collapses_the_whitespace_a_removed_filler_leaves_behind() {
        assert_eq!(remove_universal_fillers("hola   eh   mundo"), "hola mundo");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(remove_universal_fillers(""), "");
    }
}
