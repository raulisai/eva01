//! Keeping the model's punctuation when it got a word wrong.
//!
//! The formatter's job is punctuation and capitalization; the words are the
//! user's. Measured on the on-device model (`eval/format_corpus.txt`, greedy
//! decoding, so the same every run): 3 of 40 dictations came back perfectly
//! punctuated but with one word changed or dropped — "llego en diez minutos"
//! → "Llegaré en diez minutos.", "son doscientos cincuenta dólares…" →
//! "Doscientos cincuenta dólares…". The faithfulness guard rightly refuses
//! them, and the whole dictation used to fall back to rules alone, with no
//! commas or question marks at all. In a long dictation, formatted piece by
//! piece, every refused piece was a stretch without punctuation.
//!
//! [`restore_dictated_words`] lines the model's words up with the dictated
//! ones and rebuilds the text from the *dictated* words, placed within the
//! model's punctuation. It only tries when the two are nearly the same
//! sentence; anything more rewritten than that is not a formatting to rescue.

use crate::faithfulness::{check_format, clean_format, FILLERS};
use crate::normalize::fold_diacritics;

/// What a model's formatting of `input` becomes: the model's text if it is
/// faithful, the dictated words in the model's punctuation if only a word or
/// two went astray, or the guard's reason when neither holds.
///
/// # Errors
/// Why the model's text could be neither accepted nor repaired.
pub(crate) fn faithful_formatting(input: &str, model_output: &str) -> Result<String, String> {
    let output = clean_format(input, model_output);
    let reason = match check_format(input, &output) {
        Ok(()) => return Ok(output),
        Err(reason) => reason,
    };
    restore_dictated_words(input, &output)
        .filter(|repaired| check_format(input, repaired).is_ok())
        .ok_or_else(|| format!("{reason}: {output:?}"))
}

/// Question words the model may legitimately write with the accent the
/// dictation lacked — kept as the model wrote them.
const ACCENTED_QUESTION_WORDS: &[&str] =
    &["qué", "cómo", "cuándo", "dónde", "quién", "quiénes", "cuál", "cuáles", "cuánto", "cuánta", "cuántos", "cuántas"];

/// A word of the model's output and what follows it up to the next word.
#[derive(Debug)]
struct Token<'a> {
    word: &'a str,
    after: &'a str,
}

/// One step of the alignment between dictated words (`input`) and the
/// model's words (`output`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The same word (ignoring case and accents), or one the model changed.
    Pair { input: usize, output: usize },
    /// A dictated word the model left out.
    Dropped { input: usize },
    /// A word the model added.
    Added { output: usize },
}

/// A word of the rebuilt text.
struct Placed {
    text: String,
    after: String,
    /// The dictated form, when there is one, to decide its capital letter.
    dictated: Option<String>,
    /// The model capitalized it only because it started a sentence there.
    capital_was_positional: bool,
}

/// The dictation's words, in the model's punctuation — or `None` when the
/// model's text is not close enough to the dictation to borrow from, or
/// turned words into figures or symbols (numbers, e-mail addresses), which
/// this cannot rebuild faithfully. The caller still checks the result.
pub(crate) fn restore_dictated_words(input: &str, output: &str) -> Option<String> {
    let dictated = alphanumeric_runs(input);
    let (lead, tokens) = tokenize(output);
    if dictated.is_empty() || tokens.is_empty() || !only_prose_separators(&tokens) {
        return None;
    }
    if tokens.iter().any(|t| t.word.chars().any(|c| c.is_ascii_digit())) {
        return None;
    }

    let steps = align(&dictated, &tokens);
    if !close_enough(&steps, &dictated, &tokens) {
        return None;
    }

    let placed = place(&steps, &dictated, &tokens);
    Some(render(lead, placed))
}

/// Runs of letters and digits, as the dictation's words.
fn alphanumeric_runs(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect()
}

