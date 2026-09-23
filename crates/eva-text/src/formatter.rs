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

use crate::style::Style;
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

    /// Formats `text` for the app it is going into. The default ignores the
    /// style; formatters that can honor it override this.
    ///
    /// # Errors
    /// As [`Formatter::format`].
    fn format_styled(&self, text: &str, style: Style) -> Result<String, FormatError> {
        let _ = style;
        self.format(text)
    }

    /// Rewrites `text` following a spoken `instruction` ("hazlo más
    /// formal") — the edit mode of `docs/PLAN.md` fase 9. Unlike
    /// [`Formatter::format`], the words are *meant* to change.
    ///
    /// # Errors
    /// [`FormatError::Unavailable`] for a formatter that cannot rewrite
    /// (the default), or whatever went wrong for one that can.
    fn rewrite(&self, text: &str, instruction: &str) -> Result<String, FormatError> {
        let _ = (text, instruction);
        Err(FormatError::Unavailable("este formateador no sabe reescribir texto".to_string()))
    }
}

/// Runs one throwaway format so the model behind `formatter` is loaded before
/// the first real dictation. Measured on Apple Intelligence: the first call
/// took ~2.2 s and every one after ~0.7 s, so without this the user's first
/// dictation of the day is the slow one. Returns how long it took; a failure
/// is not an error here (the real call will report it).
pub fn warm_up(formatter: &dyn Formatter) -> std::time::Duration {
    let start = std::time::Instant::now();
    let _ = formatter.format("hola buenos días");
    start.elapsed()
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
        self.format_styled(text, Style::Default)
    }

    fn format_styled(&self, text: &str, style: Style) -> Result<String, FormatError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }

        if style == Style::Terminal {
            return Ok(terminal_form(trimmed));
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

        // A chat message is not closed with a period; a question or an
        // exclamation keeps its mark, and a multi-sentence message keeps the
        // periods between its sentences.
        if style == Style::Casual {
            return Ok(strip_lone_final_period(&capitalized));
        }

        let needs_terminator = !capitalized.ends_with(['.', '?', '!', '…', ':']);
        let result = if needs_terminator {
            format!("{capitalized}.")
        } else {
            capitalized
        };

        Ok(result)
    }
}

/// A command line: words only. Drops opening `¿`/`¡` and trailing
/// punctuation, and lowercases the first letter (`Git status.` → `git status`).
fn terminal_form(text: &str) -> String {
    let without_edges = text
        .trim_start_matches(['¿', '¡'])
        .trim_end_matches(['.', ',', ';', ':', '!', '?', '…'])
        .trim();
    let mut chars = without_edges.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Removes a final `.` only when it is the sentence's only period — "Voy
/// para allá." loses it, "Llego tarde. Empiecen sin mí." keeps both.
fn strip_lone_final_period(text: &str) -> String {
    match text.strip_suffix('.') {
        Some(rest) if !rest.contains('.') && !rest.ends_with('.') => rest.to_string(),
        _ => text.to_string(),
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
    fn terminal_style_is_words_only() {
        let f = |t: &str| RuleOnlyFormatter.format_styled(t, Style::Terminal).expect("never fails");
        assert_eq!(f("Git status."), "git status");
        assert_eq!(f("¿Cómo estás?"), "cómo estás");
        assert_eq!(f("cd código, "), "cd código");
    }

    #[test]
    fn casual_style_drops_a_lone_final_period_but_keeps_marks_that_mean_something() {
        let f = |t: &str| RuleOnlyFormatter.format_styled(t, Style::Casual).expect("never fails");
        assert_eq!(f("voy para allá"), "Voy para allá");
        assert_eq!(f("¿vienes?"), "¿Vienes?");
        assert_eq!(f("qué bien!"), "Qué bien!");
        assert_eq!(f("llego tarde. empiecen sin mí."), "Llego tarde. empiecen sin mí.");
    }

    #[test]
    fn formal_and_default_styles_close_every_sentence() {
        for style in [Style::Default, Style::Formal] {
            let out = RuleOnlyFormatter.format_styled("gracias por tu tiempo", style).expect("never fails");
            assert_eq!(out, "Gracias por tu tiempo.");
        }
    }

    #[test]
    fn a_formatter_that_does_not_override_rewrite_says_so_instead_of_pretending() {
        let result = RuleOnlyFormatter.rewrite("hola", "hazlo formal");
        assert!(matches!(result, Err(FormatError::Unavailable(_))));
    }

    #[test]
    fn leaves_ambiguous_fillers_alone_because_it_has_no_context() {
        let out = RuleOnlyFormatter
            .format("este coche es bueno")
            .expect("never fails");
        assert_eq!(out, "Este coche es bueno.");
    }
}
