#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! Turns a raw STT transcript into pasted, polished text.
//!
//! This is the pipeline from `docs/PLAN.md` §2A/§3: universal filler removal
//! → personal dictionary correction → context-aware formatting. Every stage
//! is independently testable (see each module's unit tests); [`clean`] wires
//! them together the way `eva-worker` uses them in production.

pub mod apple_intelligence;
pub mod dictionary;
mod faithfulness;
pub mod filler;
pub mod formatter;
mod normalize;
pub mod remote;
pub mod style;

pub use apple_intelligence::AppleIntelligenceFormatter;
pub use dictionary::Dictionary;
pub use formatter::{warm_up, FormatError, Formatter, RuleOnlyFormatter};
pub use normalize::fold_diacritics;
pub use remote::{OpenAiCompatibleFormatter, RemoteAssisted};
pub use style::Style;

/// The result of running [`clean`]: both the intermediate and final text are
/// kept, because `docs/PLAN.md` §3 fase 3's corpus harvesting stores the raw,
/// dictionary-corrected, and formatted forms of every transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanedTranscript {
    /// The text exactly as the STT engine produced it.
    pub raw: String,
    /// After universal filler removal and dictionary correction, before formatting.
    pub pre_formatted: String,
    /// The final, pasted text.
    pub formatted: String,
}

/// Runs the full text pipeline with the default style — see [`clean_styled`].
pub fn clean(raw: &str, dictionary: &Dictionary, formatter: &dyn Formatter) -> CleanedTranscript {
    clean_styled(raw, dictionary, formatter, Style::Default)
}

/// Runs the full text pipeline: strip universal fillers, correct against the
/// personal dictionary, then hand off to `formatter` for the context-aware
/// pass in the `style` of the app the text is going into. If `formatter`
/// fails for any reason, falls back to [`RuleOnlyFormatter`] (in the same
/// style) rather than losing the transcript — this is the "never silent"
/// rule from `docs/PLAN.md` §3.3 point 5, enforced in code rather than left
/// as a convention to remember.
pub fn clean_styled(raw: &str, dictionary: &Dictionary, formatter: &dyn Formatter, style: Style) -> CleanedTranscript {
    let after_fillers = filler::remove_universal_fillers(raw);
    let pre_formatted = dictionary.correct(&after_fillers, 0.88);

    let formatted = match formatter.format_styled(&pre_formatted, style) {
        Ok(text) if !text.trim().is_empty() || pre_formatted.trim().is_empty() => text,
        _ => {
            // Either the formatter errored, or it returned empty output for
            // non-empty input (an InvalidOutput case worth degrading from
            // too) — fall back rather than paste nothing.
            #[allow(clippy::expect_used)] // RuleOnlyFormatter::format_styled never returns Err
            RuleOnlyFormatter.format_styled(&pre_formatted, style).expect("RuleOnlyFormatter never fails")
        }
    };

    CleanedTranscript { raw: raw.to_string(), pre_formatted, formatted }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    struct AlwaysFails;
    impl Formatter for AlwaysFails {
        fn format(&self, _text: &str) -> Result<String, FormatError> {
            Err(FormatError::Unavailable("test double".into()))
        }
    }

    struct AlwaysEmpty;
    impl Formatter for AlwaysEmpty {
        fn format(&self, _text: &str) -> Result<String, FormatError> {
            Ok(String::new())
        }
    }

    #[test]
    fn full_pipeline_removes_fillers_corrects_dictionary_and_formats() {
        let dict = Dictionary::new(["García"]);
        let result = clean("eh mándale el archivo a Garcia", &dict, &RuleOnlyFormatter);
        assert_eq!(result.raw, "eh mándale el archivo a Garcia");
        assert_eq!(result.pre_formatted, "mándale el archivo a García");
        assert_eq!(result.formatted, "Mándale el archivo a García.");
    }

    #[test]
    fn falls_back_to_rule_only_formatter_when_the_real_one_errors() {
        let dict = Dictionary::new(Vec::<String>::new());
        let result = clean("hola mundo", &dict, &AlwaysFails);
        // Degraded, but never silent: still capitalized and punctuated.
        assert_eq!(result.formatted, "Hola mundo.");
    }

    #[test]
    fn falls_back_when_the_real_formatter_returns_empty_for_nonempty_input() {
        let dict = Dictionary::new(Vec::<String>::new());
        let result = clean("hola mundo", &dict, &AlwaysEmpty);
        assert_eq!(result.formatted, "Hola mundo.");
    }

    #[test]
    fn the_style_reaches_the_fallback_too() {
        let dict = Dictionary::new(Vec::<String>::new());
        let terminal = clean_styled("git status", &dict, &AlwaysFails, Style::Terminal);
        assert_eq!(terminal.formatted, "git status", "a terminal must not get a capital or a period even on fallback");
        let casual = clean_styled("voy para allá", &dict, &AlwaysFails, Style::Casual);
        assert_eq!(casual.formatted, "Voy para allá");
    }

    #[test]
    fn empty_input_produces_empty_output_at_every_stage() {
        let dict = Dictionary::new(Vec::<String>::new());
        let result = clean("", &dict, &RuleOnlyFormatter);
        assert_eq!(result.formatted, "");
    }
}
