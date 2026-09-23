//! Is a model's "formatted" text still the same words the user dictated?
//!
//! A formatting pass may capitalize, punctuate, spell numbers as digits and
//! drop filler words — nothing else. Anything more is the model *answering*
//! or *rewriting* instead of formatting, and the words are the user's: it is
//! better to paste a plainer text than a fluent one they did not say. Found
//! by measuring the on-device model against a corpus of real dictation
//! (`eval/format_corpus.txt`): with a naive prompt it answered short
//! greetings like a chatbot, and even with a good one it occasionally
//! changed a verb ("llego" → "Llegué"), invented an accent ("Quédamos") or
//! dropped a word ("son doscientos…" → "Doscientos…") — none of which a
//! simple "not longer than the input" check can see. So the check is on the
//! words themselves:
//!
//! 1. every word in the output was in the input (exactly, ignoring case),
//!    save for the accents on question words the model is asked to restore;
//! 2. every input word missing from the output is a filler, or a number or
//!    symbol word that the output visibly turned into digits or symbols;
//! 3. no chat artifacts: markdown, extra lines, or SHOUTING.
//!
//! Pure functions over strings, so every one of those observed failures is a
//! unit test here without needing the model.

use crate::normalize::fold_diacritics;
use std::collections::HashSet;

/// Words a formatter may remove because they carry no meaning
/// (`docs/PLAN.md` §2A: "quítalas solo si no cambian el significado").
const FILLERS: &[&str] = &["eh", "ehm", "em", "mm", "mmm", "este", "pues", "bueno", "o", "sea", "osea", "digo"];

/// Question words whose accent the model is asked to add: dictation often
/// arrives without it ("cuando vas a llegar" → "¿Cuándo vas a llegar?").
const QUESTION_WORDS: &[&str] = &[
    "qué", "cómo", "cuándo", "dónde", "quién", "quiénes", "cuál", "cuáles", "cuánto", "cuánta", "cuántos", "cuántas",
];

/// Words that turn into digits when the model writes numbers as figures
/// ("diez y media" → "10:30"). Only droppable when the output has a digit.
const NUMBER_WORDS: &[&str] = &[
    "cero", "un", "uno", "una", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve", "diez", "once",
    "doce", "trece", "catorce", "quince", "dieciséis", "diecisiete", "dieciocho", "diecinueve", "veinte",
    "veintiuno", "veintidós", "veintitrés", "veinticuatro", "veinticinco", "veintiséis", "veintisiete",
    "veintiocho", "veintinueve", "treinta", "cuarenta", "cincuenta", "sesenta", "setenta", "ochenta", "noventa",
    "cien", "ciento", "doscientos", "doscientas", "trescientos", "trescientas", "cuatrocientos", "cuatrocientas",
    "quinientos", "quinientas", "seiscientos", "seiscientas", "setecientos", "setecientas", "ochocientos",
    "ochocientas", "novecientos", "novecientas", "mil", "millón", "millones", "y", "media", "cuarto", "con",
];

/// Words that turn into symbols ("ana arroba ejemplo punto com" →
/// "ana@ejemplo.com"). Only droppable when the output has such a symbol.
const SYMBOL_WORDS: &[&str] = &[
    "arroba", "punto", "puntos", "coma", "guion", "guión", "barra", "por", "ciento", "porciento", "más", "menos",
    "igual", "dólar", "dólares", "euro", "euros", "signo", "dos",
];

/// Characters a spoken symbol word becomes.
const SYMBOL_CHARS: &[char] = &['@', '.', ':', ',', '/', '%', '$', '€', '-', '+', '=', '_'];

