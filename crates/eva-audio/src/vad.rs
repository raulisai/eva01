//! Segments a continuous stream of audio chunks into discrete utterances
//! using voice-activity probability, with hysteresis so a brief pause
//! mid-sentence does not cut an utterance in two and a short noise blip does
//! not open one at all.
//!
//! Per `docs/ENGINEERING.md` #5, the probability model itself sits behind
//! [`SpeechProbability`] so the actual interesting, bug-prone logic — *how
//! many consecutive quiet chunks end an utterance* — is tested against a
//! scripted fake, not the real Silero ONNX model.

use std::time::Duration;
use thiserror::Error;

/// Something that scores one chunk of audio as speech-probability in `[0.0, 1.0]`.
pub trait SpeechProbability: Send {
    /// Scores `chunk`, higher meaning more likely to contain speech.
    fn predict(&mut self, chunk: &[f32]) -> f32;
}

/// The real Silero-VAD-backed [`SpeechProbability`].
///
/// Gated behind the `hands-free-vad` Cargo feature (off by default) — see
/// the feature's own doc comment in `Cargo.toml` for why: it and
/// `transcribe-rs`'s `onnx` feature (Canary, this crate's real STT engine)
/// currently pin two different, mutually-exclusive exact versions of `ort`.
/// [`SpeechProbability`], [`Segmenter`], and [`mock::ScriptedProbability`]
/// stay available unconditionally; only this real implementation needs the
/// feature, and nothing in the MVP's push-to-talk flow uses it yet anyway
/// (`docs/PLAN.md` fase 10 is where it gets wired in).
#[cfg(feature = "hands-free-vad")]
pub struct SileroVad {
    inner: voice_activity_detector::VoiceActivityDetector,
}

/// Errors building or running the VAD.
#[derive(Debug, Error)]
pub enum VadError {
    /// The underlying model failed to initialize.
    #[error("no se pudo inicializar el detector de voz: {0}")]
    BuildFailed(String),
}

#[cfg(feature = "hands-free-vad")]
impl SileroVad {
    /// Builds a Silero VAD for `sample_rate` Hz audio delivered in chunks of
    /// `chunk_size` samples. Silero's published model expects 8 kHz or
    /// 16 kHz input with a matching chunk size (256/512 samples at 8 kHz,
    /// 512/1024 at 16 kHz) — passing a mismatched pair is a configuration
    /// bug the caller should fix, so it is reported as a build error rather
    /// than silently degraded.
    pub fn new(sample_rate: u32, chunk_size: usize) -> Result<Self, VadError> {
        let inner = voice_activity_detector::VoiceActivityDetector::builder()
            .sample_rate(sample_rate)
            .chunk_size(chunk_size)
            .build()
            .map_err(|e| VadError::BuildFailed(e.to_string()))?;
        Ok(SileroVad { inner })
    }
}

#[cfg(feature = "hands-free-vad")]
impl SpeechProbability for SileroVad {
    fn predict(&mut self, chunk: &[f32]) -> f32 {
        self.inner.predict(chunk.to_vec())
    }
}

/// Tuning for [`Segmenter`].
#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// The audio's sample rate, used to convert the millisecond thresholds
    /// below into a number of chunks.
    pub sample_rate: u32,
    /// How many samples each chunk passed to [`Segmenter::push_chunk`] has.
    pub chunk_size: usize,
    /// A chunk scoring at or above this is "speech".
    pub speech_threshold: f32,
    /// How long silence must persist after speech before the utterance is
    /// considered finished. Too short cuts words off at a natural pause;
    /// too long adds latency to every utterance.
    pub min_silence: Duration,
    /// Utterances shorter than this are discarded as noise, not surfaced as
    /// [`SegmentEvent::UtteranceReady`].
    pub min_speech: Duration,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        SegmenterConfig {
            sample_rate: 16_000,
            chunk_size: 512,
            speech_threshold: 0.5,
            min_silence: Duration::from_millis(500),
            min_speech: Duration::from_millis(250),
        }
    }
}

