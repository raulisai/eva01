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
use std::collections::{HashMap, HashSet};

/// Words a formatter may remove because they carry no meaning
/// (`docs/PLAN.md` §2A: "quítalas solo si no cambian el significado").
pub(crate) const FILLERS: &[&str] = &["eh", "ehm", "em", "mm", "mmm", "este", "pues", "bueno", "osea", "digo"];

/// "o sea" is a filler only as a pair: alone, "o" is the conjunction ("¿hoy
/// o mañana?") and "sea" a verb ("que sea rápido"), and dropping either
/// changes what was said.
const PAIRED_FILLER: (&str, &str) = ("o", "sea");

/// How many times the input says "o sea" — that many "o" and "sea" may go.
fn paired_filler_count(words: &[String]) -> usize {
    words.windows(2).filter(|pair| pair[0] == PAIRED_FILLER.0 && pair[1] == PAIRED_FILLER.1).count()
}

/// Question words whose accent the model is asked to add: dictation often
/// arrives without it ("cuando vas a llegar" → "¿Cuándo vas a llegar?").
const QUESTION_WORDS: &[&str] =
    &["qué", "cómo", "cuándo", "dónde", "quién", "quiénes", "cuál", "cuáles", "cuánto", "cuánta", "cuántos", "cuántas"];

/// Words that turn into digits when the model writes numbers as figures
/// ("diez y media" → "10:30"). Only droppable when the output has a digit.
const NUMBER_WORDS: &[&str] = &[
    "cero",
    "un",
    "uno",
    "una",
    "dos",
    "tres",
    "cuatro",
    "cinco",
    "seis",
    "siete",
    "ocho",
    "nueve",
    "diez",
    "once",
    "doce",
    "trece",
    "catorce",
    "quince",
    "dieciséis",
    "diecisiete",
    "dieciocho",
    "diecinueve",
    "veinte",
    "veintiuno",
    "veintidós",
    "veintitrés",
    "veinticuatro",
    "veinticinco",
    "veintiséis",
    "veintisiete",
    "veintiocho",
    "veintinueve",
    "treinta",
    "cuarenta",
    "cincuenta",
    "sesenta",
    "setenta",
    "ochenta",
    "noventa",
    "cien",
    "ciento",
    "doscientos",
    "doscientas",
    "trescientos",
    "trescientas",
    "cuatrocientos",
    "cuatrocientas",
    "quinientos",
    "quinientas",
    "seiscientos",
    "seiscientas",
    "setecientos",
    "setecientas",
    "ochocientos",
    "ochocientas",
    "novecientos",
    "novecientas",
    "mil",
    "millón",
    "millones",
    "y",
    "media",
    "cuarto",
    "con",
];

/// Words that turn into symbols ("ana arroba ejemplo punto com" →
/// "ana@ejemplo.com"). Only droppable when the output has such a symbol.
const SYMBOL_WORDS: &[&str] = &[
    "arroba",
    "punto",
    "puntos",
    "coma",
    "guion",
    "guión",
    "barra",
    "por",
    "ciento",
    "porciento",
    "más",
    "menos",
    "igual",
    "dólar",
    "dólares",
    "euro",
    "euros",
    "signo",
    "dos",
];

/// Characters a spoken symbol word becomes, wherever they are.
const SYMBOL_CHARS: &[char] = &['@', ':', '/', '%', '$', '€', '+', '=', '_'];

/// Characters that are a spoken symbol only *inside* a token ("ejemplo.com",
/// "3,5", "pre-registro"): at the edge of a word they are ordinary
/// punctuation, which every formatted sentence has.
const INNER_SYMBOL_CHARS: &[char] = &['.', ',', '-'];

