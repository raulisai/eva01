//! The word-by-word analysis behind the panel's "Salió mal" report: given
//! what came out and what the user meant, which words were wrong, at which
//! stage of the pipeline they went wrong, and which ones are worth teaching.
//!
//! Stages, in order: the speech model (`raw`), the dictionary and filler
//! pass (`pre`), the formatter (`text`). A mistake already in `raw` is the
//! model's, so a learned replacement (which runs on that text) can fix it;
//! one that appears later was introduced by our own pipeline, and teaching a
//! replacement would not reach it — the report says so instead.

use eva_text::{fold_diacritics, split_punctuation};
use serde_json::{json, Value};

/// Letters below which a word is too common to be corrected on its own.
const SHORT_WORD: usize = 6;

/// Words of a text as the analysis compares them: (original token, its core
/// without punctuation, the core folded and lowercase).
struct Word<'a> {
    token: &'a str,
    core: &'a str,
    key: String,
}

fn words(text: &str) -> Vec<Word<'_>> {
    text.split_whitespace()
        .map(|token| {
            let core = split_punctuation(token).1;
            Word { token, core, key: fold_diacritics(core).to_lowercase() }
        })
        .filter(|w| !w.key.is_empty())
        .collect()
}

/// Whether the words `needle` appear in a row inside `haystack`.
fn contains_run(haystack: &[Word<'_>], needle: &[&Word<'_>]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack.windows(needle.len()).any(|w| w.iter().zip(needle).all(|(a, b)| a.key == b.key))
}

