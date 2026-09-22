//! The context-aware formatting step: takes text that has already had
//! universal fillers removed and dictionary corrections applied, and turns
//! it into a polished sentence (capitalization, closing punctuation,
//! removing ambiguous filler words like Spanish "este"/"pues"/"bueno" when
//! they are used as filler rather than as real words).
//!
//! That last part genuinely needs context — a regex cannot tell "este coche"
//! from "este, no sé" — so the real implementation is an on-device LLM
//! (Apple Intelligence Foundation Models, per `docs/PLAN.md` §2A/§3.3) that
//! lives in a separate crate with its own FFI to Swift. This crate only
//! defines the [`Formatter`] boundary and ships [`RuleOnlyFormatter`]: the
//! fallback used when no context-aware formatter is available or it times
//! out, per the graceful-degradation matrix in `docs/PLAN.md` §3.3 point 5 —
//! "never produce silence, degrade to rules." `eva-worker` composes this with
//! a real LLM-backed `Formatter` implementation when one is wired in.

use thiserror::Error;

/// Something that turns already-cleaned raw text into a polished sentence.
pub trait Formatter: Send + Sync {
    /// Formats `text`, returning the polished version.
    ///
    /// # Errors
    /// Returns [`FormatError`] if formatting could not be completed — the
    /// caller is expected to fall back to the original `text` (or to
    /// [`RuleOnlyFormatter`]) rather than lose the transcript entirely.
    fn format(&self, text: &str) -> Result<String, FormatError>;
}

/// Why a [`Formatter`] failed to produce output.
#[derive(Debug, Error)]
pub enum FormatError {
    /// The formatter did not respond within its allotted time.
    #[error("formatter timed out after {0:?}")]
    Timeout(std::time::Duration),
    /// The formatter is not available on this system right now.
    #[error("formatter unavailable: {0}")]
    Unavailable(String),
    /// The formatter ran but returned something unusable (e.g. empty output
    /// for non-empty input).
    #[error("formatter returned invalid output: {0}")]
    InvalidOutput(String),
}

/// The pure-Rust fallback formatter: no LLM, no network, cannot fail.
///
/// It does the part that rules over the whole sentence can do safely
/// (capitalize the first letter, ensure closing punctuation) and
/// deliberately does **not** attempt to strip ambiguous Spanish fillers
/// ("este", "pues", "bueno") — see the module doc for why that needs
/// context. The result is less polished than the LLM path but is never
/// silent and never corrupts a sentence.
#[derive(Debug, Default, Clone, Copy)]
pub struct RuleOnlyFormatter;

impl Formatter for RuleOnlyFormatter {
    fn format(&self, text: &str) -> Result<String, FormatError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }

        // Capitalize the first *alphabetic* character, not literally the
        // first character — Spanish sentences routinely open with `¿`/`¡`,
        // which have no uppercase form, so `chars().next()` alone would
        // silently skip the real first letter forever.
        let capitalized = match trimmed.char_indices().find(|(_, c)| c.is_alphabetic()) {
            Some((idx, first_letter)) => {
                let (before, after) = trimmed.split_at(idx);
                let mut rest = after.chars();
                rest.next(); // drop the lowercase form of `first_letter`, we re-add it uppercased
                let mut out = String::with_capacity(trimmed.len());
                out.push_str(before);
                out.extend(first_letter.to_uppercase());
                out.push_str(rest.as_str());
                out
            }
            // No alphabetic character at all (pure punctuation/numbers) —
            // nothing to capitalize, pass the trimmed text through.
            None => trimmed.to_string(),
        };

        let needs_terminator = !capitalized.ends_with(['.', '?', '!', '…', ':']);
        let result = if needs_terminator {
            format!("{capitalized}.")
        } else {
            capitalized
        };

        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn capitalizes_the_first_letter() {
        let out = RuleOnlyFormatter.format("hola mundo").expect("never fails");
        assert_eq!(out, "Hola mundo.");
    }

    #[test]
    fn does_not_double_up_closing_punctuation() {
        let out = RuleOnlyFormatter
            .format("¿cómo estás?")
            .expect("never fails");
        assert_eq!(out, "¿Cómo estás?");
    }

    #[test]
    fn empty_input_produces_empty_output_not_a_stray_period() {
        let out = RuleOnlyFormatter.format("   ").expect("never fails");
        assert_eq!(out, "");
    }

    #[test]
    fn leaves_ambiguous_fillers_alone_because_it_has_no_context() {
        let out = RuleOnlyFormatter
            .format("este coche es bueno")
            .expect("never fails");
        assert_eq!(out, "Este coche es bueno.");
    }
}
