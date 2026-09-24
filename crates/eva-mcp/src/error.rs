//! Errors this crate can return. See `docs/ENGINEERING.md` #2.

use thiserror::Error;

/// Something went wrong performing a desktop action.
#[derive(Debug, Error)]
pub enum DesktopError {
    /// The underlying macOS call failed.
    #[error(transparent)]
    Macos(#[from] eva_macos::MacosError),

    /// Launching `say` for text-to-speech failed.
    #[error("no se pudo iniciar la síntesis de voz: {0}")]
    SpeakFailed(#[from] std::io::Error),
}
