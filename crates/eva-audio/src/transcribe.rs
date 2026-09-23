//! Speech-to-text, behind a small trait so `eva-worker`'s dispatch logic
//! never has to load a real model to be tested (`docs/ENGINEERING.md` #5).
//!
//! **Model choice, stated plainly:** `docs/PLAN.md` §5 names
//! `canary-1b-flash` (via `transcribe-rs`'s `onnx` feature) as the default
//! production model, chosen for Spanish. This module's concrete
//! implementation, [`WhisperSpeechToText`], instead wraps `transcribe-rs`'s
//! `whisper-cpp` engine. That is a deliberate, narrow choice for *this*
//! increment, not a change to the model decision: Whisper ships as one
//! self-contained GGUF file, so it is the one candidate that could actually
//! be downloaded and run end-to-end for real in the time this session had —
//! Canary/Parakeet need a directory of ONNX files whose exact layout was not
//! verified against a real download here. `SpeechToText` does not know or
//! care which engine backs it, so swapping in the Canary implementation
//! later is a new `impl`, not a redesign.

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
}

/// Silence added before the audio Canary sees. Measured, not guessed: on
/// clips that start speaking at sample zero (a recording begun by a key that
/// is already being held) the 1B model dropped the first word — "Hay que
/// actualizar…" came back as "Que actualizar…" — and 0.3–0.4 s of lead-in
/// fixed it. A real push-to-talk clip usually has some silence already, so
/// this is a floor, not a delay: it costs the encoder a few frames, not time
/// the user waits on.
const LEAD_IN: std::time::Duration = std::time::Duration::from_millis(300);

/// Silence added after the audio, so a last word cut off by the key release
/// is not also the end of the encoder's context.
const TAIL: std::time::Duration = std::time::Duration::from_millis(200);

/// `samples` with [`LEAD_IN`] of silence before and [`TAIL`] after, at the
/// 16 kHz every [`SpeechToText`] input uses.
fn padded_with_silence(samples: &[f32]) -> Vec<f32> {
    let rate = crate::capture::TARGET_SAMPLE_RATE as f64;
    let lead = (LEAD_IN.as_secs_f64() * rate) as usize;
    let tail = (TAIL.as_secs_f64() * rate) as usize;
    let mut padded = Vec::with_capacity(lead + samples.len() + tail);
    padded.resize(lead, 0.0);
    padded.extend_from_slice(samples);
    padded.resize(lead + samples.len() + tail, 0.0);
    padded
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
        Ok(CanarySpeechToText { model: std::sync::Mutex::new(model), language: language.into() })
    }
}

impl SpeechToText for CanarySpeechToText {
    fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError> {
        let mut model = self.model.lock().map_err(|_| {
            TranscribeError::TranscriptionFailed("el modelo está en un estado inconsistente".to_string())
        })?;

        let params =
            transcribe_rs::onnx::canary::CanaryParams { language: Some(self.language.clone()), ..Default::default() };
        let result = model
            .transcribe_with(&padded_with_silence(samples), &params)
            .map_err(|e| TranscribeError::TranscriptionFailed(e.to_string()))?;

        Ok(Transcript { text: result.text })
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

    #[test]
    fn padding_adds_silence_on_both_sides_and_keeps_the_speech_intact() {
        let speech = vec![0.5_f32; 100];
        let padded = padded_with_silence(&speech);
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
