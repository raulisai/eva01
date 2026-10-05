//! Repetitions and restarts of spoken language, removed before formatting.
//!
//! Only what is unambiguous without understanding the sentence, found in this
//! user's own dictations:
//!
//! - a word said twice in a row: «la la poliana», «a a uno de ajedrez» —
//!   except the words people double on purpose («no no», «sí sí», «muy muy»);
//! - a run of two or three words said twice: «que se vea que se vea más»;
//! - a word cut off and started again: «agrega agregar», «configu configuración»
//!   — the first is the start of the second and at least four letters long, and
//!   is not a word that is also said before its own longer form («para parar»,
//!   «esta estaba»).
//!
//! A self-correction with different words («puedes una hacer», «del desde la
//! vista») needs to understand the sentence and is left to a model.

use crate::normalize::{fold_diacritics, split_punctuation};

/// Words doubled on purpose, for emphasis or as an answer.
const EMPHATIC: &[&str] = &[
    "no", "si", "ya", "muy", "mas", "poco", "casi", "tan", "bien", "claro", "ahora", "nada", "otra", "vez", "dale",
    "va", "ok", "okay", "yes", "very", "bye", "adios", "ja", "je", "jaja", "corre", "rapido", "mucho", "mucha",
];

/// Words that are the start of another word said right after them in normal
/// speech («para parar», «esta estaba», «como comió»): never a restart.
const NOT_A_RESTART: &[&str] = &[
    "para", "como", "esta", "este", "esto", "estas", "estos", "pero", "entre", "sobre", "cuando", "donde", "hasta",
    "desde", "todo", "toda", "todos", "todas", "otro", "otra", "cada", "mismo", "misma", "algo", "nada", "bien",
    "mejor", "casa", "hora", "vida", "mano", "cosa", "parte", "tanto", "poco", "solo", "sola", "dice", "hace", "tiene",
    "puede", "quiere", "pone", "sale", "viene", "mira", "deja", "pasa", "lleva", "queda", "sigue", "vale", "manda",
];

/// Fewest letters a cut-off start needs: shorter ones («es», «con») begin
/// too many ordinary words.
const MIN_RESTART_LETTERS: usize = 4;

fn key(token: &str) -> String {
    fold_diacritics(split_punctuation(token).1).to_lowercase()
}

fn has_trailing_punctuation(token: &str) -> bool {
    !split_punctuation(token).2.is_empty()
}

/// Whether `first` is a cut-off start of `second`.
fn is_restart(first: &str, second: &str) -> bool {
    let (a, b) = (first.chars().count(), second.chars().count());
    a >= MIN_RESTART_LETTERS
        && b > a
        && second.starts_with(first)
        && !NOT_A_RESTART.contains(&first)
        && first.chars().all(char::is_alphabetic)
}

/// `text` without its repetitions and restarts. Punctuation between the
/// repeated words means they were not a repetition («no, no»), so they stay.
pub fn remove_repetitions(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = tokens.iter().map(|t| key(t)).collect();
    let mut out: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    'next: while i < tokens.len() {
        // A run of three, then two words, said twice: the first time goes.
        for n in [3, 2] {
            if i + 2 * n <= tokens.len()
                && keys[i..i + n] == keys[i + n..i + 2 * n]
                && keys[i..i + n].iter().all(|k| !k.is_empty())
                && !tokens[i..i + n].iter().any(|t| has_trailing_punctuation(t))
                && !keys[i..i + n].iter().all(|k| EMPHATIC.contains(&k.as_str()))
            {
                i += n;
                continue 'next;
            }
        }
        if i + 1 < tokens.len() && !has_trailing_punctuation(tokens[i]) && !keys[i].is_empty() {
            let (this, next) = (&keys[i], &keys[i + 1]);
            let doubled = this == next && !EMPHATIC.contains(&this.as_str()) && this.chars().all(char::is_alphabetic);
            if doubled || is_restart(this, next) {
                // The first is dropped; its leading punctuation («¿la la…»)
                // moves to the word that stays.
                let (prefix, _, _) = split_punctuation(tokens[i]);
                if !prefix.is_empty() {
                    out.push(prefix);
                    out.push("\u{0}"); // glue marker: no space after the prefix
                }
                i += 1;
                continue;
            }
        }
        out.push(tokens[i]);
        i += 1;
    }
    let mut joined = String::with_capacity(text.len());
    let mut glue = false;
    for token in out {
        if token == "\u{0}" {
            glue = true;
            continue;
        }
        if !joined.is_empty() && !glue {
            joined.push(' ');
        }
        joined.push_str(token);
        glue = false;
    }
    joined
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_repetitions_in_this_users_dictations_go() {
        for (said, meant) in [
            ("se ve la la poliana en el centro", "se ve la poliana en el centro"),
            ("parecido a a uno de ajedrez", "parecido a uno de ajedrez"),
            ("mejore y agrega agregar si necesita", "mejore y agregar si necesita"),
            ("que se vea que se vea más tres d", "que se vea más tres d"),
            ("revisa el configu configuración", "revisa el configuración"),
            ("¿la la u i?", "¿la u i?"),
        ] {
            assert_eq!(remove_repetitions(said), meant, "{said}");
        }
    }

    #[test]
    fn what_is_said_twice_on_purpose_stays() {
        for said in [
            "no no, así no",
            "sí sí, dale",
            "muy muy lejos",
            "no, no quiero",
            "es para parar el servidor",
            "esta estaba bien",
            "todo todos los días",
            "cada cadáver",
            "un bye bye",
            "de 2 2",
        ] {
            assert_eq!(remove_repetitions(said), said, "{said}");
        }
    }

    #[test]
    fn a_repeated_word_across_a_comma_is_not_a_repetition() {
        assert_eq!(remove_repetitions("dije que, que no"), "dije que, que no");
    }
}
