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

/// How the wake word was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeMatch {
    /// As written in the config (accents and case aside).
    Exact,
    /// A spelling this user's speech has been confirmed to produce ("adam").
    Learned,
    /// Close to it ("adam", "agan" for "Adán") but not known: only good
    /// enough when what follows is clearly a command.
    Similar,
}

/// The wake word found at the start of a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake<'a> {
    /// What follows it, as [`strip_wake_word`] gives it.
    pub rest: &'a str,
    /// How it was heard.
    pub how: WakeMatch,
    /// The first word as heard, folded (no accents, lowercase): what is
    /// remembered when it turns out to be right.
    pub heard: String,
}

fn is_vowel(c: char) -> bool {
    "aeiou".contains(c)
}

/// How far a heard word may be from the wake word, in single-letter edits:
/// one for short words, two for long ones. A four-letter word two edits away
/// is a different word.
fn tolerated_edits(wake_len: usize) -> usize {
    if wake_len <= 5 {
        1
    } else {
        2
    }
}

/// Finds the wake word at the start of `text`, however it was heard: exactly,
/// as one of the `learned` spellings, or merely close to it. Never decides
/// whether a [`WakeMatch::Similar`] is good enough — that depends on what
/// follows, which is the caller's to judge.
pub fn find_wake_word<'a>(text: &'a str, wake_word: &str, learned: &[String]) -> Option<Wake<'a>> {
    let folded_wake = fold_diacritics(wake_word.trim()).to_lowercase();
    if let Some(rest) = strip_wake_word(text, wake_word) {
        return Some(Wake { rest, how: WakeMatch::Exact, heard: folded_wake });
    }
    let trimmed = text.trim_start();
    let end = trimmed.find(|c: char| !c.is_alphanumeric()).unwrap_or(trimmed.len());
    let heard = fold_diacritics(&trimmed[..end]).to_lowercase();
    if heard.chars().count() < 3 || folded_wake.is_empty() {
        return None;
    }
    let rest = trimmed[end..].trim_start_matches(|c: char| !c.is_alphanumeric());
    if learned.contains(&heard) {
        return Some(Wake { rest, how: WakeMatch::Learned, heard });
    }
    let (first_heard, first_wake) = (heard.chars().next(), folded_wake.chars().next());
    // Vowels are what speech recognition swaps most ("Eva" → "Ava"); a
    // different first consonant is a different word.
    let same_start =
        first_heard == first_wake || first_heard.zip(first_wake).is_some_and(|(a, b)| is_vowel(a) && is_vowel(b));
    let close = same_start && strsim::levenshtein(&heard, &folded_wake) <= tolerated_edits(folded_wake.chars().count());
    close.then_some(Wake { rest, how: WakeMatch::Similar, heard })
}

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
    #[test]
    fn a_wake_word_close_to_the_real_one_is_found_as_similar_and_a_far_one_is_not() {
        for heard in ["Adam, abre Spotify", "Agan abre Spotify", "adan abre Spotify"] {
            let found = find_wake_word(heard, "Adán", &[]).unwrap_or_else(|| panic!("{heard}"));
            assert_eq!(found.rest, "abre Spotify", "{heard}");
        }
        assert_eq!(find_wake_word("Adam abre Spotify", "Adán", &[]).unwrap().how, WakeMatch::Similar);
        assert_eq!(find_wake_word("Adán abre Spotify", "Adán", &[]).unwrap().how, WakeMatch::Exact);
        for far in ["Hola, abre Spotify", "Ah, Dan, abre", "Ana abre Spotify", "Abre Spotify", "Ad abre"] {
            assert_eq!(find_wake_word(far, "Adán", &[]), None, "{far}");
        }
    }

    #[test]
    fn eva_is_also_heard_as_ava_or_eba_but_not_as_a_different_consonant() {
        for heard in ["Ava, abre Spotify", "Eba abre Spotify", "Eve, abre Spotify"] {
            assert!(find_wake_word(heard, "Eva", &[]).is_some(), "{heard}");
        }
        for other in ["Seva abre", "Ella abre", "Ver abre", "Tres abre Spotify"] {
            assert_eq!(find_wake_word(other, "Eva", &[]).map(|w| w.how), None, "{other}");
        }
    }

    #[test]
    fn a_learned_spelling_is_found_as_learned_even_if_it_is_far() {
        let learned = vec!["atan".to_string(), "eydan".to_string()];
        assert_eq!(find_wake_word("Eydan, abre", "Adán", &learned).unwrap().how, WakeMatch::Learned);
    }

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
