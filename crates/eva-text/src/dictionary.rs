//! Personal dictionary: fuzzy-corrects transcribed n-grams against a list of
//! words the user cares about (project names, people, technical terms).
//!
//! Unlike the upstream project this design was audited against
//! (`docs/PLAN.md` §2A, Hallazgo 3), matching here is Unicode-first: the
//! comparison key is built with [`crate::normalize::fold_diacritics`], so
//! "García" and "Garcia" compare equal instead of the accented spelling being
//! silently excluded. There is no phonetic algorithm (Soundex is English
//! letter-to-sound rules and does not generalize to Spanish); a single
//! Jaro-Winkler distance, which only looks at character-level similarity, is
//! simpler and works the same regardless of language.
//!
//! A near miss is only corrected for an entry that is *distinctive* — seven
//! letters or more, or with inner capitals or digits ("Kubernetes",
//! "ChargeBee") — and only from a word that starts with the same letter and
//! is a small edit away. Found by running dictations through it: with
//! "García", "Pedro", "Marta" and "Clara" in the dictionary, "muchas gracias"
//! became "muchas García", "mi perro" "mi Pedro", "el martes" "el Marta" and
//! "claro que sí" "Clara que sí". Short names collide with everyday words;
//! for them only the exact word, with its accents and capitals restored
//! ("garcia" → "García"), is a correction.

use crate::normalize::{collapse_whitespace, fold_diacritics, match_case, split_punctuation};

/// A word as replacements compare it: no accents, no capitals.
fn fold_word(word: &str) -> String {
    fold_diacritics(word).to_lowercase()
}

/// One entry in the personal dictionary.
struct CustomWord {
    /// The text to output when this word is matched, in its preferred form
    /// (e.g. `"García"`, accents included — this is what gets pasted).
    display: String,
    /// `fold_diacritics(display)`, precomputed once so matching never
    /// recomputes it per candidate.
    fold_key: String,
    /// Whether a near miss may be corrected to it, not only the exact word.
    distinctive: bool,
}

/// Letters an entry needs for a near miss to be corrected to it.
const DISTINCTIVE_LENGTH: usize = 7;
/// Letters a dictated word needs before it is taken for a near miss.
const SHORTEST_NEAR_MISS: usize = 5;
/// How close in edits a near miss must be, besides Jaro-Winkler.
const NEAR_MISS_LEVENSHTEIN: f64 = 0.75;

/// Whether `display` is unusual enough that a word close to it is a
/// mishearing of it rather than a different word: long, or shaped like a
/// product name (inner capitals, digits).
fn is_distinctive(display: &str, fold_key: &str) -> bool {
    let letters = fold_key.chars().filter(|c| c.is_alphabetic()).count();
    let inner_capital = display.chars().skip(1).any(char::is_uppercase);
    let digit = display.chars().any(|c| c.is_ascii_digit());
    letters >= DISTINCTIVE_LENGTH || inner_capital || digit
}

/// A personal dictionary of words to fuzzy-correct transcripts against.
pub struct Dictionary {
    words: Vec<CustomWord>,
    /// Phrases the user reported as misheard, with what they meant.
    replacements: Vec<Replacement>,
}

/// "Heard X, meant Y", taught by reporting a dictation.
struct Replacement {
    /// The folded, lowercase words of what was heard.
    heard: Vec<String>,
    meant: String,
}

/// The longest phrase a replacement can be, in words.
const LONGEST_REPLACEMENT: usize = 5;

impl Dictionary {
    /// Builds a dictionary from a list of display-form words. Empty or
    /// whitespace-only entries are dropped; there is no length or character
    /// set restriction — a word with tildes, `ñ`, or any other Unicode letter
    /// is a normal entry, not a special case.
    pub fn new<I, S>(words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let words = words
            .into_iter()
            .filter_map(|w| {
                let display = w.as_ref().trim().to_string();
                if display.is_empty() {
                    return None;
                }
                let fold_key = fold_diacritics(&display);
                let distinctive = is_distinctive(&display, &fold_key);
                Some(CustomWord { display, fold_key, distinctive })
            })
            .collect();
        Dictionary { words, replacements: Vec::new() }
    }