/// The output as words with what follows each, plus what precedes the first
/// (an opening "¿" or "¡").
fn tokenize(text: &str) -> (&str, Vec<Token<'_>>) {
    let mut tokens = Vec::new();
    let starts: Vec<(usize, usize)> = word_spans(text);
    let lead = starts.first().map_or(text, |(start, _)| &text[..*start]);
    for (i, (start, end)) in starts.iter().enumerate() {
        let next = starts.get(i + 1).map_or(text.len(), |(next_start, _)| *next_start);
        tokens.push(Token { word: &text[*start..*end], after: &text[*end..next] });
    }
    (lead, tokens)
}

/// Byte ranges of the alphanumeric runs in `text`.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans
}

/// Whether every gap between two words is ordinary prose punctuation with a
/// space in it. "ana@ejemplo.com" or "pre-registro" are conversions of
/// several spoken words into one written one, which rebuilding from the
/// spoken words would undo into nonsense.
fn only_prose_separators(tokens: &[Token<'_>]) -> bool {
    let (last, between) = match tokens.split_last() {
        Some(split) => split,
        None => return true,
    };
    let prose = |gap: &str| gap.chars().all(|c| c.is_whitespace() || ",.;:!?¿¡…\"'«»()".contains(c));
    between.iter().all(|t| prose(t.after) && t.after.chars().any(char::is_whitespace)) && prose(last.after)
}

fn same_word(a: &str, b: &str) -> bool {
    fold_diacritics(a) == fold_diacritics(b)
}

/// Word-level edit alignment of the dictation against the model's words,
/// comparing without case or accents.
fn align(dictated: &[&str], tokens: &[Token<'_>]) -> Vec<Step> {
    let (n, m) = (dictated.len(), tokens.len());
    let mut cost = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in cost[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let pair = cost[i - 1][j - 1] + usize::from(!same_word(dictated[i - 1], tokens[j - 1].word));
            cost[i][j] = pair.min(cost[i - 1][j] + 1).min(cost[i][j - 1] + 1);
        }
    }

    let mut steps = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        let pair_cost =
            |i: usize, j: usize| cost[i - 1][j - 1] + usize::from(!same_word(dictated[i - 1], tokens[j - 1].word));
        if i > 0 && j > 0 && cost[i][j] == pair_cost(i, j) {
            steps.push(Step::Pair { input: i - 1, output: j - 1 });
            i -= 1;
            j -= 1;
        } else if i > 0 && cost[i][j] == cost[i - 1][j] + 1 {
            steps.push(Step::Dropped { input: i - 1 });
            i -= 1;
        } else {
            steps.push(Step::Added { output: j - 1 });
            j -= 1;
        }
    }
    steps.reverse();
    steps
}

