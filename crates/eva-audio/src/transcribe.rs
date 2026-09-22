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
        let mut engine = self
            .engine
            .lock()
            .map_err(|_| TranscribeError::TranscriptionFailed("el modelo está en un estado inconsistente".to_string()))?;

        let result = engine
            .transcribe_with(samples, &transcribe_rs::whisper_cpp::WhisperInferenceParams::default())
            .map_err(|e| TranscribeError::TranscriptionFailed(e.to_string()))?;

        Ok(Transcript { text: result.text })
    }
}

/// A test double for [`SpeechToText`], per `docs/ENGINEERING.md` #5.
pub mod mock {
    use super::{SpeechToText, Transcript, TranscribeError};

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
}