    /// Adds what the user taught by reporting mistakes: each `(heard, meant)`
    /// replaces the phrase `heard` — compared without accents or capitals,
    /// whole words only — by `meant` wherever it appears. Longer phrases win
    /// over shorter ones that start at the same word.
    #[must_use]
    pub fn with_replacements<I, H, M>(mut self, pairs: I) -> Self
    where
        I: IntoIterator<Item = (H, M)>,
        H: AsRef<str>,
        M: AsRef<str>,
    {
        for (heard, meant) in pairs {
            let words: Vec<String> = heard.as_ref().split_whitespace().map(fold_word).collect();
            if words.is_empty() || words.len() > LONGEST_REPLACEMENT || words.iter().any(String::is_empty) {
                continue;
            }
            self.replacements.push(Replacement { heard: words, meant: meant.as_ref().trim().to_string() });
        }
        self.replacements.sort_by_key(|r| std::cmp::Reverse(r.heard.len()));
        self
    }

    /// Applies only the learned replacements (no fuzzy dictionary matching):
    /// what a command is read with before it is interpreted.
    pub fn replace_learned(&self, text: &str) -> String {
        if self.replacements.is_empty() || text.trim().is_empty() {
            return text.to_string();
        }
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let folded: Vec<String> = tokens.iter().map(|t| fold_word(split_punctuation(t).1)).collect();
        let mut out: Vec<String> = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            let hit = self.replacements.iter().find(|r| {
                let n = r.heard.len();
                i + n <= tokens.len()
                    && folded[i..i + n] == r.heard[..]
                    // Punctuation inside the phrase means it was not one phrase.
                    && tokens[i..i + n - 1].iter().all(|t| split_punctuation(t).2.is_empty())
            });
            match hit {
                Some(r) => {
                    let n = r.heard.len();
                    let (prefix, _, _) = split_punctuation(tokens[i]);
                    let (_, _, suffix) = split_punctuation(tokens[i + n - 1]);
                    out.push(format!("{prefix}{}{suffix}", r.meant));
                    i += n;
                }
                None => {
                    out.push(tokens[i].to_string());
                    i += 1;
                }
            }
        }
        out.join(" ")
    }

    /// Returns `true` if the dictionary has no entries.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Corrects `text` in place: every 1- or 2-word window is compared
    /// against every dictionary entry (folded, so accents never block a
    /// match), and the closest match at or above `threshold` replaces it,
    /// preserving the original's leading/trailing punctuation and the case
    /// pattern of its first word.
    ///
    /// `threshold` is a Jaro-Winkler similarity in `[0.0, 1.0]`: `1.0` only
    /// accepts an exact fold match, lower values accept looser typos. `0.88`
    /// is a reasonable starting point for short technical terms and names.
    pub fn correct(&self, text: &str, threshold: f64) -> String {
        if text.trim().is_empty() || (self.words.is_empty() && self.replacements.is_empty()) {
            return text.to_string();
        }
        let learned = self.replace_learned(text);
        let text = learned.as_str();
        if self.words.is_empty() {
            return collapse_whitespace(text);
        }

        let tokens: Vec<&str> = text.split_whitespace().collect();
        let mut output: Vec<String> = Vec::with_capacity(tokens.len());
        let mut i = 0;

        while i < tokens.len() {
            match self.best_match_starting_at(&tokens, i, threshold) {
                Some((span, replacement)) => {
                    output.push(replacement);
                    i += span;
                }
                None => {
                    output.push(tokens[i].to_string());
                    i += 1;
                }
            }
        }

        collapse_whitespace(&output.join(" "))
    }

    /// Tries n-grams of length 2, then 1, starting at `start`, returning the
    /// widest one that beats `threshold`, or `None` if nothing matches well
    /// enough. Longest-first avoids a short match ("Charge") pre-empting a
    /// better multi-word one ("Charge Bee") that starts at the same token.
    fn best_match_starting_at(&self, tokens: &[&str], start: usize, threshold: f64) -> Option<(usize, String)> {
        // Only 1- and 2-word windows are considered. Two words exist purely
        // to catch a compound dictionary entry the STT engine split in two
        // ("Charge Bee" → "ChargeBee"), which is always a near-exact fold
        // match. A 3-word window adds a lot of false-positive surface for a
        // case ("García Núñez"-style multi-word names) rare enough to not be
        // worth it — simpler is also safer here, not just less code.
        let max_n = 2.min(tokens.len() - start);
        let mut best: Option<(usize, String, f64)> = None;

        for n in (1..=max_n).rev() {
            let span = &tokens[start..start + n];

            // Don't let an n-gram cross a punctuation boundary: only the
            // last token in the span may carry trailing punctuation.
            if span[..n - 1].iter().any(|t| !split_punctuation(t).2.is_empty()) {
                continue;
            }

            let cores: Vec<&str> = span.iter().map(|t| split_punctuation(t).1).collect();
            if cores.iter().any(|c| c.is_empty()) {
                continue;
            }
            let candidate_key = fold_diacritics(&cores.join(""));

            // A 2-word window concatenates two tokens with no separator, so
            // an unrelated short word glued onto the real target ("a" +
            // "garcia" = "agarcia") can look deceptively close to a
            // dictionary entry ("garcia") under plain edit-distance
            // similarity. Multi-word matches only exist for the
            // split-compound-word case, which folds to an exact or
            // near-exact match — so a high floor here does not cost the
            // legitimate case anything, and it is what rules out the false
            // positive.
            let effective_threshold = if n > 1 { threshold.max(0.95) } else { threshold };

            if let Some((word, score)) = self.closest_entry(&candidate_key) {
                if score >= effective_threshold {
                    let is_better = best.as_ref().is_none_or(|(_, _, s)| score > *s);
                    if is_better {
                        let (prefix, _, _) = split_punctuation(span[0]);
                        let (_, _, suffix) = split_punctuation(span[n - 1]);
                        let cased = match_case(cores[0], word);
                        best = Some((n, format!("{prefix}{cased}{suffix}"), score));
                    }
                }
            }
        }

        best.map(|(n, replacement, _)| (n, replacement))
    }

    /// Finds the dictionary entry whose folded key is closest to `candidate_key`.
    /// Applies a length-ratio guard first (candidates whose folded length
    /// differs by more than 30% from the entry's are skipped) so, e.g., a
    /// four-letter word never gets fuzzy-matched to a fifteen-letter one just
    /// because Jaro-Winkler is lenient on short strings.
    fn closest_entry(&self, candidate_key: &str) -> Option<(&str, f64)> {
        let candidate_len = candidate_key.chars().count() as f64;
        if candidate_len == 0.0 {
            return None;
        }

        self.words
            .iter()
            .filter_map(|entry| {
                let entry_len = entry.fold_key.chars().count() as f64;
                if entry_len == 0.0 {
                    return None;
                }
                if candidate_key == entry.fold_key {
                    return Some((entry.display.as_str(), 1.0));
                }
                let len_diff = (candidate_len - entry_len).abs();
                let max_len = candidate_len.max(entry_len);
                let near_miss = entry.distinctive
                    && candidate_len as usize >= SHORTEST_NEAR_MISS
                    && len_diff / max_len <= 0.30
                    && candidate_key.chars().next() == entry.fold_key.chars().next()
                    && strsim::normalized_levenshtein(candidate_key, &entry.fold_key) >= NEAR_MISS_LEVENSHTEIN;
                near_miss.then(|| (entry.display.as_str(), strsim::jaro_winkler(candidate_key, &entry.fold_key)))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn corrects_an_unaccented_dictation_to_the_accented_display_form() {
        let dict = Dictionary::new(["García"]);
        assert_eq!(dict.correct("hola Garcia como estas", 0.88), "hola García como estas");
    }

    #[test]
    fn a_learned_replacement_applies_to_whole_words_and_keeps_punctuation() {
        let dict =
            Dictionary::new(Vec::<String>::new()).with_replacements([("todo eso", "todo esto"), ("adam", "Adán")]);
        assert_eq!(dict.correct("Adam, abre Brave y todo eso.", 0.88), "Adán, abre Brave y todo esto.");
        assert_eq!(dict.correct("madam todo esos", 0.88), "madam todo esos");
        assert_eq!(dict.replace_learned("¿ADAM?"), "¿Adán?");
    }

    #[test]
    fn the_longer_learned_phrase_wins_and_a_broken_phrase_does_not_match() {
        let dict = Dictionary::new(Vec::<String>::new()).with_replacements([("a b", "X"), ("a b c", "Y"), ("d", "Z")]);
        assert_eq!(dict.replace_learned("a b c"), "Y");
        assert_eq!(dict.replace_learned("a, b"), "a, b");
    }

    #[test]
    fn corrects_the_reverse_direction_too() {
        let dict = Dictionary::new(["Nunez"]);
        assert_eq!(dict.correct("es de Núñez", 0.85), "es de Nunez");
    }

    #[test]
    fn preserves_case_pattern_of_the_original() {
        let dict = Dictionary::new(["García"]);
        assert_eq!(dict.correct("GARCIA", 0.88), "GARCÍA");
        // Lowercase dictation ("garcia", how everything reads before the
        // first word of a sentence) still corrects to the dictionary's own
        // canonical capitalization — a surname is not supposed to render in
        // lowercase just because speech has no concept of capital letters.
        assert_eq!(dict.correct("garcia", 0.88), "García");
    }

    #[test]
    fn preserves_surrounding_punctuation() {
        let dict = Dictionary::new(["Adán"]);
        assert_eq!(dict.correct("¿Adan?", 0.88), "¿Adán?");
        assert_eq!(dict.correct("adan,", 0.88), "Adán,");
    }

    #[test]
    fn does_not_correct_below_threshold() {
        let dict = Dictionary::new(["García"]);
        // "casa" is not remotely close to "garcía" and must be left alone.
        assert_eq!(dict.correct("mi casa es grande", 0.88), "mi casa es grande");
    }

    #[test]
    fn everyday_words_are_never_turned_into_a_name_in_the_dictionary() {
        let dict = Dictionary::new(["García", "Pedro", "Marta", "Clara", "Mirna", "Dima"]);
        for sentence in [
            "muchas gracias por todo",
            "mi perro se llama toby",
            "nos vemos el martes",
            "claro que sí",
            "mira esto",
            "dime cuándo",
        ] {
            assert_eq!(dict.correct(sentence, 0.88), sentence, "{sentence}");
        }
    }

    #[test]
    fn a_short_name_is_still_restored_when_it_is_the_same_word() {
        let dict = Dictionary::new(["García", "Pedro", "Núñez"]);
        assert_eq!(dict.correct("habla con garcia y con pedro nunez", 0.88), "habla con García y con Pedro Núñez");
    }

    #[test]
    fn a_misheard_technical_term_is_still_corrected() {
        let dict = Dictionary::new(["Kubernetes", "Supabase", "ChargeBee", "TypeScript"]);
        assert_eq!(dict.correct("el cluster de kubernetis", 0.88), "el cluster de Kubernetes");
        assert_eq!(dict.correct("la base de superbase", 0.88), "la base de Supabase");
        assert_eq!(dict.correct("migra a typescrip", 0.88), "migra a TypeScript");
    }

    #[test]
    fn a_near_miss_must_start_with_the_same_letter() {
        let dict = Dictionary::new(["Kubernetes"]);
        assert_eq!(dict.correct("gubernetes", 0.88), "gubernetes");
    }

    #[test]
    fn matches_the_longer_ngram_when_both_fit() {
        let dict = Dictionary::new(["ChargeBee"]);
        assert_eq!(dict.correct("usa Charge Bee para cobrar", 0.80), "usa ChargeBee para cobrar");
    }

    #[test]
    fn does_not_cross_a_punctuation_boundary_mid_ngram() {
        let dict = Dictionary::new(["ChargeBee"]);
        // The comma after "Charge" closes the n-gram there; "Bee" alone must
        // not be pulled in across it.
        let out = dict.correct("Charge, Bee para cobrar", 0.80);
        assert!(!out.contains("ChargeBee"));
    }

    #[test]
    fn empty_dictionary_returns_input_unchanged() {
        let dict = Dictionary::new(Vec::<String>::new());
        assert!(dict.is_empty());
        assert_eq!(dict.correct("cualquier texto", 0.9), "cualquier texto");
    }

    #[test]
    fn blank_entries_are_dropped_and_never_match_empty_tokens() {
        let dict = Dictionary::new(["", "   ", "García"]);
        assert_eq!(dict.correct("Garcia", 0.88), "García");
    }

    // Property-based tests (docs/PLAN.md §3.4): arbitrary text against an
    // arbitrary dictionary must never panic, regardless of how strange either
    // one is — empty words, punctuation-only tokens, emoji, mismatched
    // lengths. This is the guarantee that matters most for a function that
    // runs on every single thing the user ever dictates.
    proptest::proptest! {
        #[test]
        fn correct_never_panics_on_arbitrary_text_and_dictionary(
            text in ".*",
            words in proptest::collection::vec(".*", 0..5),
            threshold in 0.0f64..=1.0f64,
        ) {
            let dict = Dictionary::new(words);
            let _ = dict.correct(&text, threshold);
        }

        #[test]
        fn empty_dictionary_is_always_a_no_op(text in ".*", threshold in 0.0f64..=1.0f64) {
            let dict = Dictionary::new(Vec::<String>::new());
            proptest::prop_assert_eq!(dict.correct(&text, threshold), text);
        }
    }
}