/// An event produced while feeding chunks to a [`Segmenter`].
#[derive(Debug, Clone, PartialEq)]
pub enum SegmentEvent {
    /// Speech was just detected after a period of silence.
    SpeechStarted,
    /// An utterance finished (silence persisted for `min_silence`) and met
    /// `min_speech`. Carries the utterance's samples, ready for STT.
    UtteranceReady(Vec<f32>),
}

#[derive(Debug)]
enum State {
    Idle,
    InSpeech { buffer: Vec<f32>, silence_run: usize },
}

/// Turns a stream of fixed-size audio chunks into discrete utterances.
pub struct Segmenter<P: SpeechProbability> {
    predictor: P,
    config: SegmenterConfig,
    state: State,
}

impl<P: SpeechProbability> Segmenter<P> {
    /// Builds a segmenter over the given probability source.
    pub fn new(predictor: P, config: SegmenterConfig) -> Self {
        Segmenter { predictor, config, state: State::Idle }
    }

    fn chunk_duration(&self) -> Duration {
        Duration::from_secs_f64(self.config.chunk_size as f64 / self.config.sample_rate as f64)
    }

    /// Feeds one chunk of audio (exactly `config.chunk_size` samples).
    /// Returns any events the chunk caused — usually none, occasionally
    /// [`SegmentEvent::SpeechStarted`] or [`SegmentEvent::UtteranceReady`].
    pub fn push_chunk(&mut self, chunk: &[f32]) -> Vec<SegmentEvent> {
        let probability = self.predictor.predict(chunk);
        let is_speech = probability >= self.config.speech_threshold;
        let chunk_duration = self.chunk_duration();
        let mut events = Vec::new();

        match &mut self.state {
            State::Idle => {
                if is_speech {
                    self.state = State::InSpeech { buffer: chunk.to_vec(), silence_run: 0 };
                    events.push(SegmentEvent::SpeechStarted);
                }
            }
            State::InSpeech { buffer, silence_run } => {
                buffer.extend_from_slice(chunk);
                if is_speech {
                    *silence_run = 0;
                } else {
                    *silence_run += 1;
                }

                let silence_elapsed = chunk_duration * (*silence_run as u32);
                if silence_elapsed >= self.config.min_silence {
                    let speech_samples = buffer.len().saturating_sub(*silence_run * self.config.chunk_size);
                    let speech_duration =
                        Duration::from_secs_f64(speech_samples as f64 / self.config.sample_rate as f64);

                    if speech_duration >= self.config.min_speech {
                        events.push(SegmentEvent::UtteranceReady(std::mem::take(buffer)));
                    }
                    // Below `min_speech`: discarded as noise, no event —
                    // either way the utterance is over, so return to `Idle`.
                    self.state = State::Idle;
                }
            }
        }

        events
    }
}

/// Test doubles for [`SpeechProbability`], per `docs/ENGINEERING.md` #5.
pub mod mock {
    use super::SpeechProbability;

    /// Returns a pre-scripted probability for each call, in order, then
    /// repeats the last value once the script is exhausted (so a test does
    /// not need to know exactly how many chunks a segmenter will consume).
    pub struct ScriptedProbability {
        script: Vec<f32>,
        index: usize,
    }

    impl ScriptedProbability {
        /// Builds a scripted predictor from a fixed sequence of probabilities.
        pub fn new(script: Vec<f32>) -> Self {
            ScriptedProbability { script, index: 0 }
        }
    }

