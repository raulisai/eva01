//! Speech-to-text, behind a small trait so `eva-worker`'s dispatch logic
//! never has to load a real model to be tested (`docs/ENGINEERING.md` #5).
//!
//! Two engines, both through `transcribe-rs`: [`CanarySpeechToText`], the
//! production default (`canary-1b-flash`, `docs/PLAN.md` §5; its measured
//! limits and what is done about them are in `crate::segment` and
//! `crate::second_opinion`), and [`WhisperSpeechToText`], used only when no
//! Canary model is installed and a Whisper file is.

use std::path::Path;
use thiserror::Error;

/// Errors loading a model or transcribing audio.
#[derive(Debug, Error)]
pub enum TranscribeError {
    /// The model file could not be loaded.
    #[error("no se pudo cargar el modelo: {0}")]
    ModelLoadFailed(String),
    /// Transcription failed after the model was loaded.
    #[error("la transcripción falló: {0}")]
    TranscriptionFailed(String),
}

/// The result of transcribing one utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// The recognized text, exactly as the engine produced it — no cleanup
    /// applied yet (that is `eva-text`'s job).
    pub text: String,
}

/// Something that turns 16 kHz mono f32 samples into text.
pub trait SpeechToText: Send + Sync {
    /// Transcribes `samples` (16 kHz mono).
    ///
    /// # Errors
    /// Returns [`TranscribeError::TranscriptionFailed`] if the engine could
    /// not process the audio.
    fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError>;
}

/// The real `transcribe-rs`-backed [`SpeechToText`], using the
/// `whisper-cpp` engine. See the module doc for why Whisper specifically.
pub struct WhisperSpeechToText {
    engine: std::sync::Mutex<transcribe_rs::whisper_cpp::WhisperEngine>,
}

impl WhisperSpeechToText {
    /// Loads a GGUF Whisper model from `model_path`.
    ///
    /// # Errors
    /// Returns [`TranscribeError::ModelLoadFailed`] if the file is missing
    /// or is not a model `transcribe-rs` recognizes.
    pub fn load(model_path: &Path) -> Result<Self, TranscribeError> {
        let engine = transcribe_rs::whisper_cpp::WhisperEngine::load(model_path)
            .map_err(|e| TranscribeError::ModelLoadFailed(e.to_string()))?;
        Ok(WhisperSpeechToText { engine: std::sync::Mutex::new(engine) })
    }
}

impl SpeechToText for WhisperSpeechToText {
    fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError> {
        let mut engine = self.engine.lock().map_err(|_| {
            TranscribeError::TranscriptionFailed("el modelo está en un estado inconsistente".to_string())
        })?;

        let result = engine
            .transcribe_with(samples, &transcribe_rs::whisper_cpp::WhisperInferenceParams::default())
            .map_err(|e| TranscribeError::TranscriptionFailed(e.to_string()))?;

        Ok(Transcript { text: result.text })
    }
}

/// The real `transcribe-rs`-backed [`SpeechToText`], using the `onnx`
/// feature's Canary engine — `docs/PLAN.md` §5's actual production default
/// (`canary-1b-flash`, native Spanish support, not a translation bolt-on).
/// Verified against the smaller `canary-180m-flash` variant (~213 MB vs.
/// 1B-flash's ~1.7 GB) in this increment, which exercises the exact same
/// `transcribe-rs` code path — swapping the model directory for
/// `canary-1b-flash` needs no code change, only a bigger download.
pub struct CanarySpeechToText {
    model: std::sync::Mutex<transcribe_rs::onnx::canary::CanaryModel>,
    /// BCP-47 language hint passed to every call — `docs/PLAN.md`'s target
    /// audience dictates in Spanish, so this defaults to `"es"` rather than
    /// leaving Canary to guess per utterance.
    language: String,
    padding: Padding,
}