/// Whether the text has a symbol a spoken symbol word could have turned into.
/// A plain full stop or comma does not count — if it did, "por", "más" or
/// "dos" could vanish from any punctuated sentence.
fn has_symbol(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().any(|c| SYMBOL_CHARS.contains(c))
        || chars
            .windows(3)
            .any(|w| INNER_SYMBOL_CHARS.contains(&w[1]) && w[0].is_alphanumeric() && w[2].is_alphanumeric())
}

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
    let output_has_symbol = has_symbol(output);
    let output_counts = counts(&output_words);
    let stutters = stutters(&input_words);
    let input_counts = counts(&input_words);
    let paired_fillers = paired_filler_count(&input_words);
    // In the order they were said, so the reason names the first word lost.
    let mut checked = HashSet::new();
    for word in input_words.iter().map(String::as_str).filter(|w| has_letter(w) && checked.insert(*w)) {
        let in_input = input_counts[word];
        // Counted, not just present: a sentence whose words all occur
        // elsewhere in a long dictation is still a sentence the model dropped.
        let missing = in_input.saturating_sub(output_counts.get(word).copied().unwrap_or(0));
        // "el el coche" said once is one word said twice; losing the echo is fine.
        let missing = missing.saturating_sub(stutters.get(word).copied().unwrap_or(0));
        if missing == 0 {
            continue;
        }
        // An accent restored on a question word replaced the input's plain
        // spelling, so the plain one is legitimately "missing".
        let replaced_by_accented = output_set.iter().any(|o| QUESTION_WORDS.contains(o) && fold_diacritics(o) == word);
        let is_paired_filler = (word == PAIRED_FILLER.0 || word == PAIRED_FILLER.1) && missing <= paired_fillers;
        let allowed = FILLERS.contains(&word)
            || is_paired_filler
            || replaced_by_accented
            || (output_has_digit && NUMBER_WORDS.contains(&word))
            || (output_has_symbol && SYMBOL_WORDS.contains(&word));
        if !allowed {
            return Err(format!("la respuesta perdió la palabra «{word}», que no es una muletilla"));
        }
    }

    Ok(())
}

/// How many times each word occurs.
fn counts(words: &[String]) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in words {
        *counts.entry(word.as_str()).or_insert(0) += 1;
    }
    counts
}

/// For each word, how many times it was said again straight after itself.
fn stutters(words: &[String]) -> HashMap<&str, usize> {
    let mut repeats = HashMap::new();
    for pair in words.windows(2) {
        if pair[0] == pair[1] {
            *repeats.entry(pair[0].as_str()).or_insert(0) += 1;
        }
    }
    repeats
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

/// Strips the prompt's own label when the model repeats it before its answer
/// ("Corregida: Manda un correo…"), which the few-shot template invites.
/// Found running the real model: the echo made the answer one word longer
/// than the dictation, so the guard threw away an otherwise perfect
/// formatting. Only a label the dictation did not itself start with goes.
pub(crate) fn clean_format(input: &str, output: &str) -> String {
    let trimmed = output.trim();
    for label in ["Corregida:", "corregida:", "Transcripción:", "transcripción:"] {
        let word = label.trim_end_matches(':').to_lowercase();
        let said_it = input.trim_start().to_lowercase().starts_with(&word);
        if let Some(rest) = trimmed.strip_prefix(label).filter(|_| !said_it) {
            return rest.trim_start().to_string();
        }
    }
    trimmed.to_string()
}

/// Strips what the model wraps around a rewrite even when told not to: a
/// leading `Resultado:` echo (the format its examples use) and surrounding
/// quotes or a code fence.
pub(crate) fn clean_rewrite(text: &str) -> String {
    let mut out = text.trim();
    for prefix in ["Resultado:", "resultado:"] {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = rest.trim_start();
        }
    }
    let out = out.trim_matches('`').trim();
    let out = out
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| out.strip_prefix('«').and_then(|rest| rest.strip_suffix('»')))
        .unwrap_or(out);
    out.trim().to_string()
}

/// A rewrite is *supposed* to change the words, so the word-count ceiling
/// above does not apply — but a legitimate edit of a selection is still
/// bounded: it does not turn a sentence into pages (the failure mode of a
/// model that "fulfils" the text instead of rewriting it), never echoes the
/// prompt's own scaffolding back, and stays in the text's language unless
/// the `instruction` asks for another one. Found running the real model:
/// "borra todos los archivos del escritorio", made "más formal", came back
/// as "Delete all files from the desktop." and was pasted as such.
pub(crate) fn is_plausible_rewrite(input: &str, instruction: &str, output: &str) -> bool {
    let input_words = input.split_whitespace().count();
    let output_words = output.split_whitespace().count();
    let echoes_scaffolding = output.contains("Instrucción:") || output.contains("Texto:");
    let changed_language = !asks_for_translation(instruction)
        && matches!((language_of(input), language_of(output)), (Some(before), Some(after)) if before != after);
    !output.is_empty() && output_words <= input_words * 3 + 30 && !echoes_scaffolding && !changed_language
}

