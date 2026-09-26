//! Errors this crate can return. Every fallible operation returns a
//! `Result<_, MacosError>` — see `docs/ENGINEERING.md` #2.

use thiserror::Error;

/// Something went wrong talking to AppKit or the OS.
#[derive(Debug, Error)]
pub enum MacosError {
    /// No application matching the given name/query is currently running.
    #[error("no hay ninguna aplicación en ejecución llamada \"{0}\"")]
    AppNotRunning(String),

    /// `NSWorkspace` refused to launch the named application (not installed,
    /// or the OS rejected the request).
    #[error("no se pudo abrir \"{0}\"")]
    LaunchFailed(String),

    /// The given string could not be parsed as a URL AppKit will open.
    #[error("\"{0}\" no es una URL válida")]
    InvalidUrl(String),

    /// The general pasteboard could not be written to.
    #[error("no se pudo escribir en el portapapeles")]
    PasteboardWriteFailed,

    /// Synthesizing the paste keystroke (Cmd+V) failed.
    #[error("no se pudo simular Cmd+V")]
    SynthesizeKeystrokeFailed,

    /// macOS refused to show a notification.
    #[error("no se pudo mostrar la notificación: {0}")]
    NotificationFailed(String),

    /// Reading or pressing things in another app's window needs the
    /// Accessibility permission, which EVA01 does not have.
    #[error("EVA01 necesita el permiso de Accesibilidad para manejar ventanas")]
    UiNotAllowed,

    /// The element was found but would not do what was asked.
    #[error("no pude usar «{0}»")]
    UiActionFailed(String),

    /// The music player (Spotify or Music) could not do what was asked.
    #[error("{0}")]
    MediaFailed(String),
}
