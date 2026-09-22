//! The wake-word gate: is this transcript a command, or just dictation?
//!
//! This is the fix designed from day 1 for the bug documented in
//! `docs/PLAN.md` §2A, Hallazgo 4: a plain case-folded string comparison
//! fails silently the moment the STT engine renders the wake word without
//! its accent ("adan" instead of "adán"), and a missed wake word means the
//! command gets pasted as text into whatever document is open instead of
//! being executed — the single most expensive failure mode in the whole
//! project. [`strip_wake_word`] compares with [`eva_text::fold_diacritics`],
//! so "Adán", "adan", "ADÁN", and "Adán," all gate identically.

use eva_text::fold_diacritics;

/// If `text` begins with `wake_word` as a whole word (accent-insensitive,
/// case-insensitive), returns the remainder of `text` with the wake word and
/// any punctuation/whitespace immediately after it stripped. Returns `None`
/// if `text` does not start with the wake word — the caller should treat
/// that as plain dictation.
///
/// "Whole word" means the character right after the wake word (if any) is
/// not alphanumeric, so a wake word of "Adán" does not fire on "Adánica" —
/// mirroring the correctness the upstream project's own prefix gate got
/// right, without inheriting the accent bug it also has.
pub fn strip_wake_word<'a>(text: &'a str, wake_word: &str) -> Option<&'a str> {
    let wake_word = wake_word.trim();
    if wake_word.is_empty() {
        return None;
    }

    let folded_text = fold_diacritics(text.trim_start());
    let folded_wake = fold_diacritics(wake_word);

    if !folded_text.starts_with(&folded_wake) {
        return None;
    }

    // Folding can change byte length (NFD decomposition, case changes), so
    // the match position found in the folded string cannot be used to slice
    // the original `text` directly. Instead, walk `text`'s own characters,
    // consuming exactly as many as the wake word has, then confirm the next
    // character (if any) is not alphanumeric before accepting the match.
    let trimmed = text.trim_start();
    let wake_char_count = wake_word.chars().count();
    let mut chars = trimmed.char_indices();
    let mut consumed = 0;
    let mut boundary = trimmed.len();

    for (idx, ch) in &mut chars {
        if consumed == wake_char_count {
            boundary = idx;
            if ch.is_alphanumeric() {
                // The character right after the wake word is still part of
                // a longer word (e.g. "Adánica") — not a real match.
                return None;
            }
            break;
        }
        consumed += 1;
    }

    if consumed < wake_char_count {
        // `trimmed` was shorter than the wake word itself, even though the
        // folded prefix matched — can only happen if folding drops
        // characters (combining marks), which never changes character
        // *count* enough to hit this in practice, but it is handled rather
        // than indexed into blindly.
        return None;
    }

    // Skip any punctuation/whitespace separating the wake word from the rest
    // ("Adán, abre Brave" → "abre Brave").
    let remainder = trimmed[boundary..].trim_start_matches(|c: char| !c.is_alphanumeric());
    Some(remainder)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn matches_with_the_accent() {
        assert_eq!(strip_wake_word("Adán, abre Brave", "Adán"), Some("abre Brave"));
    }

    #[test]
    fn matches_without_the_accent_this_is_the_headline_fix() {
        assert_eq!(strip_wake_word("adan abre brave", "Adán"), Some("abre brave"));
        assert_eq!(strip_wake_word("ADAN abre brave", "Adán"), Some("abre brave"));
    }

    #[test]
    fn does_not_match_a_longer_word_with_the_wake_word_as_a_prefix() {
        assert_eq!(strip_wake_word("Adánica es un nombre", "Adán"), None);
    }

    #[test]
    fn does_not_match_when_the_wake_word_is_not_at_the_start() {
        assert_eq!(strip_wake_word("oye Adán abre Brave", "Adán"), None);
    }

    #[test]
    fn strips_punctuation_between_the_wake_word_and_the_command() {
        assert_eq!(strip_wake_word("Adán... abre Brave", "Adán"), Some("abre Brave"));
        assert_eq!(strip_wake_word("Adán: abre Brave", "Adán"), Some("abre Brave"));
    }

    #[test]
    fn plain_dictation_with_no_wake_word_returns_none() {
        assert_eq!(strip_wake_word("mañana voy a abrir Brave", "Adán"), None);
    }

    #[test]
    fn a_bare_wake_word_with_nothing_after_it_returns_an_empty_remainder() {
        assert_eq!(strip_wake_word("Adán", "Adán"), Some(""));
    }

    #[test]
    fn empty_wake_word_never_matches_anything() {
        assert_eq!(strip_wake_word("cualquier cosa", ""), None);
    }

    #[test]
    fn leading_whitespace_on_the_transcript_is_tolerated() {
        assert_eq!(strip_wake_word("   Adán, abre Brave", "Adán"), Some("abre Brave"));
    }

    proptest::proptest! {
        #[test]
        fn never_panics_on_arbitrary_text_and_wake_word(text in ".*", wake_word in ".*") {
            let _ = strip_wake_word(&text, &wake_word);
        }
    }
}
