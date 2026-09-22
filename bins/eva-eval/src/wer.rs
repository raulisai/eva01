//! Word Error Rate: the standard speech-recognition accuracy metric,
//! `docs/PLAN.md` fase 3's diagnostic number (never a release gate on its
//! own — see `docs/PLAN.md` §7 for why p95 latency and daily rework count
//! are the numbers that actually block a release).
//!
//! `WER = (substitutions + deletions + insertions) / reference_word_count`,
//! computed via the standard word-level Levenshtein alignment.

/// The result of comparing one hypothesis transcript against its reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WerResult {
    /// Words in the reference that were replaced by a different word.
    pub substitutions: usize,
    /// Words in the reference missing from the hypothesis entirely.
    pub deletions: usize,
    /// Words in the hypothesis that are not in the reference.
    pub insertions: usize,
    /// Word count of the reference — the denominator.
    pub reference_len: usize,
}

impl WerResult {
    /// The error rate itself, in `[0.0, +∞)` (it can exceed `1.0` if the
    /// hypothesis has far more insertions than the reference has words).
    /// `0.0` for an empty reference with an empty hypothesis (a perfect,
    /// vacuous match); an empty reference with any hypothesis text is
    /// defined as `1.0` per word inserted, avoiding a division by zero.
    pub fn rate(&self) -> f64 {
        if self.reference_len == 0 {
            return if self.insertions == 0 { 0.0 } else { 1.0 };
        }
        (self.substitutions + self.deletions + self.insertions) as f64 / self.reference_len as f64
    }
}