/// At most one changed, dropped or added word in five (and at least one in
/// any sentence), not counting fillers the model was allowed to drop — past
/// that the model rewrote the sentence instead of punctuating it.
fn close_enough(steps: &[Step], dictated: &[&str], tokens: &[Token<'_>]) -> bool {
    let is_filler = |word: &str| FILLERS.contains(&word.to_lowercase().as_str());
    let content_words = dictated.iter().filter(|w| !is_filler(w)).count();
    let edits = steps
        .iter()
        .filter(|step| match **step {
            Step::Pair { input, output } => !same_word(dictated[input], tokens[output].word),
            Step::Dropped { input } => !is_filler(dictated[input]),
            Step::Added { .. } => true,
        })
        .count();
    let kept = steps
        .iter()
        .filter(
            |step| matches!(**step, Step::Pair { input, output } if same_word(dictated[input], tokens[output].word)),
        )
        .count();
    kept > 0 && edits <= (content_words / 5).max(1)
}

/// The rebuilt words, each with the model's punctuation after it.
fn place(steps: &[Step], dictated: &[&str], tokens: &[Token<'_>]) -> Vec<Placed> {
    let output_positional = positional_capitals(tokens);
    let mut placed: Vec<Placed> = Vec::new();
    for (k, step) in steps.iter().enumerate() {
        match *step {
            Step::Pair { input, output } => {
                let (said, wrote) = (dictated[input], tokens[output].word);
                let text = if said.to_lowercase() == wrote.to_lowercase()
                    || (ACCENTED_QUESTION_WORDS.contains(&wrote.to_lowercase().as_str()) && same_word(said, wrote))
                {
                    wrote.to_string()
                } else {
                    with_first_letter_case_of(said, wrote)
                };
                placed.push(Placed {
                    text,
                    after: tokens[output].after.to_string(),
                    dictated: Some(said.to_string()),
                    capital_was_positional: output_positional[output],
                });
            }
            Step::Dropped { input } => {
                let said = dictated[input];
                if FILLERS.contains(&said.to_lowercase().as_str()) {
                    continue;
                }
                let any_model_word_follows = steps[k + 1..].iter().any(|s| matches!(s, Step::Pair { .. }));
                // At the end, the model's closing punctuation moves past the
                // word it left out; elsewhere the word goes in with a space.
                let after = match placed.last_mut() {
                    Some(previous) if !any_model_word_follows => {
                        std::mem::replace(&mut previous.after, " ".to_string())
                    }
                    _ => " ".to_string(),
                };
                placed.push(Placed {
                    text: said.to_string(),
                    after,
                    dictated: Some(said.to_string()),
                    capital_was_positional: false,
                });
            }
            Step::Added { output } => {
                // The word goes; its punctuation survives if the word before
                // had none of its own.
                if let Some(previous) = placed.last_mut() {
                    let punctuation = tokens[output].after;
                    if previous.after.trim().is_empty() && !punctuation.trim().is_empty() {
                        previous.after = punctuation.to_string();
                    }
                }
            }
        }
    }
    placed
}

/// For each output word, whether it starts a sentence in the model's text
/// (so its capital letter was about position, not about the word).
fn positional_capitals(tokens: &[Token<'_>]) -> Vec<bool> {
    let mut starts = Vec::with_capacity(tokens.len());
    let mut at_start = true;
    for token in tokens {
        starts.push(at_start);
        at_start = ends_sentence(token.after);
    }
    starts
}

fn ends_sentence(gap: &str) -> bool {
    gap.contains(['.', '?', '!', '…'])
}

/// `word` with its first letter in the case `model_word`'s has.
fn with_first_letter_case_of(word: &str, model_word: &str) -> String {
    let upper = model_word.chars().next().is_some_and(char::is_uppercase);
    set_first_letter(word, upper)
}

fn set_first_letter(word: &str, upper: bool) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) if upper => first.to_uppercase().chain(chars).collect(),
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Joins the words, capitalizing each sentence's first word, and taking back
/// a capital the model gave a word only because it started a sentence that
/// now starts elsewhere.
fn render(lead: &str, placed: Vec<Placed>) -> String {
    let mut out = lead.to_string();
    let mut at_start = true;
    for word in placed {
        let dictated_lowercase =
            word.dictated.as_deref().is_some_and(|d| d.chars().next().is_some_and(char::is_lowercase));
        let text = if at_start {
            set_first_letter(&word.text, true)
        } else if word.capital_was_positional && dictated_lowercase {
            set_first_letter(&word.text, false)
        } else {
            word.text
        };
        out.push_str(&text);
        out.push_str(&word.after);
        at_start = ends_sentence(&word.after);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::faithfulness::check_format;

    fn repaired(input: &str, output: &str) -> String {
        let fixed = restore_dictated_words(input, output).unwrap_or_else(|| panic!("no repair for {output:?}"));
        assert!(check_format(input, &fixed).is_ok(), "the repair must pass the guard: {fixed:?}");
        fixed
    }

    #[test]
    fn a_changed_verb_goes_back_to_the_dictated_one_in_the_models_punctuation() {
        assert_eq!(repaired("llego en diez minutos", "Llegaré en diez minutos."), "Llego en diez minutos.");
        assert_eq!(repaired("quedamos a las cinco", "Quédamos a las cinco."), "Quedamos a las cinco.");
    }

    #[test]
    fn a_dropped_word_comes_back_and_the_sentence_still_starts_with_a_capital() {
        assert_eq!(
            repaired("son doscientos cincuenta dólares por mes", "Doscientos cincuenta dólares por mes."),
            "Son doscientos cincuenta dólares por mes."
        );
    }

    #[test]
    fn a_word_dropped_at_the_end_goes_before_the_closing_punctuation() {
        assert_eq!(repaired("nos vemos mañana temprano", "Nos vemos mañana."), "Nos vemos mañana temprano.");
    }

    #[test]
    fn an_added_word_goes_but_its_punctuation_can_stay() {
        assert_eq!(repaired("claro que sí nos vemos", "Claro que sí, yo nos vemos."), "Claro que sí, nos vemos.");
    }

    #[test]
    fn question_marks_accents_on_question_words_and_proper_nouns_are_kept() {
        assert_eq!(
            repaired("cuando llega maría al aeropuerto", "¿Cuándo llegó María al aeropuerto?"),
            "¿Cuándo llega María al aeropuerto?"
        );
    }

    #[test]
    fn fillers_the_model_dropped_stay_dropped() {
        assert_eq!(repaired("este llego en diez minutos", "Llegaré en diez minutos."), "Llego en diez minutos.");
    }

    #[test]
    fn the_whole_decision_accepts_repairs_or_refuses() {
        assert_eq!(faithful_formatting("hola cómo estás", "Hola, ¿cómo estás?").unwrap(), "Hola, ¿cómo estás?");
        assert_eq!(
            faithful_formatting("llego en diez minutos", "Corregida: Llegaré en diez minutos.").unwrap(),
            "Llego en diez minutos."
        );
        let refused = faithful_formatting("gracias", "¡De nada! ¿En qué más te puedo ayudar?").unwrap_err();
        assert!(refused.contains("De nada"), "the reason quotes what the model said: {refused}");
    }

    #[test]
    fn a_rewritten_sentence_is_not_rescued() {
        assert_eq!(restore_dictated_words("gracias", "¡De nada! ¿En qué más te puedo ayudar?"), None);
        assert_eq!(restore_dictated_words("dime cuando estés listo para salir", "Avísame cuando puedas irte."), None);
    }

    #[test]
    fn figures_and_symbols_are_left_to_the_guard_not_undone() {
        assert_eq!(restore_dictated_words("son las diez y media", "Serán las 10:30."), None);
        assert_eq!(
            restore_dictated_words("mi correo es ana arroba ejemplo punto com", "Tu correo es ana@ejemplo.com."),
            None
        );
    }

    #[test]
    fn several_sentences_keep_their_own_capitals() {
        assert_eq!(
            repaired(
                "llegué tarde hoy mañana llego temprano y te aviso",
                "Llegué tarde hoy. Mañana llegaré temprano y te aviso."
            ),
            "Llegué tarde hoy. Mañana llego temprano y te aviso."
        );
    }

    proptest::proptest! {
        #[test]
        fn never_panics_and_only_ever_returns_the_dictated_words(input in "[a-zñáé ]{0,40}", output in "[A-Za-zñáé ,.¿?]{0,50}") {
            if let Some(fixed) = restore_dictated_words(&input, &output) {
                let dictated: Vec<String> = alphanumeric_runs(&input)
                    .into_iter()
                    .map(fold_diacritics)
                    .filter(|w| !FILLERS.contains(&w.as_str()))
                    .collect();
                let rebuilt: Vec<String> = alphanumeric_runs(&fixed).into_iter().map(fold_diacritics).collect();
                proptest::prop_assert_eq!(rebuilt, dictated);
            }
        }
    }
}
