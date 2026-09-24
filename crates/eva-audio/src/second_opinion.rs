//! Two things measured on `canary-1b-flash` that the model alone gets wrong
//! on short clips, and what this module does about them.
//!
//! **It misses the wake word.** Short spoken commands ("Adán, abre Brave",
//! 1–2 s) came back as "Y luego, ¿cómo te crees?" or "Ah, entonces, verás
//! Spotify": "Adán" was recognized 2 times in 8. The same clips through the
//! much smaller `canary-180m-flash` gave "Adán" 7 times in 8 — but that model
//! drops the "ñ" in ordinary dictation ("ma ana"), so it cannot replace the big
//! one. [`SecondOpinion`] asks the small model only when a clip is short and
//! the big one's text is not what was wanted (a command), and takes its text
//! only if *it* is.
//!
//! **It loops.** On some short clips the decoder repeats itself until it runs
//! out of tokens: "Computa, bueno, bueno, bueno, …" dozens of times. That
//! would be pasted as is. A looping transcript also asks for the second
//! opinion, and whatever is kept has its loops collapsed ([`collapse_loops`]).

use crate::transcribe::{SpeechToText, TranscribeError, Transcript};
use crate::TARGET_SAMPLE_RATE;
use std::sync::Arc;
use std::time::Duration;

/// Clips up to this long are short enough to be a command, and cheap to
/// transcribe twice.
pub const SHORT_CLIP: Duration = Duration::from_secs(4);

/// A word said this many times in a row is a loop, not speech.
const LOOP_WORD_RUN: usize = 4;
/// A pair of words said this many times in a row is a loop too ("es porque
/// es porque es porque").
const LOOP_PAIR_RUN: usize = 3;

/// What a transcript must look like to be kept from the second opinion: in
/// the worker, "starts with the wake word".
pub type Wanted = dyn Fn(&str) -> bool + Send + Sync;

/// The speech-to-text EVA01 uses: the main model, an optional second model
/// for short clips, and loops collapsed in whatever comes out.
pub struct SecondOpinion {
    primary: Arc<dyn SpeechToText>,
    second: Option<Arc<dyn SpeechToText>>,
    wanted: Arc<Wanted>,
}

impl SecondOpinion {
    /// `primary` alone, or with `second` for short clips; `wanted` says which
    /// transcript of a short clip is the right one.
    pub fn new(primary: Arc<dyn SpeechToText>, second: Option<Arc<dyn SpeechToText>>, wanted: Arc<Wanted>) -> Self {
        SecondOpinion { primary, second, wanted }
    }

    fn choose(&self, samples: &[f32], first: Transcript) -> Transcript {
        let Some(second) = &self.second else { return first };
        let short = samples.len() as f64 <= SHORT_CLIP.as_secs_f64() * f64::from(TARGET_SAMPLE_RATE);
        let first_loops = has_loop(&first.text);
        let first_wanted = (self.wanted)(&first.text);
        if first_wanted || !(short || first_loops) {
            return first;
        }
        match second.transcribe(samples) {
            Ok(other) if (self.wanted)(&other.text) => other,
            Ok(other) if first_loops && !has_loop(&other.text) => other,
            Ok(_) => first,
            Err(e) => {
                tracing::warn!("el segundo modelo falló; se usa el primero: {e}");
                first
            }
        }
    }
}

impl SpeechToText for SecondOpinion {
    fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError> {
        let first = self.primary.transcribe(samples)?;
        let chosen = self.choose(samples, first);
        Ok(Transcript { text: collapse_loops(&chosen.text) })
    }
}

