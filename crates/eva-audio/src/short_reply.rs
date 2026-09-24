//! One-word replies ("sí", "vale", "gracias"), which the speech model cannot
//! be trusted with alone.
//!
//! With half a second of audio the model goes silent or invents ("Vale" →
//! "Ballet", "De acuerdo" → "Y ser como, camino a ir"): measured on 28 spoken
//! replies, 1 came out right without help and 11 with digital silence around
//! the clip. Said *three times in a row* in one audio, the same reply comes
//! out right 15–17 times of 28 whatever surrounds it, because the model then
//! has something to go on and three tries to get it right. So a short clip is
//! repeated, transcribed, and the repetitions vote on what was said.

use crate::capture::TARGET_SAMPLE_RATE;
use std::collections::HashMap;

const RATE: usize = TARGET_SAMPLE_RATE as usize;

/// A clip up to this long is a short reply (1.5 s).
const MAX_LEN: usize = 3 * RATE / 2;

/// How many times the clip is put into the audio: measured best at 3–4 (5 is worse).
const REPEATS: usize = 3;

/// Silence between two copies: long enough to be a pause between sayings.
const GAP: usize = RATE * 35 / 100;

/// Whether `samples` is short enough to need the repeat-and-vote treatment.
pub fn is_short_reply(samples: &[f32]) -> bool {
    !samples.is_empty() && samples.len() <= MAX_LEN
}

/// `samples` [`REPEATS`] times over, a pause between each copy.
pub fn repeated(samples: &[f32]) -> Vec<f32> {
    let mut audio = Vec::with_capacity(REPEATS * samples.len() + (REPEATS - 1) * GAP);
    for copy in 0..REPEATS {
        if copy > 0 {
            audio.extend(std::iter::repeat_n(0.0, GAP));
        }
        audio.extend_from_slice(samples);
    }
    audio
}

/// What was said once, out of a transcript of [`repeated`] audio: the phrase
/// the repetitions agree on ("Vale. Vale. Vale." → "Vale."), the most common
/// one when they disagree, the first when none repeats.
pub fn vote(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let cores: Vec<String> = tokens.iter().map(|t| core(t)).collect();

    // The whole text is one phrase said k times ("Sí, sí, sí." / "Gracias. Gracias.").
    for copies in [REPEATS, 2] {
        let n = cores.len();
        if n >= copies && n.is_multiple_of(copies) {
            let each = n / copies;
            if (1..copies).all(|c| cores[c * each..(c + 1) * each] == cores[..each]) {
                return closed(&tokens[..each], tokens[n - 1]);
            }
        }
    }

    // Otherwise the phrase that comes up most.
    if cores.len() <= 1 {
        return text.trim().to_string();
    }
    let phrases: Vec<&str> =
        text.split(['.', '?', '!', '…', ',', ';']).map(str::trim).filter(|p| !p.is_empty()).collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for phrase in &phrases {
        *counts.entry(phrase.split_whitespace().map(core).collect::<Vec<_>>().join(" ")).or_default() += 1;
    }
    let best = phrases.iter().max_by_key(|p| {
        let key = p.split_whitespace().map(core).collect::<Vec<_>>().join(" ");
        // The most common; on a tie the earliest (`max_by_key` keeps the last of equals).
        (counts[&key], std::cmp::Reverse(phrases.iter().position(|q| q == *p)))
    });
    best.map_or_else(String::new, |phrase| phrase.to_string())
}

/// The words of one saying, ended by the punctuation the last copy ended with.
fn closed(words: &[&str], last_token: &str) -> String {
    let joined = words.join(" ");
    let body = joined.trim_end_matches(|c: char| !c.is_alphanumeric());
    match last_token.chars().last().filter(|c| matches!(c, '.' | '?' | '!' | '…')) {
        Some(end) => format!("{body}{end}"),
        None => body.to_string(),
    }
}

/// The comparable core of a word: letters and digits, lowercase.
fn core(token: &str) -> String {
    token.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn only_a_clip_of_a_second_and_a_half_or_less_is_a_short_reply() {
        assert!(is_short_reply(&vec![0.2; RATE / 2]));
        assert!(is_short_reply(&vec![0.2; MAX_LEN]));
        assert!(!is_short_reply(&vec![0.2; MAX_LEN + 1]));
        assert!(!is_short_reply(&[]));
    }

    #[test]
    fn the_clip_is_repeated_three_times_with_a_pause_between() {
        let audio = repeated(&[0.5; 100]);
        assert_eq!(audio.len(), 3 * 100 + 2 * GAP);
        assert_eq!(audio[..100], [0.5; 100]);
        assert!(audio[100..100 + GAP].iter().all(|s| *s == 0.0));
        assert_eq!(audio[100 + GAP..200 + GAP], [0.5; 100]);
    }

    #[test]
    fn repetitions_that_agree_are_said_once_however_the_model_punctuated_them() {
        assert_eq!(vote("Vale. Vale. Vale."), "Vale.");
        assert_eq!(vote("Sí, sí, sí."), "Sí.");
        assert_eq!(vote("Gracias. Gracias."), "Gracias.");
        assert_eq!(vote("No, no, no."), "No.");
        assert_eq!(vote("De acuerdo. De acuerdo. De acuerdo."), "De acuerdo.");
        assert_eq!(vote("¿Sí? ¿Sí? ¿Sí?"), "¿Sí?");
        assert_eq!(vote("Correcto."), "Correcto.");
        assert_eq!(vote("siguiente"), "siguiente");
    }

    #[test]
    fn repetitions_that_disagree_go_to_the_most_common() {
        assert_eq!(vote("Vale. Ballet. Vale."), "Vale");
        assert_eq!(vote("Ballet. Vale. Vale."), "Vale");
    }

    #[test]
    fn when_nothing_repeats_the_first_is_kept_and_silence_stays_silence() {
        assert_eq!(vote("Uno. Dos. Tres."), "Uno");
        assert_eq!(vote(""), "");
        assert_eq!(vote("   "), "");
    }
}
