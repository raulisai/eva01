//! Mending the punctuation a long transcript picks up where it was stitched.
//!
//! A long recording is transcribed in pieces cut at its pauses
//! (`eva-audio::segment`), and the speech model punctuates every piece as if
//! it were a whole utterance: a capital on its first word, often a full stop
//! at its end. Joined back, a real 70 s dictation read "…del contrato nuevo.
//! porque todavía faltan…" and "…durante la conversación Por otro lado…" —
//! punctuation that contradicts itself, and that the formatter then trusted
//! as sentence boundaries.
//!
//! Two seams are unambiguous in written Spanish and mended here:
//! - a full stop followed by a lower-case word: the lower case is the
//!   model's own reading of the words, the stop is only where a piece ended;
//! - a capitalized function word ("Por", "Porque", "Y") in the middle of a
//!   sentence: such words are never proper nouns, so the capital is only
//!   where a piece began.

/// Words that are never proper nouns, so a capital on one in mid-sentence is
/// always a mistake.
const FUNCTION_WORDS: &[&str] = &[
    "a", "al", "de", "del", "en", "con", "por", "para", "sin", "sobre", "entre", "hasta", "desde", "hacia", "y", "e",
    "o", "u", "ni", "pero", "porque", "aunque", "sino", "que", "si", "como", "cuando", "donde", "mientras", "así",
    "entonces", "luego", "después", "además", "también", "ya", "pues", "el", "la", "los", "las", "un", "una", "unos",
    "unas", "lo", "le", "les", "se", "me", "te", "nos", "mi", "mis", "tu", "tus", "su", "sus", "este", "esta", "estos",
    "estas", "ese", "esa", "eso", "esto", "muy", "más", "no",
];

/// Abbreviations end in a full stop and may be followed by anything ("etc.
/// y más"); a word this short before a stop is left alone.
const LONGEST_ABBREVIATION: usize = 3;

/// `text` with the two self-contradicting seams above mended. Words and their
/// order never change; only a full stop may go and a first letter may lower.
pub(crate) fn mend_sentence_seams(text: &str) -> String {
    let mut tokens: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    for i in 1..tokens.len() {
        let next_starts_lower = first_letter(&tokens[i]).is_some_and(char::is_lowercase);
        let previous = &tokens[i - 1];
        let spurious_stop = next_starts_lower
            && previous.ends_with('.')
            && !previous.ends_with("..")
            && core(previous).chars().count() > LONGEST_ABBREVIATION;
        if spurious_stop {
            tokens[i - 1].pop();
            continue;
        }

        let mid_sentence = !tokens[i - 1].ends_with(['.', '?', '!', '…', ':', '"', '»']);
        let word = core(&tokens[i]);
        let capitalized_function_word = first_letter(&tokens[i]).is_some_and(char::is_uppercase)
            && word.chars().skip(1).all(|c| !c.is_uppercase())
            && FUNCTION_WORDS.contains(&word.to_lowercase().as_str());
        if mid_sentence && capitalized_function_word {
            tokens[i] = lower_first_letter(&tokens[i]);
        }
    }
    tokens.join(" ")
}

/// The letters of a token, without the punctuation around them.
fn core(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

fn first_letter(token: &str) -> Option<char> {
    token.chars().find(|c| c.is_alphabetic())
}

fn lower_first_letter(token: &str) -> String {
    let mut done = false;
    token
        .chars()
        .flat_map(|c| {
            if !done && c.is_alphabetic() {
                done = true;
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_two_seams_of_a_real_long_dictation_are_mended() {
        assert_eq!(
            mend_sentence_seams("hablar del contrato nuevo. porque todavía faltan"),
            "hablar del contrato nuevo porque todavía faltan"
        );
        assert_eq!(
            mend_sentence_seams("durante la conversación Por otro lado el equipo"),
            "durante la conversación por otro lado el equipo"
        );
    }

    #[test]
    fn real_sentence_ends_and_proper_nouns_are_left_alone() {
        let text = "Llegó ayer. Por eso no vino. Dile a Marta que venga. ¿Vienes? Y luego qué.";
        assert_eq!(mend_sentence_seams(text), text);
        assert_eq!(mend_sentence_seams("habló con Marta sobre el plan"), "habló con Marta sobre el plan");
    }

    #[test]
    fn abbreviations_acronyms_and_colons_are_left_alone() {
        assert_eq!(mend_sentence_seams("libros, cuadernos, etc. y más"), "libros, cuadernos, etc. y más");
        assert_eq!(mend_sentence_seams("habló con la ONU ayer"), "habló con la ONU ayer");
        assert_eq!(mend_sentence_seams("el cartel decía: Por favor"), "el cartel decía: Por favor");
    }

    #[test]
    fn a_mended_seam_keeps_the_punctuation_around_the_word() {
        assert_eq!(mend_sentence_seams("dijo que «Porque sí»"), "dijo que «porque sí»");
    }

    proptest::proptest! {
        #[test]
        fn words_are_never_added_removed_or_reordered(text in "[A-Za-zñáé .,¿?]{0,80}") {
            let mended = mend_sentence_seams(&text);
            let letters = |t: &str| t.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
            proptest::prop_assert_eq!(letters(&mended), letters(&text));
        }
    }
}
