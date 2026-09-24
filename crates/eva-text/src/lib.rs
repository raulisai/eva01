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
mod pieces;
pub mod remote;
mod repair;
mod seams;
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
    let pre_formatted = seams::mend_sentence_seams(&dictionary.correct(&after_fillers, 0.88));

    // A long dictation goes to the formatter a piece at a time (see
    // `pieces`): a piece that fails falls back alone, not the whole text.
    let pieces = pieces::split_for_formatting(&pre_formatted);
    let formatted_pieces: Vec<String> =
        pieces.iter().map(|piece| format_or_fall_back(formatter, &piece.text, style)).collect();
    let formatted = pieces::stitch(&pieces, &formatted_pieces);

    CleanedTranscript { raw: raw.to_string(), pre_formatted, formatted }
}

fn format_or_fall_back(formatter: &dyn Formatter, text: &str, style: Style) -> String {
    match formatter.format_styled(text, style) {
        Ok(formatted) if !formatted.trim().is_empty() || text.trim().is_empty() => formatted,
        // Either the formatter errored, or it returned empty output for
        // non-empty input (an InvalidOutput case worth degrading from
        // too) — fall back rather than paste nothing.
        _ => fall_back(text, style),
    }
}

fn fall_back(text: &str, style: Style) -> String {
    #[allow(clippy::expect_used)] // RuleOnlyFormatter::format_styled never returns Err
    RuleOnlyFormatter.format_styled(text, style).expect("RuleOnlyFormatter never fails")
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

    /// Formats like a good model would, but refuses one particular piece, and
    /// remembers every piece it was given.
    struct FailsOn {
        marker: &'static str,
        seen: std::sync::Mutex<Vec<String>>,
    }
    impl Formatter for FailsOn {
        fn format(&self, text: &str) -> Result<String, FormatError> {
            self.seen.lock().unwrap().push(text.to_string());
            if text.contains(self.marker) {
                return Err(FormatError::InvalidOutput("no".into()));
            }
            RuleOnlyFormatter.format(text).map(|t| format!("«{t}»"))
        }
    }

    #[test]
    fn a_long_dictation_is_formatted_in_pieces_and_one_bad_piece_falls_back_alone() {
        let text = (0..100).map(|i| format!("p{i}")).collect::<Vec<_>>().join(" ");
        let formatter = FailsOn { marker: "p60", seen: Default::default() };
        let result = clean(&text, &Dictionary::new(Vec::<String>::new()), &formatter);

        let seen = formatter.seen.lock().unwrap();
        assert_eq!(seen.len(), 4, "four pieces of 25 words");
        assert!(seen.iter().all(|p| p.split_whitespace().count() <= 30));
        // The piece with p60 fell back to rules; the other three kept the model's version.
        assert_eq!(result.formatted.matches('«').count(), 3, "{}", result.formatted);
        assert_eq!(
            result
                .formatted
                .replace(['«', '»', '.', ','], "")
                .split_whitespace()
                .map(str::to_lowercase)
                .collect::<Vec<_>>(),
            text.split_whitespace().map(str::to_string).collect::<Vec<_>>(),
            "every word survives, in order"
        );
    }

    #[test]
    fn a_short_dictation_is_still_one_call() {
        let formatter = FailsOn { marker: "nunca", seen: Default::default() };
        clean("hola qué tal", &Dictionary::new(Vec::<String>::new()), &formatter);
        assert_eq!(formatter.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn empty_input_produces_empty_output_at_every_stage() {
        let dict = Dictionary::new(Vec::<String>::new());
        let result = clean("", &dict, &RuleOnlyFormatter);
        assert_eq!(result.formatted, "");
    }
}