/// Checks that `output` is a faithful formatting of `input`.
///
/// # Errors
/// The reason it is not, in words fit for a log line.
pub(crate) fn check_format(input: &str, output: &str) -> Result<(), String> {
    check_artifacts(input, output)?;

    let input_words = words(input);
    let output_words = words(output);
    if output_words.len() > input_words.len() {
        return Err(format!(
            "la respuesta tiene más palabras ({}) que el dictado ({})",
            output_words.len(),
            input_words.len()
        ));
    }

    let input_set: HashSet<&str> = input_words.iter().map(String::as_str).collect();
    let output_set: HashSet<&str> = output_words.iter().map(String::as_str).collect();
    let input_folded: HashSet<String> = input_words.iter().map(|w| fold_diacritics(w)).collect();

    for word in output_words.iter().filter(|w| has_letter(w)) {
        if input_set.contains(word.as_str()) {
            continue;
        }
        let accent_restored = QUESTION_WORDS.contains(&word.as_str()) && input_folded.contains(&fold_diacritics(word));
        if !accent_restored {
            return Err(format!("la respuesta tiene la palabra «{word}», que no estaba en el dictado"));
        }
    }

    let output_has_digit = output.chars().any(|c| c.is_ascii_digit());
    let output_has_symbol = output.chars().any(|c| SYMBOL_CHARS.contains(&c));
    for word in input_words.iter().filter(|w| has_letter(w) && !output_set.contains(w.as_str())) {
        let word = word.as_str();
        // An accent restored on a question word replaced the input's plain
        // spelling, so the plain one is legitimately "missing".
        let replaced_by_accented = output_set.iter().any(|o| QUESTION_WORDS.contains(o) && fold_diacritics(o) == word);
        let allowed = FILLERS.contains(&word)
            || replaced_by_accented
            || (output_has_digit && NUMBER_WORDS.contains(&word))
            || (output_has_symbol && SYMBOL_WORDS.contains(&word));
        if !allowed {
            return Err(format!("la respuesta perdió la palabra «{word}», que no es una muletilla"));
        }
    }

    Ok(())
}

/// Chat-assistant leftovers that no formatting of one dictated line can
/// legitimately contain.
fn check_artifacts(input: &str, output: &str) -> Result<(), String> {
    if output.contains(['*', '`', '#']) && !input.contains(['*', '`', '#']) {
        return Err("la respuesta trae formato de markdown".to_string());
    }
    if output.contains('\n') && !input.contains('\n') {
        return Err("la respuesta ocupa varias líneas".to_string());
    }
    let letters: Vec<char> = output.chars().filter(|c| c.is_alphabetic()).collect();
    let input_is_shouting = input.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
    if letters.len() > 3 && letters.iter().all(|c| c.is_uppercase()) && !input_is_shouting {
        return Err("la respuesta está toda en mayúsculas".to_string());
    }
    Ok(())
}