/// Alignment of two word lists: pairs of matching positions, by longest
/// common subsequence of their keys.
fn align(a: &[Word<'_>], b: &[Word<'_>]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] =
                if a[i].key == b[j].key { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i].key == b[j].key {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

fn stage_of(heard: &[&Word<'_>], raw: &[Word<'_>], pre: &[Word<'_>]) -> (&'static str, &'static str) {
    if contains_run(raw, heard) {
        ("model", "Lo oyó así el modelo de voz")
    } else if contains_run(pre, heard) {
        ("dictionary", "Lo introdujo el diccionario o el filtro de muletillas")
    } else {
        ("format", "Lo introdujo el formateo del texto")
    }
}

/// Every difference between `text` (what was pasted) and `intended`, as
/// JSON edits the panel lists for the user to confirm.
///
/// Each edit has `op` (`replace`, `missing`, `extra`, `format`, `punct`),
/// `heard`, `meant`, `category`, `stage`, `why`, and `learn`: whether it is
/// worth teaching by default.
pub fn edits(raw: &str, pre: &str, text: &str, intended: &str) -> Vec<Value> {
    let (raw_w, pre_w, text_w, want_w) = (words(raw), words(pre), words(text), words(intended));
    let pairs = align(&text_w, &want_w);
    let mut out = Vec::new();

    let hunk = |ti: usize, tj: usize, ui: usize, uj: usize, out: &mut Vec<Value>| {
        let (mut ti, mut ui) = (ti, ui);
        // A short word is common enough that fixing it everywhere would break
        // other dictations ("eso" → "esto"): it is taught with the word
        // before it, which the two texts share ("todo eso" → "todo esto").
        let short_swap = tj - ti == 1 && uj - ui == 1 && text_w[ti].key.chars().count() < SHORT_WORD;
        let with_context = short_swap && ti > 0 && ui > 0 && pairs.contains(&(ti - 1, ui - 1));
        if with_context {
            ti -= 1;
            ui -= 1;
        }
        let heard: Vec<&Word<'_>> = text_w[ti..tj].iter().collect();
        let meant: Vec<&Word<'_>> = want_w[ui..uj].iter().collect();
        if heard.is_empty() && meant.is_empty() {
            return;
        }
        let heard_text = heard.iter().map(|w| w.core).collect::<Vec<_>>().join(" ");
        let meant_text = meant.iter().map(|w| w.core).collect::<Vec<_>>().join(" ");
        let (op, category, stage, why, learn) = if heard.is_empty() {
            (
                "missing",
                "Faltó una palabra",
                "",
                "El modelo no la captó o se perdió: no hay dónde anclar una corrección",
                false,
            )
        } else if meant.is_empty() {
            (
                "extra",
                "Sobra una palabra",
                "",
                "Puede ser una muletilla o un ruido que el modelo tomó por palabra",
                false,
            )
        } else {
            let similarity = strsim::normalized_levenshtein(
                &heard.iter().map(|w| w.key.as_str()).collect::<String>(),
                &meant.iter().map(|w| w.key.as_str()).collect::<String>(),
            );
            let (stage, why) = stage_of(&heard, &raw_w, &pre_w);
            let category = if similarity >= 0.6 { "Suena parecido" } else { "Otra palabra" };
            let short = heard.len() == 1 && heard[0].key.chars().count() < SHORT_WORD;
            let learn = stage == "model" && !short;
            let why = if short {
                "Demasiado corta para aprenderla sola: cambiaría demasiados dictados"
            } else if with_context {
                "Palabra corta: se aprende junto a la anterior para no cambiarla en otros dictados"
            } else {
                why
            };
            ("replace", category, stage, why, learn)
        };
        let stage = if stage.is_empty() && !heard.is_empty() { stage_of(&heard, &raw_w, &pre_w).0 } else { stage };
        out.push(json!({
            "op": op, "heard": heard_text, "meant": meant_text, "category": category,
            "stage": stage, "why": why, "learn": learn,
        }));
    };

    let (mut ti, mut ui) = (0, 0);
    for &(pi, pj) in &pairs {
        hunk(ti, pi, ui, pj, &mut out);
        // The words match; the way they were written may not.
        let (a, b) = (&text_w[pi], &want_w[pj]);
        if a.core != b.core {
            out.push(json!({
                "op": "format", "heard": a.core, "meant": b.core, "category": "Mayúsculas o acentos",
                "stage": "format", "why": "Mismas letras, distinta escritura", "learn": false,
            }));
        } else if a.token != b.token {
            out.push(json!({
                "op": "punct", "heard": a.token, "meant": b.token, "category": "Puntuación",
                "stage": "format", "why": "Mismas palabras, distinta puntuación", "learn": false,
            }));
        }
        ti = pi + 1;
        ui = pj + 1;
    }
    hunk(ti, text_w.len(), ui, want_w.len(), &mut out);
    out
}

/// If the user meant to say `wake_word` and the text starts with something
/// that sounds like it but is not, that spelling ("adam") is how the wake
/// word comes out for them.
pub fn wake_variant(wake_word: &str, text: &str, intended: &str) -> Option<String> {
    let wake = fold_diacritics(wake_word).to_lowercase();
    let first_wanted = words(intended).into_iter().next()?.key;
    let first_heard = words(text).into_iter().next()?.key;
    (first_wanted == wake && first_heard != wake && strsim::normalized_levenshtein(&first_heard, &wake) >= 0.4)
        .then_some(first_heard)
}

/// A replacement key: the folded, lowercase words of `heard`.
pub fn key_of(heard: &str) -> String {
    words(heard).into_iter().map(|w| w.key).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn field<'a>(edit: &'a Value, key: &str) -> &'a str {
        edit[key].as_str().unwrap()
    }

    #[test]
    fn a_misheard_word_is_a_model_error_worth_learning() {
        let e = edits("hola adam abre brave", "hola adam abre brave", "Hola adam abre brave.", "Hola Adán abre brave.");
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(
            (field(&e[0], "op"), field(&e[0], "heard"), field(&e[0], "meant")),
            ("replace", "Hola adam", "Hola Adán")
        );
        assert_eq!(field(&e[0], "stage"), "model");
        assert_eq!(field(&e[0], "category"), "Suena parecido");
        assert!(e[0]["learn"].as_bool().unwrap());
    }

    #[test]
    fn what_our_own_pipeline_introduced_is_not_offered_as_a_replacement() {
        // The model said "todo esto"; something after it turned it into "todo eso".
        let e = edits("dime todo esto", "dime todo eso", "Dime todo eso.", "Dime todo esto.");
        assert_eq!(field(&e[0], "stage"), "dictionary");
        assert!(!e[0]["learn"].as_bool().unwrap());
    }

    #[test]
    fn missing_extra_case_and_punctuation_are_told_apart() {
        let e = edits("a b c", "a b c", "A b c d.", "a x b c");
        let ops: Vec<&str> = e.iter().map(|x| field(x, "op")).collect();
        assert_eq!(ops, vec!["format", "missing", "extra"], "{e:?}");
        assert!(e.iter().all(|x| !x["learn"].as_bool().unwrap()));
        let e = edits("hola mundo", "hola mundo", "Hola mundo", "Hola, mundo.");
        assert_eq!(e.iter().map(|x| field(x, "op")).collect::<Vec<_>>(), vec!["punct", "punct"], "{e:?}");
    }

    #[test]
    fn a_short_word_is_taught_with_the_word_before_it() {
        let e = edits("dime todo eso", "dime todo eso", "Dime todo eso.", "Dime todo esto.");
        assert_eq!((field(&e[0], "heard"), field(&e[0], "meant")), ("todo eso", "todo esto"), "{e:?}");
        assert!(e[0]["learn"].as_bool().unwrap());
    }

    #[test]
    fn a_short_word_with_nothing_before_it_is_never_offered_for_learning() {
        let e = edits("voy casa", "voy casa", "Voy", "Ir");
        assert!(!e[0]["learn"].as_bool().unwrap(), "{e:?}");
    }

    #[test]
    fn the_wake_word_spelling_is_found_only_when_it_sounds_like_it() {
        assert_eq!(wake_variant("Adán", "Adam abre Brave", "Adán abre Brave"), Some("adam".to_string()));
        assert_eq!(wake_variant("Adán", "Hola abre Brave", "Adán abre Brave"), None);
        assert_eq!(wake_variant("Adán", "Adán abre Brave", "Adán abre Brave"), None);
    }
}