/// Room added before the audio Canary sees. Measured, not guessed: on clips
/// that start speaking at sample zero (a recording begun by a key that is
/// already being held) the 1B model dropped the first word — "Hay que
/// actualizar…" came back as "Que actualizar…" — and 0.3–0.4 s of lead-in
/// fixed it. A real push-to-talk clip usually has some quiet already, so this
/// is a floor, not a delay: it costs the encoder a few frames, not time the
/// user waits on.
const LEAD_IN: std::time::Duration = std::time::Duration::from_millis(300);

/// Room added after the audio, so a last word cut off by the key release is
/// not also the end of the encoder's context.
const TAIL: std::time::Duration = std::time::Duration::from_millis(200);

/// A recording whose quietest 50 ms is below this (RMS, of 1.0) has a silent
/// background — a synthetic voice, a gated microphone — and digital silence
/// around it is consistent with the rest of the clip.
const SILENT_BACKGROUND: f32 = 0.001;

/// What surrounds a recording before Canary hears it.
///
/// Measured on `canary-1b-flash`, the same 12 phrases with different amounts
/// of background noise added (40, 30 dB below the speech; fan-like noise at 20
/// and 10 dB; WER, mean over the phrases):
///
/// | padding | clean | white −40 dB | fan −20 dB | fan −10 dB |
/// |---|---|---|---|---|
/// | digital silence | 4 % | 25 % | 27 % | 154 % |
/// | comfort noise at the clip's own floor | 12 % | 11 % | 8 % | 14 % |
/// | none | 7 % | 3 % | 5 % | 10 % |
///
/// A stretch of exact zeros next to a noisy recording is what the model
/// stumbles on, so it is only added where nothing is noisy for it to clash
/// with. (Comfort noise was tried to avoid the clash and lost to no padding
/// everywhere but the cleanest audio, so it was dropped.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Padding {
    /// Digital silence if the recording's background is silent, nothing
    /// otherwise.
    #[default]
    Auto,
    /// Digital silence, always. What fixes a first word lost when a clip starts
    /// speaking at sample zero, at the price of the noise sensitivity above.
    Silence,
    /// Nothing added.
    None,
}

impl Padding {
    /// The setting's name in `config.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Padding::Auto => "auto",
            Padding::Silence => "silence",
            Padding::None => "none",
        }
    }

    /// The padding a setting names (`auto`, `silence` or `none`).
    pub fn from_name(name: &str) -> Option<Padding> {
        [Padding::Auto, Padding::Silence, Padding::None].into_iter().find(|p| p.name() == name.trim())
    }

    /// `samples` with the lead-in before and the tail after, at the 16 kHz
    /// every [`SpeechToText`] input uses — or as they are.
    pub fn apply(self, samples: &[f32]) -> Vec<f32> {
        let padded = match self {
            Padding::None => false,
            Padding::Silence => true,
            Padding::Auto => quietest_level(samples) < SILENT_BACKGROUND,
        };
        if !padded {
            return samples.to_vec();
        }
        let rate = crate::capture::TARGET_SAMPLE_RATE as f64;
        let lead = (LEAD_IN.as_secs_f64() * rate) as usize;
        let tail = (TAIL.as_secs_f64() * rate) as usize;
        let mut out = Vec::with_capacity(lead + samples.len() + tail);
        out.resize(lead, 0.0);
        out.extend_from_slice(samples);
        out.resize(lead + samples.len() + tail, 0.0);
        out
    }
}

/// The RMS of the quietest 50 ms of `samples` (`INFINITY` if it is shorter
/// than that — then there is nothing to call quiet).
fn quietest_level(samples: &[f32]) -> f32 {
    const WINDOW: usize = 800;
    const HOP: usize = 160;
    let mut quietest = f32::INFINITY;
    let mut start = 0;
    while start + WINDOW <= samples.len() {
        let energy: f32 = samples[start..start + WINDOW].iter().map(|s| s * s).sum();
        quietest = quietest.min((energy / WINDOW as f32).sqrt());
        start += HOP;
    }
    quietest
}