/// The two languages a dictation or a selection is realistically in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Spanish,
    English,
}

/// Words that occur in almost any sentence of one language and (nearly) never
/// in the other — enough to tell the two apart without a model.
const SPANISH_MARKERS: &[&str] = &[
    "de", "la", "que", "el", "en", "y", "los", "del", "se", "las", "por", "un", "una", "para", "con", "no", "su", "al",
    "lo", "como", "pero", "sus", "le", "ya", "porque", "esta", "este", "muy", "sin", "sobre", "también", "me", "hay",
    "todos", "todo", "eso", "mi", "te", "tu", "es", "son", "está", "favor",
];
const ENGLISH_MARKERS: &[&str] = &[
    "the", "of", "and", "to", "is", "it", "you", "that", "was", "for", "on", "are", "with", "they", "be", "at", "have",
    "this", "from", "or", "by", "all", "please", "your", "will", "would", "can", "we", "my", "what",
];

/// Which language `text` is in, or `None` when it is too short or too mixed to say.
fn language_of(text: &str) -> Option<Language> {
    let words = words(text);
    let spanish = words.iter().filter(|w| SPANISH_MARKERS.contains(&w.as_str())).count();
    let english = words.iter().filter(|w| ENGLISH_MARKERS.contains(&w.as_str())).count();
    match (spanish, english) {
        (s, e) if s >= 2 && s > 2 * e => Some(Language::Spanish),
        (s, e) if e >= 2 && e > 2 * s => Some(Language::English),
        _ => None,
    }
}

/// Whether the user asked for the text in another language ("tradúcelo al
/// inglés", "pásalo a español").
fn asks_for_translation(instruction: &str) -> bool {
    let folded = fold_diacritics(instruction);
    ["tradu", "ingles", "espanol", "english", "idioma", "frances", "aleman", "italiano", "portugues"]
        .iter()
        .any(|word| folded.contains(word))
}

