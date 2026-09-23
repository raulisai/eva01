//! Small text-normalization helpers shared by [`crate::filler`] and
//! [`crate::dictionary`], so the punctuation/case/diacritic handling lives in
//! exactly one place instead of being copied between the two.

use unicode_normalization::UnicodeNormalization;

/// Unicode combining diacritical marks (U+0300–U+036F). NFD-decomposing a
/// precomposed accented letter (á, ñ, ç, …) always splits it into a base
/// letter followed by one or more code points in this block, so filtering
/// them out after `.nfd()` is a complete, general "strip the accent" that
/// needs no per-language special-casing and no extra crate beyond
/// `unicode-normalization` (already a workspace dependency).
const COMBINING_MARKS_START: u32 = 0x0300;
const COMBINING_MARKS_END: u32 = 0x036F;

/// Folds a string to a diacritic-insensitive, lowercase comparison key.
///
/// This is the fix for the bug this project audited in an upstream project:
/// a comparison key that requires pure ASCII silently excludes every word
/// with a tilde or an `ñ` from fuzzy matching. Here there is no ASCII gate at
/// all — `fold_diacritics("García")` and `fold_diacritics("Garcia")` are both
/// `"garcia"`, and so are `fold_diacritics("Adán")` and `fold_diacritics("Adan")`.
///
/// Only the combining marks are dropped; every other Unicode character
/// (including full non-Latin scripts) passes through NFD decomposition and
/// lowercasing unchanged, so this degrades gracefully instead of failing
/// closed on unexpected input.
pub fn fold_diacritics(input: &str) -> String {
    input
        .nfd()
        .filter(|c| !(COMBINING_MARKS_START..=COMBINING_MARKS_END).contains(&(*c as u32)))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Splits a token into `(leading punctuation, alphanumeric core, trailing punctuation)`.
///
/// Operates on `char_indices` (not byte offsets) so multi-byte punctuation
/// such as `¿`, `¡`, or curly quotes is never split in the middle of its
/// UTF-8 encoding.
pub fn split_punctuation(token: &str) -> (&str, &str, &str) {
    let core_start = token.char_indices().find(|(_, c)| c.is_alphanumeric()).map(|(idx, _)| idx).unwrap_or(token.len());

    let core_end =
        token.char_indices().rev().find(|(_, c)| c.is_alphanumeric()).map(|(idx, c)| idx + c.len_utf8()).unwrap_or(0);

    if core_start >= core_end {
        // No alphanumeric core at all (e.g. the token is just "...").
        return (token, "", "");
    }

    (&token[..core_start], &token[core_start..core_end], &token[core_end..])
}

/// Re-applies the capitalization pattern of `original` to `replacement`:
/// all-uppercase stays all-uppercase, an initial capital is preserved on the
/// first character, and — deliberately — a lowercase `original` (ordinary
/// mid-sentence dictation, which carries no real capitalization signal at
/// all) leaves `replacement` in its own canonical casing rather than forcing
/// it to lowercase. This matters for the dictionary: a name like "García"
/// dictated as "garcia" should still correct to "García", not "garcía".
pub fn match_case(original: &str, replacement: &str) -> String {
    if !original.is_empty() && original.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) {
        return replacement.to_uppercase();
    }
    let mut chars = replacement.chars();
    match chars.next() {
        Some(first) if original.chars().next().is_some_and(char::is_uppercase) => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => replacement.to_string(),
    }
}

/// Collapses runs of two or more whitespace characters into a single space
/// and trims the ends. Cheap, run after every transformation that might
/// leave a double space behind (e.g. deleting a word from the middle of a
/// sentence).
pub fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn fold_diacritics_makes_accented_and_plain_spellings_equal() {
        assert_eq!(fold_diacritics("García"), fold_diacritics("Garcia"));
        assert_eq!(fold_diacritics("Adán"), fold_diacritics("adan"));
        assert_eq!(fold_diacritics("Íñigo"), fold_diacritics("Inigo"));
        assert_eq!(fold_diacritics("Núñez"), fold_diacritics("nunez"));
    }

    #[test]
    fn fold_diacritics_is_idempotent() {
        let once = fold_diacritics("García Núñez");
        let twice = fold_diacritics(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn fold_diacritics_leaves_plain_ascii_alone_except_case() {
        assert_eq!(fold_diacritics("Brave"), "brave");
    }

    #[test]
    fn split_punctuation_extracts_prefix_and_suffix() {
        assert_eq!(split_punctuation("dude,"), ("", "dude", ","));
        assert_eq!(split_punctuation("¿Adán?"), ("¿", "Adán", "?"));
        assert_eq!(split_punctuation("hello"), ("", "hello", ""));
        assert_eq!(split_punctuation("..."), ("...", "", ""));
    }

    #[test]
    fn match_case_preserves_all_caps_and_initial_caps() {
        assert_eq!(match_case("GARCIA", "garcía"), "GARCÍA");
        assert_eq!(match_case("Garcia", "garcía"), "García");
        assert_eq!(match_case("garcia", "garcía"), "garcía");
    }

    #[test]
    fn collapse_whitespace_removes_double_spaces_and_trims() {
        assert_eq!(collapse_whitespace("  hola   mundo  "), "hola mundo");
    }

    // Property-based tests (docs/PLAN.md §3.4): for ANY string, not just the
    // handful of examples above, folding must never panic and must be
    // idempotent. This is what actually gives confidence that emoji, CJK
    // text, or control characters arriving from a misbehaving STT model
    // can't crash the text pipeline.
    proptest::proptest! {
        #[test]
        fn fold_diacritics_never_panics_on_arbitrary_unicode(s in ".*") {
            let _ = fold_diacritics(&s);
        }

        #[test]
        fn fold_diacritics_is_idempotent_for_any_input(s in ".*") {
            let once = fold_diacritics(&s);
            let twice = fold_diacritics(&once);
            proptest::prop_assert_eq!(once, twice);
        }

        #[test]
        fn fold_diacritics_on_pure_ascii_is_just_lowercasing(s in "[a-zA-Z0-9 ]*") {
            proptest::prop_assert_eq!(fold_diacritics(&s), s.to_lowercase());
        }

        #[test]
        fn split_punctuation_never_panics_and_recombines_to_the_original(s in ".*") {
            let (prefix, core, suffix) = split_punctuation(&s);
            proptest::prop_assert_eq!(format!("{prefix}{core}{suffix}"), s);
        }

        #[test]
        fn match_case_never_panics_on_arbitrary_unicode(original in ".*", replacement in ".*") {
            let _ = match_case(&original, &replacement);
        }
    }
}