impl CanarySpeechToText {
    /// Loads a Canary model from `model_dir` (the directory layout
    /// `transcribe-rs`'s own README documents: `encoder-model*.onnx`,
    /// `decoder-model*.onnx`, `vocab.txt`), int8-quantized.
    ///
    /// # Errors
    /// Returns [`TranscribeError::ModelLoadFailed`] if the directory is
    /// missing expected files or `transcribe-rs` otherwise rejects it.
    pub fn load(model_dir: &Path, language: impl Into<String>) -> Result<Self, TranscribeError> {
        let model = transcribe_rs::onnx::canary::CanaryModel::load(model_dir, &transcribe_rs::onnx::Quantization::Int8)
            .map_err(|e| TranscribeError::ModelLoadFailed(e.to_string()))?;
        Ok(CanarySpeechToText {
            model: std::sync::Mutex::new(model),
            language: language.into(),
            padding: Padding::default(),
        })
    }

    /// Uses `padding` around every recording instead of the default.
    #[must_use]
    pub fn with_padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }
}

impl SpeechToText for CanarySpeechToText {
    fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError> {
        let mut model = self.model.lock().map_err(|_| {
            TranscribeError::TranscriptionFailed("el modelo está en un estado inconsistente".to_string())
        })?;

        let params =
            transcribe_rs::onnx::canary::CanaryParams { language: Some(self.language.clone()), ..Default::default() };
        // The model only copes with utterance-sized audio: a long dictation is
        // fed a piece at a time, cut at its pauses (see `crate::segment`).
        let mut texts = Vec::new();
        for piece in crate::segment::split_at_pauses(samples) {
            let result = model
                .transcribe_with(&self.padding.apply(piece), &params)
                .map_err(|e| TranscribeError::TranscriptionFailed(e.to_string()))?;
            let text = result.text.trim();
            if !text.is_empty() {
                texts.push(text.to_string());
            }
        }
        Ok(Transcript { text: texts.join(" ") })
    }
}

/// A test double for [`SpeechToText`], per `docs/ENGINEERING.md` #5.
pub mod mock {
    use super::{SpeechToText, TranscribeError, Transcript};

    /// Always returns the same fixed transcript, regardless of input audio.
    pub struct FixedTranscript {
        text: String,
    }

    impl FixedTranscript {
        /// Builds a mock that always transcribes to `text`.
        pub fn new(text: impl Into<String>) -> Self {
            FixedTranscript { text: text.into() }
        }
    }

    impl SpeechToText for FixedTranscript {
        fn transcribe(&self, _samples: &[f32]) -> Result<Transcript, TranscribeError> {
            Ok(Transcript { text: self.text.clone() })
        }
    }

    /// Returns the given transcripts one call after another (the last one
    /// again when they run out), for a conversation of several utterances.
    pub struct SequenceTranscript {
        texts: std::sync::Mutex<std::collections::VecDeque<String>>,
        last: String,
    }

    impl SequenceTranscript {
        /// A mock that transcribes to each of `texts` in turn.
        pub fn new(texts: &[&str]) -> Self {
            SequenceTranscript {
                texts: std::sync::Mutex::new(texts.iter().map(|t| (*t).to_string()).collect()),
                last: texts.last().map(|t| (*t).to_string()).unwrap_or_default(),
            }
        }
    }

    impl SpeechToText for SequenceTranscript {
        fn transcribe(&self, _samples: &[f32]) -> Result<Transcript, TranscribeError> {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            let next = self.texts.lock().unwrap().pop_front();
            Ok(Transcript { text: next.unwrap_or_else(|| self.last.clone()) })
        }
    }

    /// Always fails, to exercise error-handling paths.
    pub struct AlwaysFails;

