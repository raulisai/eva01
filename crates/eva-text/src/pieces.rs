//! Cutting a long dictation into pieces the formatter can be trusted with.
//!
//! Measured on Apple Intelligence: a 120–190 word dictation comes back with
//! words *changed* ("mándamelo" → "mándame", "revises" → "revise") — the
//! faithfulness guard rightly rejects it, and the whole text falls back to
//! rules alone, with no punctuation at all. Sentence-sized pieces come back
//! intact, and a piece that does fail costs only itself.

/// Up to this many words are formatted in one go, as every short dictation is.
const WHOLE_LIMIT: usize = 40;

/// The most words in one piece of a longer text.
const PIECE_WORDS: usize = 30;

/// One piece of a long dictation, to be formatted on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// The words of the piece.
    pub text: String,
    /// Whether the piece stops where the speech model put a full stop, or
    /// only where this module had to cut — in the middle of a sentence.
    pub cut_mid_sentence: bool,
}

/// Words that open a clause with some weight of their own: cutting *before*
/// one reads as natural, and the model's full stop there becomes a comma
/// that belongs ("…del contrato nuevo, porque todavía…").
const STRONG_STARTERS: &[&str] = &[
    "pero",
    "porque",
    "aunque",
    "sino",
    "así",
    "entonces",
    "luego",
    "después",
    "además",
    "también",
    "finalmente",
    "mientras",
    "cuando",
    "donde",
    "primero",
    "segundo",
    "y",
    "e",
    "o",
    "u",
];

/// Words that often open a clause but just as often a plain phrase ("mándalo
/// | por correo"): a cut before one of these is a last resort.
const WEAK_STARTERS: &[&str] = &["que", "si", "como", "ya", "pues", "por", "para", "sin"];

/// Words that bind to what follows them, so a cut right after one splits a
/// phrase: "por | si", "así | que", "del | contrato", "lo | que".
const BINDS_FORWARD: &[&str] = &[
    "a", "al", "de", "del", "en", "con", "por", "para", "sin", "sobre", "entre", "hasta", "desde", "hacia", "así",
    "ya", "que", "lo", "la", "el", "los", "las", "un", "una", "unos", "unas", "y", "e", "o", "u", "ni", "pero", "mi",
    "tu", "su", "mis", "tus", "sus", "muy", "más", "me", "te", "se", "le", "les", "nos", "no", "si", "como", "cuando",
    "porque", "aunque", "este", "esta", "ese", "esa", "aquel", "aquella",
];

/// `text` as the pieces to format one by one, in order; joining them with
/// spaces gives back exactly the same words. Text that is short enough comes
/// back as a single piece.
///
/// Pieces end at the sentence punctuation the speech model already put (`.`,
/// `?`, `!`), packed together up to [`PIECE_WORDS`] words. A stretch with none
/// is cut at the clause boundary nearest the end of the window — after a
/// comma, or before a word like "porque" — and only failing that at the
/// window's end.
pub fn split_for_formatting(text: &str) -> Vec<Piece> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= WHOLE_LIMIT {
        return vec![Piece { text: text.trim().to_string(), cut_mid_sentence: false }];
    }

    let mut pieces: Vec<Vec<&str>> = Vec::new();
    for sentence in sentences(&words) {
        let mut rest = sentence;
        while rest.len() > PIECE_WORDS {
            let cut = clause_boundary(rest);
            pieces.push(rest[..cut].to_vec());
            rest = &rest[cut..];
        }
        match pieces.last_mut() {
            Some(last) if last.len() + rest.len() <= PIECE_WORDS && !ends_sentence(last) => last.extend(rest),
            _ => pieces.push(rest.to_vec()),
        }
    }
    let last = pieces.len().saturating_sub(1);
    pieces
        .into_iter()
        .enumerate()
        // The end of the dictation is an end, not a cut.
        .map(|(i, piece)| Piece { cut_mid_sentence: i < last && !ends_sentence(&piece), text: piece.join(" ") })
        .collect()
}

fn ends_sentence(words: &[&str]) -> bool {
    words.last().is_some_and(|w| w.ends_with(['.', '?', '!', '…']))
}