    impl SpeechProbability for ScriptedProbability {
        fn predict(&mut self, _chunk: &[f32]) -> f32 {
            let value = self.script.get(self.index).or(self.script.last()).copied().unwrap_or(0.0);
            self.index += 1;
            value
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::mock::ScriptedProbability;
    use super::*;

    /// A config with tiny, test-friendly thresholds: 1 chunk of silence ends
    /// an utterance, 1 chunk of speech is enough to count.
    fn fast_config() -> SegmenterConfig {
        SegmenterConfig {
            sample_rate: 16_000,
            chunk_size: 512,
            speech_threshold: 0.5,
            min_silence: Duration::from_millis(1), // one 32ms chunk already exceeds this
            min_speech: Duration::from_millis(1),
        }
    }

    fn silent_chunk() -> Vec<f32> {
        vec![0.0; 512]
    }

    #[test]
    fn silence_only_never_starts_an_utterance() {
        let mut seg = Segmenter::new(ScriptedProbability::new(vec![0.0, 0.0, 0.0]), fast_config());
        for _ in 0..3 {
            assert!(seg.push_chunk(&silent_chunk()).is_empty());
        }
    }

    #[test]
    fn speech_then_silence_produces_started_then_ready() {
        let mut seg = Segmenter::new(ScriptedProbability::new(vec![0.9, 0.9, 0.0]), fast_config());

        let first = seg.push_chunk(&silent_chunk());
        assert_eq!(first, vec![SegmentEvent::SpeechStarted]);

        let second = seg.push_chunk(&silent_chunk());
        assert!(second.is_empty(), "still speaking, no event yet");

        let third = seg.push_chunk(&silent_chunk());
        assert_eq!(third.len(), 1);
        assert!(matches!(third[0], SegmentEvent::UtteranceReady(_)));
    }

    #[test]
    fn utterance_ready_carries_all_the_buffered_samples() {
        let mut seg = Segmenter::new(ScriptedProbability::new(vec![0.9, 0.0]), fast_config());
        seg.push_chunk(&silent_chunk());
        let events = seg.push_chunk(&silent_chunk());

        let SegmentEvent::UtteranceReady(samples) = &events[0] else {
            panic!("expected UtteranceReady");
        };
        assert_eq!(samples.len(), 512 * 2, "both chunks (speech + the trailing silence chunk) are included");
    }

    #[test]
    fn a_single_chunk_blip_below_min_speech_is_discarded_as_noise() {
        let config = SegmenterConfig {
            min_silence: Duration::from_millis(1),
            min_speech: Duration::from_secs(10), // nothing this short will ever qualify
            ..fast_config()
        };
        let mut seg = Segmenter::new(ScriptedProbability::new(vec![0.9, 0.0]), config);

        seg.push_chunk(&silent_chunk());
        let events = seg.push_chunk(&silent_chunk());
        assert!(events.is_empty(), "a blip shorter than min_speech must not surface an utterance");
    }

    #[test]
    fn a_brief_pause_mid_sentence_does_not_end_the_utterance() {
        // min_silence needs 3 consecutive quiet chunks; only 1 occurs before
        // speech resumes, so the utterance must keep going.
        let config = SegmenterConfig {
            min_silence: Duration::from_millis(100), // > 1 chunk (32ms) but reachable in a few
            min_speech: Duration::from_millis(1),
            ..fast_config()
        };
        let mut seg = Segmenter::new(ScriptedProbability::new(vec![0.9, 0.0, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0]), config);

        let mut saw_ready = false;
        for i in 0..4 {
            let events = seg.push_chunk(&silent_chunk());
            if events.iter().any(|e| matches!(e, SegmentEvent::UtteranceReady(_))) {
                saw_ready = true;
                assert!(i >= 2, "must not end on the single brief pause at chunk 1");
            }
        }
        // With the resumed speech at chunks 2-3, the run of silence restarts
        // and does not accumulate past the pause — the utterance is still
        // open after 4 chunks in this script.
        assert!(!saw_ready, "the brief mid-sentence pause must not have ended the utterance yet");
    }

    #[test]
    fn returns_to_idle_after_an_utterance_and_can_start_a_new_one() {
        let mut seg = Segmenter::new(ScriptedProbability::new(vec![0.9, 0.0, 0.0, 0.9, 0.0]), fast_config());

        seg.push_chunk(&silent_chunk()); // start
        seg.push_chunk(&silent_chunk()); // ready (min_silence=1 chunk)
        seg.push_chunk(&silent_chunk()); // idle, silence, no-op — actually this call uses the 3rd script value
        let restart = seg.push_chunk(&silent_chunk());
        assert_eq!(restart, vec![SegmentEvent::SpeechStarted], "must be able to start a fresh utterance");
    }
}
