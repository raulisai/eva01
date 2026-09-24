#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! Microphone capture, voice-activity segmentation, and speech-to-text —
//! the pipeline from `docs/PLAN.md` §3 (`eva-audio`). Every piece that
//! touches real hardware or a real model sits behind a trait
//! (`docs/ENGINEERING.md` #5), so the logic that actually has bugs to find
//! — rechunking, resampling, the VAD hysteresis state machine — is tested
//! without a microphone or a downloaded model.

pub mod capture;
pub mod resample;
pub mod second_opinion;
pub mod segment;
pub mod short_reply;
pub mod transcribe;
pub mod vad;
pub mod wav;

pub use capture::{AudioError, AudioSource, CaptureHandle, InputInfo, MicrophoneSource, Rechunker, TARGET_SAMPLE_RATE};
pub use transcribe::{CanarySpeechToText, SpeechToText, TranscribeError, Transcript, WhisperSpeechToText};
#[cfg(feature = "hands-free-vad")]
pub use vad::SileroVad;
pub use vad::{SegmentEvent, Segmenter, SegmenterConfig, SpeechProbability, VadError};