/// Computes the word-level edit distance between `reference` and
/// `hypothesis`, tokenizing both on whitespace. Case-sensitive and
/// punctuation-sensitive by design — normalize both strings the same way
/// before calling this if that is not what a particular comparison wants
/// (the eval corpus's own reference transcripts should already be written
/// in the exact casing/punctuation the pipeline is expected to produce).
pub fn word_error_rate(reference: &str, hypothesis: &str) -> WerResult {
    let ref_words: Vec<&str> = reference.split_whitespace().collect();
    let hyp_words: Vec<&str> = hypothesis.split_whitespace().collect();

    let n = ref_words.len();
    let m = hyp_words.len();

    // Standard DP edit-distance table, but tracking which of the three
    // operations was used at each cell so the final counts can be
    // recovered by walking the table back from `(n, m)`, not just the
    // total distance.
    let mut dist = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dist.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dist[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            if ref_words[i - 1] == hyp_words[j - 1] {
                dist[i][j] = dist[i - 1][j - 1];
            } else {
                let substitution = dist[i - 1][j - 1] + 1;
                let deletion = dist[i - 1][j] + 1;
                let insertion = dist[i][j - 1] + 1;
                dist[i][j] = substitution.min(deletion).min(insertion);
            }
        }
    }

    let (mut i, mut j) = (n, m);
    let (mut substitutions, mut deletions, mut insertions) = (0, 0, 0);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && ref_words[i - 1] == hyp_words[j - 1] {
            i -= 1;
            j -= 1;
            continue;
        }
        let sub_cost = if i > 0 && j > 0 { dist[i - 1][j - 1] } else { usize::MAX };
        let del_cost = if i > 0 { dist[i - 1][j] } else { usize::MAX };
        let ins_cost = if j > 0 { dist[i][j - 1] } else { usize::MAX };

        // Tie-break order matters: on equal cost, prefer a deletion or an
        // insertion over a substitution. Substituting on a tie can pair up
        // two words that don't actually correspond (e.g. matching the
        // reference's last word against an unrelated extra word at the end
        // of the hypothesis, instead of recognizing the extra word as its
        // own insertion and letting the real match happen one step later).
        // Preferring indel keeps the alignment "hugging the diagonal",
        // which produces the decomposition a human would actually write
        // down — substitution is only taken when it is strictly cheaper.
        if del_cost <= sub_cost && del_cost <= ins_cost && i > 0 {
            deletions += 1;
            i -= 1;
        } else if ins_cost <= sub_cost && j > 0 {
            insertions += 1;
            j -= 1;
        } else if i > 0 && j > 0 {
            substitutions += 1;
            i -= 1;
            j -= 1;
        } else if i > 0 {
            deletions += 1;
            i -= 1;
        } else {
            insertions += 1;
            j -= 1;
        }
    }

    WerResult { substitutions, deletions, insertions, reference_len: n }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn identical_strings_have_zero_error() {
        let result = word_error_rate("hola mundo cruel", "hola mundo cruel");
        assert_eq!(result.rate(), 0.0);
        assert_eq!(result.substitutions, 0);
        assert_eq!(result.deletions, 0);
        assert_eq!(result.insertions, 0);
    }

    #[test]
    fn a_single_substitution_is_counted_correctly() {
        let result = word_error_rate("hola mundo cruel", "hola mundo feliz");
        assert_eq!(result.substitutions, 1);
        assert_eq!(result.deletions, 0);
        assert_eq!(result.insertions, 0);
        assert_eq!(result.rate(), 1.0 / 3.0);
    }

    #[test]
    fn a_missing_word_is_a_deletion() {
        let result = word_error_rate("hola mundo cruel", "hola cruel");
        assert_eq!(result.deletions, 1);
        assert_eq!(result.rate(), 1.0 / 3.0);
    }

    #[test]
    fn an_extra_word_is_an_insertion() {
        let result = word_error_rate("hola mundo", "hola mundo cruel");
        assert_eq!(result.insertions, 1);
        // Insertions are normalized by the REFERENCE length (2), not the
        // hypothesis length — WER can exceed 1.0 when the hypothesis is
        // much longer than the reference, and that is the correct,
        // standard definition, not a bug to clamp away.
        assert_eq!(result.rate(), 0.5);
    }

    #[test]
    fn completely_different_strings_are_all_substitutions_when_lengths_match() {
        let result = word_error_rate("uno dos tres", "cuatro cinco seis");
        assert_eq!(result.substitutions, 3);
        assert_eq!(result.rate(), 1.0);
    }

    #[test]
    fn both_empty_is_a_perfect_vacuous_match() {
        let result = word_error_rate("", "");
        assert_eq!(result.rate(), 0.0);
    }

    #[test]
    fn empty_reference_with_hypothesis_text_is_defined_not_a_division_by_zero() {
        let result = word_error_rate("", "algo inesperado");
        assert_eq!(result.rate(), 1.0);
    }

    #[test]
    fn is_case_and_punctuation_sensitive_by_design() {
        let result = word_error_rate("Hola.", "hola");
        assert_eq!(result.substitutions, 1, "callers must normalize first if that is not desired");
    }

    #[test]
    fn a_realistic_mixed_case_matches_the_hand_computed_alignment() {
        // Reference: "el rápido zorro marrón salta" (5 words)
        // Hypothesis: "el rápido sorro salta ahora" — "zorro"→"sorro" (sub),
        // "marrón" deleted, "ahora" inserted at the end.
        let result = word_error_rate("el rápido zorro marrón salta", "el rápido sorro salta ahora");
        assert_eq!(result.substitutions, 1);
        assert_eq!(result.deletions, 1);
        assert_eq!(result.insertions, 1);
        assert_eq!(result.rate(), 3.0 / 5.0);
    }

    proptest::proptest! {
        #[test]
        fn never_panics_on_arbitrary_text(reference in ".*", hypothesis in ".*") {
            let _ = word_error_rate(&reference, &hypothesis);
        }

        #[test]
        fn rate_is_never_negative(reference in ".*", hypothesis in ".*") {
            let result = word_error_rate(&reference, &hypothesis);
            proptest::prop_assert!(result.rate() >= 0.0);
        }
    }
}