/// The words split after each one that ends a sentence.
fn sentences<'a>(words: &'a [&'a str]) -> Vec<&'a [&'a str]> {
    let mut sentences = Vec::new();
    let mut start = 0;
    for (i, word) in words.iter().enumerate() {
        if word.ends_with(['.', '?', '!', '…']) {
            sentences.push(&words[start..=i]);
            start = i + 1;
        }
    }
    if start < words.len() {
        sentences.push(&words[start..]);
    }
    sentences
}

/// Where to cut `words` (more than [`PIECE_WORDS`] of them, no full stop
/// inside), looking at the final third of the window, latest first: after a
/// comma; else before a strong clause word; else before a weak one; else at
/// the window's end. A cut never falls right after a word that binds to the
/// next one, which is what split "por si" and "así que" in a real dictation.
fn clause_boundary(words: &[&str]) -> usize {
    let lower = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let splits_a_phrase = |cut: usize| BINDS_FORWARD.contains(&lower(words[cut - 1]).as_str());
    let before = |cut: usize, starters: &[&str]| {
        !splits_a_phrase(cut) && words.get(cut).is_some_and(|w| starters.contains(&lower(w).as_str()))
    };
    let candidates = || (PIECE_WORDS * 2 / 3..=PIECE_WORDS).rev();

    candidates()
        .find(|&cut| words[cut - 1].ends_with([',', ';', ':']))
        .or_else(|| candidates().find(|&cut| before(cut, STRONG_STARTERS)))
        .or_else(|| candidates().find(|&cut| before(cut, WEAK_STARTERS)))
        .or_else(|| candidates().find(|&cut| !splits_a_phrase(cut)))
        .unwrap_or(PIECE_WORDS)
}

/// Sews formatted pieces back into one text. Where a piece was cut in the
/// middle of a sentence, the full stop the model closed it with becomes a
/// comma and the next piece gives its first letter back to lower case if it
/// was lower case as said — so the seam reads as one sentence, not two.
pub fn stitch(pieces: &[Piece], formatted: &[String]) -> String {
    let mut out = String::new();
    for (i, (piece, text)) in pieces.iter().zip(formatted).enumerate() {
        let mut text = text.trim().to_string();
        if i > 0 && pieces[i - 1].cut_mid_sentence {
            text = match_first_letter_case(&text, &piece.text);
        }
        if piece.cut_mid_sentence && i + 1 < pieces.len() {
            if let Some(without_stop) = text.strip_suffix('.') {
                text = format!("{without_stop},");
            }
        }
        if !out.is_empty() && !text.is_empty() {
            out.push(' ');
        }
        out.push_str(&text);
    }
    out
}

/// `formatted`, with its first letter in the case `original` has it.
fn match_first_letter_case(formatted: &str, original: &str) -> String {
    let lower = original.chars().find(|c| c.is_alphabetic()).is_some_and(char::is_lowercase);
    let mut chars = formatted.chars();
    let mut out = String::new();
    let mut done = false;
    for c in chars.by_ref() {
        if !done && c.is_alphabetic() {
            out.extend(if lower { c.to_lowercase().collect::<Vec<_>>() } else { vec![c] });
            done = true;
        } else {
            out.push(c);
        }
        if done {
            break;
        }
    }
    out.extend(chars);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn texts(pieces: &[Piece]) -> Vec<String> {
        pieces.iter().map(|p| p.text.clone()).collect()
    }

    fn words_of(pieces: &[Piece]) -> Vec<String> {
        pieces.iter().flat_map(|p| p.text.split_whitespace().map(str::to_string)).collect()
    }

    fn count(piece: &Piece) -> usize {
        piece.text.split_whitespace().count()
    }

    #[test]
    fn a_normal_dictation_is_one_piece_untouched() {
        assert_eq!(texts(&split_for_formatting("hola qué tal cómo estás")), vec!["hola qué tal cómo estás"]);
        let forty = vec!["palabra"; WHOLE_LIMIT].join(" ");
        assert_eq!(texts(&split_for_formatting(&forty)), vec![forty]);
    }

    #[test]
    fn sentences_are_kept_whole_and_packed_up_to_the_limit() {
        let sentence = |n: usize| format!("{}.", vec!["uno"; n].join(" "));
        let text = [sentence(20), sentence(15), sentence(25), sentence(10)].join(" ");
        let pieces = split_for_formatting(&text);
        // 20+15 > 30 so it cannot pack; each ends where its sentence ends.
        assert_eq!(pieces.len(), 4, "{pieces:?}");
        assert!(pieces.iter().all(|p| p.text.ends_with('.') && !p.cut_mid_sentence));
    }

    #[test]
    fn a_stretch_without_punctuation_is_cut_at_a_clause_boundary_when_there_is_one() {
        // 45 words; "porque" is word 26 (index 25): the cut goes right before it.
        let mut words: Vec<String> = (0..45).map(|i| format!("p{i}")).collect();
        words[25] = "porque".to_string();
        let pieces = split_for_formatting(&words.join(" "));

        assert_eq!(pieces.len(), 2);
        assert_eq!(count(&pieces[0]), 25);
        assert!(pieces[1].text.starts_with("porque"), "{pieces:?}");
        assert!(pieces[0].cut_mid_sentence && !pieces[1].cut_mid_sentence);
    }

    #[test]
    fn a_cut_never_splits_por_si_or_asi_que() {
        // The two bad seams of a real 70 s dictation.
        let copia = "recuérdame también llevar la presentación impresa y una copia del acuerdo anterior \
                     por si hay que compararlos durante la conversación y además revisar el presupuesto con calma";
        let lista = "el equipo de desarrollo terminó la versión nueva de la aplicación móvil y ya está lista para \
                     pruebas así que te pido que coordines con marta para que la revisen antes de publicarla hoy";
        for text in [format!("{copia} {copia}"), format!("{lista} {lista}")] {
            let pieces = split_for_formatting(&text);
            assert!(pieces.len() > 1);
            for seam in pieces.windows(2) {
                let end = seam[0].text.split_whitespace().last().unwrap();
                let start = seam[1].text.split_whitespace().next().unwrap();
                assert!(!BINDS_FORWARD.contains(&end), "cut after «{end}» before «{start}»: {pieces:?}");
            }
        }
    }

    #[test]
    fn a_strong_clause_word_beats_a_later_weak_one() {
        // "pero" at word 22, "que" at word 27: the cut goes before "pero".
        let mut words: Vec<String> = (0..45).map(|i| format!("p{i}")).collect();
        words[21] = "pero".to_string();
        words[26] = "que".to_string();
        let pieces = split_for_formatting(&words.join(" "));
        assert!(pieces[1].text.starts_with("pero"), "{pieces:?}");
    }

    #[test]
    fn a_comma_is_a_clause_boundary_too() {
        let mut words: Vec<String> = (0..45).map(|i| format!("p{i}")).collect();
        words[23] = "p23,".to_string();
        let pieces = split_for_formatting(&words.join(" "));
        assert_eq!(count(&pieces[0]), 24, "{pieces:?}");
    }

    #[test]
    fn with_no_boundary_at_all_it_cuts_at_the_window_end() {
        let text = (0..100).map(|i| format!("p{i}")).collect::<Vec<_>>().join(" ");
        let sizes: Vec<usize> = split_for_formatting(&text).iter().map(count).collect();
        assert_eq!(sizes, vec![30, 30, 30, 10]);
    }

    #[test]
    fn no_word_is_lost_or_reordered() {
        let text = (0..137)
            .map(|i| if i % 23 == 22 { format!("w{i}.") } else { format!("w{i}") })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(words_of(&split_for_formatting(&text)), text.split_whitespace().collect::<Vec<_>>());
    }

    #[test]
    fn a_seam_cut_mid_sentence_reads_as_one_sentence() {
        let pieces = vec![
            Piece { text: "lo que presentó el".into(), cut_mid_sentence: true },
            Piece { text: "equipo de finanzas y la Dirección".into(), cut_mid_sentence: true },
            Piece { text: "Marta revisa todo".into(), cut_mid_sentence: false },
        ];
        let formatted = [
            "Lo que presentó el.".to_string(),
            "Equipo de finanzas y la Dirección.".into(),
            "Marta revisa todo.".into(),
        ];
        assert_eq!(
            stitch(&pieces, &formatted),
            "Lo que presentó el, equipo de finanzas y la Dirección, Marta revisa todo.",
            "stops become commas at artificial seams, the next word is lower case again, and the real stop stays"
        );
    }

    #[test]
    fn a_real_sentence_end_keeps_its_stop_and_its_capital() {
        let pieces = vec![
            Piece { text: "primera frase.".into(), cut_mid_sentence: false },
            Piece { text: "segunda frase.".into(), cut_mid_sentence: false },
        ];
        let formatted = ["Primera frase.".to_string(), "Segunda frase.".into()];
        assert_eq!(stitch(&pieces, &formatted), "Primera frase. Segunda frase.");
    }

    #[test]
    fn a_question_or_exclamation_at_a_seam_is_left_alone() {
        let pieces = vec![
            Piece { text: "vienes".into(), cut_mid_sentence: true },
            Piece { text: "mañana".into(), cut_mid_sentence: false },
        ];
        assert_eq!(stitch(&pieces, &["¿Vienes?".to_string(), "Mañana.".into()]), "¿Vienes? mañana.");
    }

    proptest::proptest! {
        #[test]
        fn pieces_always_hold_exactly_the_words_and_stay_small(text in "([a-z]{1,6}[.?,]? ){0,200}") {
            let pieces = split_for_formatting(&text);
            proptest::prop_assert_eq!(words_of(&pieces), text.split_whitespace().map(str::to_string).collect::<Vec<_>>());
            if text.split_whitespace().count() > WHOLE_LIMIT {
                proptest::prop_assert!(pieces.iter().all(|p| count(p) <= PIECE_WORDS));
            }
        }
    }
}