    impl SpeechToText for AlwaysFails {
        fn transcribe(&self, _samples: &[f32]) -> Result<Transcript, TranscribeError> {
            Err(TranscribeError::TranscriptionFailed("mock configured to fail".to_string()))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::mock::{AlwaysFails, FixedTranscript};
    use super::*;

    #[test]
    fn fixed_transcript_mock_ignores_the_audio_and_returns_its_text() {
        let stt = FixedTranscript::new("Adán, abre Brave.");
        let result = stt.transcribe(&[0.0; 512]).expect("mock never fails");
        assert_eq!(result.text, "Adán, abre Brave.");
    }

    #[test]
    fn always_fails_mock_reports_a_typed_error() {
        let stt = AlwaysFails;
        assert!(matches!(stt.transcribe(&[0.0; 512]), Err(TranscribeError::TranscriptionFailed(_))));
    }

    #[test]
    fn loading_a_model_from_a_path_that_does_not_exist_is_a_typed_error_not_a_panic() {
        let result = WhisperSpeechToText::load(Path::new("/no/existe/modelo.bin"));
        assert!(matches!(result, Err(TranscribeError::ModelLoadFailed(_))));
    }

    /// Speech-level "voice" with a stretch of the given background level.
    fn clip_with_background(background: f32) -> Vec<f32> {
        let mut clip = vec![0.3_f32; 16_000];
        clip[6_000..8_000].fill(background);
        clip
    }

    #[test]
    fn a_clip_with_a_silent_background_gets_digital_silence_around_it() {
        let clip = clip_with_background(0.0);
        let padded = Padding::Auto.apply(&clip);
        assert_eq!(padded.len(), 4_800 + clip.len() + 3_200);
        assert!(padded[..4_800].iter().all(|s| *s == 0.0));
        assert_eq!(&padded[4_800..4_800 + clip.len()], clip.as_slice());
    }

    #[test]
    fn a_noisy_clip_is_left_alone_because_zeros_next_to_noise_is_what_the_model_stumbles_on() {
        let clip = clip_with_background(0.01);
        assert_eq!(Padding::Auto.apply(&clip), clip);
    }

    #[test]
    fn a_clip_with_no_pause_at_all_is_left_alone_and_a_clip_too_short_to_judge_too() {
        assert_eq!(Padding::Auto.apply(&vec![0.4; 16_000]), vec![0.4; 16_000]);
        assert_eq!(Padding::Auto.apply(&[0.0; 100]), vec![0.0; 100], "under 50 ms there is nothing to call quiet");
    }

    #[test]
    fn the_explicit_modes_do_what_they_say_whatever_the_clip() {
        let noisy = clip_with_background(0.01);
        assert_eq!(Padding::None.apply(&noisy), noisy);
        assert_eq!(Padding::Silence.apply(&noisy).len(), noisy.len() + 8_000);
        for padding in [Padding::Auto, Padding::Silence, Padding::None] {
            assert_eq!(Padding::from_name(padding.name()), Some(padding));
        }
        assert_eq!(Padding::from_name("ruido"), None);
        assert_eq!(Padding::default(), Padding::Auto);
    }

    #[test]
    fn padding_adds_silence_on_both_sides_and_keeps_the_speech_intact() {
        let speech = vec![0.5_f32; 100];
        let padded = Padding::Silence.apply(&speech);
        let lead = 4_800; // 300 ms at 16 kHz
        assert_eq!(padded.len(), lead + 100 + 3_200); // + 200 ms tail
        assert!(padded[..lead].iter().all(|s| *s == 0.0));
        assert_eq!(&padded[lead..lead + 100], speech.as_slice());
        assert!(padded[lead + 100..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn loading_a_canary_model_from_a_missing_directory_is_a_typed_error_not_a_panic() {
        let result = CanarySpeechToText::load(Path::new("/no/existe/directorio"), "es");
        assert!(matches!(result, Err(TranscribeError::ModelLoadFailed(_))));
    }
}