/// The comparable core of a word: letters and digits, lowercase.
fn core(token: &str) -> String {
    token.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Whether `text` repeats a word [`LOOP_WORD_RUN`] times in a row, or a pair
/// of words [`LOOP_PAIR_RUN`] times.
pub fn has_loop(text: &str) -> bool {
    let words: Vec<String> = text.split_whitespace().map(core).filter(|w| !w.is_empty()).collect();
    let word_loop = words.windows(LOOP_WORD_RUN).any(|run| run.iter().all(|w| *w == run[0]));
    let pair_loop = words.windows(2 * LOOP_PAIR_RUN).any(|run| {
        let (a, b) = (&run[0], &run[1]);
        a != b && run.chunks(2).all(|pair| pair[0] == *a && pair[1] == *b)
    });
    word_loop || pair_loop
}

/// `text` with every run of a repeated word or pair of words reduced to one
/// occurrence — "bueno, bueno, bueno, bueno." → "bueno." — keeping the
/// punctuation that ended the run. Text without a loop comes back unchanged
/// (a doubled "muy muy" is speech, and is left alone).
pub fn collapse_loops(text: &str) -> String {
    if !has_loop(text) {
        return text.to_string();
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let cores: Vec<String> = tokens.iter().map(|t| core(t)).collect();
    let mut kept: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let pair_len = run_of(&cores, i, 2);
        let word_len = run_of(&cores, i, 1);
        let (unit, repeats) = if pair_len >= LOOP_PAIR_RUN && pair_len * 2 > word_len {
            (2, pair_len)
        } else if word_len >= LOOP_WORD_RUN {
            (1, word_len)
        } else {
            kept.push(tokens[i].to_string());
            i += 1;
            continue;
        };
        let last = i + unit * repeats - 1;
        let mut once: Vec<String> = tokens[i..i + unit].iter().map(|t| t.to_string()).collect();
        // The run's own closing punctuation (a final "." after the last "bueno").
        if let (Some(end), Some(closing)) = (once.last_mut(), trailing_punctuation(tokens[last])) {
            *end = format!("{}{closing}", end.trim_end_matches(|c: char| !c.is_alphanumeric()));
        }
        kept.extend(once);
        i = last + 1;
    }
    kept.join(" ")
}

/// How many times the `unit` words starting at `start` repeat back to back.
fn run_of(cores: &[String], start: usize, unit: usize) -> usize {
    if start + unit > cores.len() || cores[start..start + unit].iter().any(String::is_empty) {
        return 0;
    }
    if unit == 2 && cores[start] == cores[start + 1] {
        return 0; // a doubled word is a word run, not a pair run
    }
    let pattern = &cores[start..start + unit];
    let mut repeats = 1;
    while start + (repeats + 1) * unit <= cores.len()
        && cores[start + repeats * unit..start + (repeats + 1) * unit] == *pattern
    {
        repeats += 1;
    }
    repeats
}

fn trailing_punctuation(token: &str) -> Option<&str> {
    let end = token.trim_end_matches(|c: char| !c.is_alphanumeric());
    let tail = &token[end.len()..];
    (!tail.is_empty()).then_some(tail)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::transcribe::mock::FixedTranscript;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_decoder_loop_is_recognized_and_collapsed() {
        let looping = "Computa, bueno, bueno, bueno, bueno, bueno, bueno, bueno.";
        assert!(has_loop(looping));
        assert_eq!(collapse_loops(looping), "Computa, bueno.");
        let pairs = "Ciertamente es porque es porque es porque es porque.";
        assert!(has_loop(pairs));
        assert_eq!(collapse_loops(pairs), "Ciertamente es porque.");
    }

    #[test]
    fn ordinary_repetition_in_speech_is_left_alone() {
        for text in ["muy muy bueno", "no, no, no quiero", "sí sí", "El clima de mañana será muy bueno."] {
            assert!(!has_loop(text), "{text}");
            assert_eq!(collapse_loops(text), text);
        }
    }

    /// A model that says `text` and counts how often it was asked.
    struct Counting {
        text: &'static str,
        asked: AtomicUsize,
    }

    impl SpeechToText for Counting {
        fn transcribe(&self, _samples: &[f32]) -> Result<Transcript, TranscribeError> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(Transcript { text: self.text.to_string() })
        }
    }

    fn counting(text: &'static str) -> Arc<Counting> {
        Arc::new(Counting { text, asked: AtomicUsize::new(0) })
    }

    fn starts_with_adan() -> Arc<Wanted> {
        Arc::new(|text: &str| text.to_lowercase().starts_with("adán") || text.to_lowercase().starts_with("adan"))
    }

    fn seconds(s: f64) -> Vec<f32> {
        vec![0.1; (s * f64::from(TARGET_SAMPLE_RATE)) as usize]
    }

    #[test]
    fn a_short_clip_whose_command_the_main_model_missed_takes_the_second_opinion() {
        let second = counting("Adán, abre Brave.");
        let stt = SecondOpinion::new(
            Arc::new(FixedTranscript::new("Y luego, ¿cómo te crees?")),
            Some(second.clone()),
            starts_with_adan(),
        );
        assert_eq!(stt.transcribe(&seconds(1.3)).unwrap().text, "Adán, abre Brave.");
    }

    #[test]
    fn the_second_opinion_is_only_taken_when_it_is_what_was_wanted() {
        let stt = SecondOpinion::new(
            Arc::new(FixedTranscript::new("El clima de mañana.")),
            Some(counting("El clima de ma ana.")),
            starts_with_adan(),
        );
        assert_eq!(stt.transcribe(&seconds(2.0)).unwrap().text, "El clima de mañana.", "the ñ is not traded away");
    }

    #[test]
    fn a_long_clip_or_a_command_already_heard_never_asks_twice() {
        let second = counting("Adán, algo.");
        let stt = SecondOpinion::new(
            Arc::new(FixedTranscript::new("Hola a todos.")),
            Some(second.clone()),
            starts_with_adan(),
        );
        stt.transcribe(&seconds(10.0)).unwrap();
        let heard = SecondOpinion::new(
            Arc::new(FixedTranscript::new("Adán, abre Brave.")),
            Some(second.clone()),
            starts_with_adan(),
        );
        heard.transcribe(&seconds(1.0)).unwrap();
        assert_eq!(second.asked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_looping_transcript_asks_for_the_second_opinion_even_when_long() {
        let stt = SecondOpinion::new(
            Arc::new(FixedTranscript::new("eve, eve, eve, eve, eve, eve.")),
            Some(counting("Eva, cierra Spotify.")),
            starts_with_adan(),
        );
        assert_eq!(stt.transcribe(&seconds(8.0)).unwrap().text, "Eva, cierra Spotify.");
    }

    #[test]
    fn without_a_second_model_a_loop_is_still_collapsed() {
        let stt =
            SecondOpinion::new(Arc::new(FixedTranscript::new("bueno, bueno, bueno, bueno.")), None, starts_with_adan());
        assert_eq!(stt.transcribe(&seconds(1.0)).unwrap().text, "bueno.");
    }

    proptest::proptest! {
        #[test]
        fn collapsing_never_panics_and_never_invents_words(text in "([a-z]{1,4}[,.]? ){0,30}") {
            let collapsed = collapse_loops(&text);
            for word in collapsed.split_whitespace().map(core) {
                proptest::prop_assert!(text.split_whitespace().map(core).any(|w| w == word));
            }
        }
    }
}