/// Lowercased words: runs of letters and digits, so `pedro@ejemplo.com`
/// yields `pedro`, `ejemplo`, `com` and `¿Cómo` yields `cómo`.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn has_letter(word: &str) -> bool {
    word.chars().any(char::is_alphabetic)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn ok(input: &str, output: &str) {
        assert_eq!(check_format(input, output), Ok(()), "{input:?} → {output:?} should be accepted");
    }

    fn rejected(input: &str, output: &str) -> String {
        check_format(input, output).expect_err(&format!("{input:?} → {output:?} should be rejected"))
    }

    #[test]
    fn plain_capitalization_and_punctuation_are_faithful() {
        ok("hola cómo estás", "Hola, ¿cómo estás?");
        ok("gracias", "Gracias.");
        ok("que buena idea vamos a la playa", "Que buena idea, vamos a la playa.");
        ok("juan garcía viene con su hermana", "Juan García viene con su hermana.");
    }

    #[test]
    fn restoring_the_accent_of_a_question_word_is_the_point_of_the_formatter() {
        ok("cuando vas a llegar a la oficina", "¿Cuándo vas a llegar a la oficina?");
        ok("que hora es", "¿Qué hora es?");
    }

    #[test]
    fn an_accent_added_to_an_ordinary_word_is_not_a_correction_the_formatter_may_make() {
        // Accents are the STT engine's job and it gets them right; the
        // model adding one to "estas" or "quedamos" is a guess, and a wrong
        // guess is worse than the plain word.
        rejected("hola como estas", "Hola, ¿cómo estás?");
    }

    #[test]
    fn dropping_a_filler_is_faithful() {
        ok("o sea este mandale el archivo a juan", "Mandale el archivo a Juan.");
        ok("bueno la verdad no tengo idea", "La verdad no tengo idea.");
    }

    #[test]
    fn numbers_and_symbols_spoken_out_may_become_figures() {
        ok("son veinte tareas", "Son 20 tareas.");
        ok("la reunión es a las diez y media", "La reunión es a las 10:30.");
        ok("mi correo es ana arroba ejemplo punto com", "Mi correo es ana@ejemplo.com.");
    }

    // ---- the real failures found by measuring the model ----

    #[test]
    fn a_changed_verb_is_rejected() {
        // "llego en diez minutos" → "Llegué en diez minutos." (same length).
        let reason = rejected("llego en diez minutos", "Llegué en diez minutos.");
        assert!(reason.contains("llegué"), "{reason}");
    }

    #[test]
    fn an_invented_accent_on_an_ordinary_word_is_rejected() {
        // "quedamos a las cinco" → "Quédamos a las cinco."
        let reason = rejected("quedamos a las cinco", "Quédamos a las cinco.");
        assert!(reason.contains("quédamos"), "{reason}");
    }

    #[test]
    fn a_silently_dropped_word_is_rejected() {
        // "son doscientos cincuenta dólares por mes" → "Doscientos cincuenta…"
        let reason = rejected("son doscientos cincuenta dólares por mes", "Doscientos cincuenta dólares por mes.");
        assert!(reason.contains("son"), "{reason}");
    }

    #[test]
    fn content_lost_around_a_converted_email_is_rejected() {
        // "mi correo es pedro arroba ejemplo punto com" → "**Pedro@ejemplo.com**."
        let reason = rejected("mi correo es pedro arroba ejemplo punto com", "Pedro@ejemplo.com.");
        assert!(reason.contains("correo") || reason.contains("mi"), "{reason}");
    }

    #[test]
    fn a_chatbot_reply_is_rejected_whatever_its_length() {
        rejected("gracias", "¡Gracias! Estoy aquí para ayudarte con cualquier otra cosa que necesites.");
        rejected("cuando sale el vuelo a madrid", "El vuelo sale a las 10:30 de la mañana.");
        rejected("perdón por la demora", "¡No te preocupes!");
    }

    #[test]
    fn markdown_extra_lines_and_shouting_are_rejected() {
        assert!(rejected("mi correo es ana", "**Mi correo es ana.**").contains("markdown"));
        assert!(rejected("hola a todos", "Hola\n\na todos").contains("líneas"));
        assert!(rejected("llego en diez minutos", "LLEGO EN DIEZ MINUTOS.").contains("mayúsculas"));
    }

    #[test]
    fn a_dictation_that_was_already_uppercase_may_stay_so() {
        ok("SOS", "SOS");
    }

    #[test]
    fn number_words_are_only_droppable_when_a_digit_appeared() {
        rejected("son veinte tareas", "Son tareas.");
        ok("son veinte tareas", "Son 20 tareas.");
    }

    #[test]
    fn empty_output_for_empty_input_is_faithful() {
        ok("", "");
    }

    proptest::proptest! {
        #[test]
        fn never_panics_on_arbitrary_text(input in ".*", output in ".*") {
            let _ = check_format(&input, &output);
        }

        #[test]
        fn text_is_always_a_faithful_version_of_itself_with_punctuation_added(input in "[a-záéíóúñ ]{0,60}") {
            let punctuated = format!("¿{}?", input.trim());
            proptest::prop_assert_eq!(check_format(&input, &punctuated), Ok(()));
        }
    }
}