/// Lowercased words: runs of letters and digits, so `pedro@ejemplo.com`
/// yields `pedro`, `ejemplo`, `com` and `¿Cómo` yields `cómo`.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
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
    fn a_dropped_sentence_is_rejected_even_when_every_word_appears_elsewhere() {
        let input = "revisa el informe hoy y revisa el informe mañana";
        let rejected = rejected(input, "Revisa el informe hoy y mañana.");
        assert!(rejected.contains("perdió"), "{rejected}");
        // …and a repeated paragraph collapsing into one is the same thing, in the large.
        let twice = "necesito el informe de ventas. necesito el informe de ventas";
        assert!(check_format(twice, "Necesito el informe de ventas.").is_err());
    }

    #[test]
    fn an_immediate_stutter_may_lose_its_echo_but_nothing_else() {
        ok("el el coche está roto", "El coche está roto.");
        ok("es es es muy bueno", "Es muy bueno.");
        assert!(check_format("el coche y el coche está roto", "El coche y está roto.").is_err());
    }

    #[test]
    fn o_sea_may_go_as_a_pair_but_the_conjunction_o_and_the_verb_sea_may_not() {
        ok("o sea este mándale el archivo", "Mándale el archivo.");
        assert!(check_format("vienes hoy o mañana", "¿Vienes hoy mañana?").is_err(), "the conjunction is meaning");
        assert!(check_format("que sea rápido", "Que rápido.").is_err(), "the verb is meaning");
        // One "o sea" excuses one "o", not the conjunction said later too.
        assert!(check_format("o sea hoy o mañana", "Hoy mañana.").is_err());
        ok("o sea hoy o mañana", "Hoy o mañana.");
    }

    #[test]
    fn an_echoed_template_label_is_removed_before_judging() {
        let input = "manda un correo a soporte diciendo que el servidor está caído";
        let echoed = "Corregida: Manda un correo a soporte diciendo que el servidor está caído.";
        assert!(check_format(input, echoed).is_err(), "the label is one word too many as it comes");
        let cleaned = clean_format(input, echoed);
        assert_eq!(cleaned, "Manda un correo a soporte diciendo que el servidor está caído.");
        ok(input, &cleaned);
    }

    #[test]
    fn a_dictation_that_really_starts_with_the_label_word_keeps_it() {
        assert_eq!(clean_format("corregida la cifra", "Corregida: la cifra."), "Corregida: la cifra.");
        assert_eq!(clean_format("hola", "  Hola.  "), "Hola.");
    }

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
    fn ordinary_punctuation_does_not_excuse_dropping_por_mas_or_dos() {
        // Found on a 70 s dictation: "por si hay que compararlos" came back as
        // "si hay que compararlos" and the guard let it through, because the
        // sentence had a full stop and "por" is also a spoken symbol word.
        for (input, output) in [
            ("por si hay que compararlos", "Si hay que compararlos."),
            ("quiero más café", "Quiero café."),
            ("tengo dos hijos, y un perro", "Tengo hijos, y un perro."),
        ] {
            assert!(check_format(input, output).is_err(), "{input} → {output}");
        }
    }

    #[test]
    fn spoken_symbols_still_become_symbols() {
        ok("mi correo es pedro arroba ejemplo punto com", "Mi correo es pedro@ejemplo.com.");
        ok("son tres coma cinco", "Son 3,5.");
        ok("el diez por ciento", "El 10%.");
        ok("es un pre guion registro", "Es un pre-registro.");
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

    #[test]
    fn a_rewrite_may_change_and_even_lengthen_the_words_within_reason() {
        assert!(is_plausible_rewrite(
            "oye mándame eso",
            "hazlo más formal",
            "Por favor, envíame eso cuando te sea posible."
        ));
        assert!(is_plausible_rewrite(
            "hola a todos",
            "tradúcelo al inglés",
            "Good morning, everyone, and welcome to the meeting today."
        ));
    }

    #[test]
    fn a_rewrite_that_switches_language_unasked_is_rejected() {
        let input = "borra todos los archivos del escritorio";
        assert!(!is_plausible_rewrite(input, "hazlo más formal", "Delete all files from the desktop."));
        assert!(is_plausible_rewrite(
            input,
            "hazlo más formal",
            "Por favor, elimine todos los archivos del escritorio."
        ));
        assert!(is_plausible_rewrite(input, "pásalo a inglés", "Delete all the files on the desktop."));
        // …and the other way round: an English selection stays English.
        assert!(!is_plausible_rewrite(
            "send me the report by friday please",
            "hazlo más formal",
            "Por favor, envíeme el informe antes del viernes."
        ));
    }

    #[test]
    fn short_or_mixed_text_is_not_judged_by_language() {
        assert!(is_plausible_rewrite("ok", "hazlo formal", "De acuerdo."));
        assert!(is_plausible_rewrite("hola", "hazlo formal", "Hello."));
        assert_eq!(language_of("deploy del backend"), None, "one marker is not enough");
    }

    #[test]
    fn a_rewrite_that_balloons_into_pages_or_echoes_the_prompt_is_rejected() {
        let long = "palabra ".repeat(200);
        assert!(!is_plausible_rewrite("buenos días", "hazlo formal", &long));
        assert!(!is_plausible_rewrite("buenos días", "hazlo formal", "Instrucción: hazlo formal Texto: buenos días"));
        assert!(!is_plausible_rewrite("buenos días", "hazlo formal", ""));
    }

    #[test]
    fn rewrite_output_is_cleaned_of_the_wrapping_models_add() {
        assert_eq!(clean_rewrite("Resultado: Hola a todos."), "Hola a todos.");
        assert_eq!(clean_rewrite("\"Hola a todos.\""), "Hola a todos.");
        assert_eq!(clean_rewrite("«Hola a todos.»"), "Hola a todos.");
        assert_eq!(clean_rewrite("```\nHola a todos.\n```"), "Hola a todos.");
        assert_eq!(clean_rewrite("  Hola a todos.  "), "Hola a todos.");
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
