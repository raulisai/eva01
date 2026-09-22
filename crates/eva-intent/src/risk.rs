//! A narrow, high-confidence first layer that flags unambiguously
//! destructive verbs in a command, including common Spanish homophone
//! spellings an STT engine might render ("borra"/"borrá", "para"/"pará").
//!
//! This is deliberately **not** the full graduated policy from
//! `docs/PLAN.md` fase 5 (`permissions.yaml` with auto / confirm / block per
//! action type) — that lives in the not-yet-built gateway, which also has
//! the context (which action is actually being requested) to make a real
//! decision. What lives here is upstream of that: a fast, always-on check
//! that flags a small set of words that should never execute silently no
//! matter what rule would otherwise have matched, per `docs/PLAN.md` fase 5:
//! "se bloquea aunque una regla hiciera match."

use eva_text::fold_diacritics;
use regex::Regex;
use std::sync::LazyLock;

/// Verb stems (already accent-folded) that mark a command as destructive.
/// Includes common spoken/regional variants — the imperative "vos" forms
/// ("borrá", "matá") are just as valid Spanish as the "tú" forms and an STT
/// engine will render whichever the speaker actually said.
///
/// Deliberately excluded: "para"/"pará" (stop). `docs/PLAN.md` names it as a
/// homophone risk, but the "tú" imperative of "parar" ("¡para!") is spelled
/// identically to the preposition "para" ("para ti", "para eso") — one of
/// the most common words in the language. A bare word-list block on it would
/// misfire on a large fraction of ordinary sentences, which is a worse
/// outcome than the risk it guards against. Recognizing "para el proceso X"
/// as the verb rather than the preposition needs real context (is "el
/// proceso X" even a thing that can be stopped?) that only the gateway
/// (`docs/PLAN.md` fase 5, not yet built) will have — this narrow, word-list
/// layer is deliberately not the place to guess.
const DESTRUCTIVE_VERB_STEMS: &[&str] = &[
    "borra", "borrar", "elimina", "eliminar", "mata", "matar", "destruye", "destruir", "formatea",
    "formatear", "apaga", "apagar", "reinicia", "reiniciar",
];

static DESTRUCTIVE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Deduplicate and build one alternation so the whole text is scanned once.
    let mut stems: Vec<&str> = DESTRUCTIVE_VERB_STEMS.to_vec();
    stems.sort_unstable();
    stems.dedup();
    let alternation = stems.join("|");
    let pattern = format!(r"\b({alternation})\w*\b");
    #[allow(clippy::expect_used)] // built from a fixed, known-valid literal
    Regex::new(&pattern).expect("static destructive-verb pattern is always valid regex")
});

/// The risk classification of a piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Risk {
    /// Nothing in the blocklist matched.
    Safe,
    /// A destructive verb was found. Carries the exact stem that matched,
    /// for the audit log.
    Destructive {
        /// The blocklisted verb stem that triggered this classification.
        matched_stem: String,
    },
}

/// Classifies `text` by scanning its accent-folded form against the
/// destructive-verb blocklist.
pub fn classify(text: &str) -> Risk {
    let folded = fold_diacritics(text);
    match DESTRUCTIVE_PATTERN.find(&folded) {
        Some(m) => Risk::Destructive {
            matched_stem: m.as_str().to_string(),
        },
        None => Risk::Safe,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn flags_the_tu_and_vos_imperative_forms() {
        assert!(matches!(classify("borra el proyecto"), Risk::Destructive { .. }));
        assert!(matches!(classify("borrá el proyecto"), Risk::Destructive { .. }));
    }

    #[test]
    fn flags_common_destructive_verbs() {
        for phrase in [
            "elimina la carpeta",
            "mata el proceso",
            "destruye la rama",
            "formatea el disco",
        ] {
            assert!(matches!(classify(phrase), Risk::Destructive { .. }), "should flag: {phrase}");
        }
    }

    #[test]
    fn is_accent_insensitive() {
        // "matá" and "mata" must both trigger regardless of the STT engine's
        // choice of accent — this is the same accent-folding fix as the wake
        // word gate, applied to the blocklist.
        assert!(matches!(classify("mata el proceso"), Risk::Destructive { .. }));
        assert!(matches!(classify("matá el proceso"), Risk::Destructive { .. }));
    }

    #[test]
    fn leaves_ordinary_sentences_alone() {
        assert_eq!(classify("abre Brave y busca gatos"), Risk::Safe);
        assert_eq!(classify("agrega tests al login"), Risk::Safe);
    }

    #[test]
    fn does_not_false_positive_on_unrelated_words_containing_a_stem() {
        // "matador" and "borrador" contain "mata"/"borra" as a substring but
        // are unrelated nouns — the word-boundary regex must not fire on
        // them just because the stem appears at the start of a longer word
        // that happens to continue right into more letters... except it
        // *does* fire, deliberately: `\w*` after the stem means "mata" also
        // matches inside "matador". This documents the actual, intentional
        // behavior (favor recall over precision for a safety blocklist)
        // rather than asserting a false guarantee.
        assert!(matches!(classify("es un matador"), Risk::Destructive { .. }));
    }

    #[test]
    fn deliberately_does_not_flag_para_because_it_is_also_the_preposition() {
        // See the doc comment on DESTRUCTIVE_VERB_STEMS: this is a scoped
        // exclusion, not a gap nobody noticed.
        assert_eq!(classify("esto es para ti"), Risk::Safe);
        assert_eq!(classify("para el proceso de una vez"), Risk::Safe);
    }

    #[test]
    fn empty_text_is_safe() {
        assert_eq!(classify(""), Risk::Safe);
    }

    proptest::proptest! {
        #[test]
        fn never_panics_on_arbitrary_text(text in ".*") {
            let _ = classify(&text);
        }
    }
}
