//! The [`Desktop`] boundary: every primitive action on the real machine,
//! behind one trait, per `docs/ENGINEERING.md` #5. The gateway-aware
//! [`crate::service::LocalService`] is tested against
//! [`mock::MockDesktop`]; [`SystemDesktop`] is what production actually runs.

use crate::error::DesktopError;
use eva_macos::RunningAppInfo;

/// Everything an MCP tool call can ask the desktop to do.
pub trait Desktop: Send + Sync {
    /// Opens (or focuses) an application by name.
    fn open_app(&self, name: &str) -> Result<(), DesktopError>;
    /// Closes a running application by name.
    fn close_app(&self, name: &str) -> Result<(), DesktopError>;
    /// Opens a URL in the default handler.
    fn open_url(&self, url: &str) -> Result<(), DesktopError>;
    /// Pastes `text` at the current cursor position.
    fn insert_text(&self, text: &str) -> Result<(), DesktopError>;
    /// The frontmost application, if one can be determined.
    fn active_window(&self) -> Option<RunningAppInfo>;
    /// Shows a system notification.
    fn notify(&self, title: &str, body: &str) -> Result<(), DesktopError>;
    /// Speaks `text` aloud.
    fn speak(&self, text: &str) -> Result<(), DesktopError>;
    /// The text selected in the frontmost app, or `None` if nothing is.
    fn selected_text(&self) -> Result<Option<String>, DesktopError>;
    /// Puts `text` on the clipboard without pasting it — what to do with a
    /// dictation that cannot be pasted (a password field is focused).
    fn copy_text(&self, text: &str) -> Result<(), DesktopError>;
    /// Whether macOS is currently blocking synthesized keystrokes
    /// system-wide (a password field has focus, or a terminal has Secure
    /// Keyboard Entry on), which would silently swallow a paste.
    fn secure_input_active(&self) -> bool {
        false
    }
}

/// The real, production [`Desktop`]: `eva-macos` for app/window/paste
/// control, `notify-rust` for notifications, and the system `say` command
/// for speech — by default the voice chosen in `docs/PLAN.md` fase 7 (Mónica,
/// one of the two "premium" Spanish voices confirmed installed via `say -v
/// '?'` on this Mac, as opposed to the "novelty" ones that also match
/// `es_ES`). The voice comes from the config (`feedback.voice`).
pub struct SystemDesktop {
    voice: String,
}

impl SystemDesktop {
    /// A desktop that speaks with `voice` (any name `say -v '?'` lists).
    pub fn with_voice(voice: impl Into<String>) -> SystemDesktop {
        SystemDesktop { voice: voice.into() }
    }
}

impl Default for SystemDesktop {
    fn default() -> Self {
        SystemDesktop::with_voice("Mónica")
    }
}

impl Desktop for SystemDesktop {
    fn open_app(&self, name: &str) -> Result<(), DesktopError> {
        eva_macos::open_app(name).map_err(DesktopError::from)
    }

    fn close_app(&self, name: &str) -> Result<(), DesktopError> {
        eva_macos::close_app(name).map_err(DesktopError::from)
    }

    fn open_url(&self, url: &str) -> Result<(), DesktopError> {
        eva_macos::open_url(url).map_err(DesktopError::from)
    }

    fn insert_text(&self, text: &str) -> Result<(), DesktopError> {
        eva_macos::paste_text(text, eva_macos::DEFAULT_RESTORE_DELAY).map_err(DesktopError::from)
    }

    fn active_window(&self) -> Option<RunningAppInfo> {
        eva_macos::frontmost_app()
    }

    fn notify(&self, title: &str, body: &str) -> Result<(), DesktopError> {
        eva_macos::notification::show(title, body).map_err(DesktopError::from)
    }

    fn speak(&self, text: &str) -> Result<(), DesktopError> {
        // Fire-and-forget: speaking should not block the MCP tool call for
        // the duration of the speech.
        std::process::Command::new("say")
            .arg("-v")
            .arg(&self.voice)
            .arg("--")
            .arg(text)
            .spawn()
            .map(|_child| ())
            .map_err(DesktopError::from)
    }

    fn selected_text(&self) -> Result<Option<String>, DesktopError> {
        eva_macos::read_selection().map_err(DesktopError::from)
    }

    fn copy_text(&self, text: &str) -> Result<(), DesktopError> {
        use eva_macos::paste::Pasteboard;
        eva_macos::paste::SystemPasteboard.write_string(text).map_err(DesktopError::from)
    }

    fn secure_input_active(&self) -> bool {
        eva_macos::is_secure_input_enabled()
    }
}

/// An in-memory [`Desktop`] for tests, per `docs/ENGINEERING.md` #5.
pub mod mock {
    use super::{Desktop, DesktopError, RunningAppInfo};
    use std::sync::Mutex;

    /// One recorded call to a [`MockDesktop`] method.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Call {
        /// [`Desktop::open_app`] was called with this name.
        OpenApp(String),
        /// [`Desktop::close_app`] was called with this name.
        CloseApp(String),
        /// [`Desktop::open_url`] was called with this URL.
        OpenUrl(String),
        /// [`Desktop::insert_text`] was called with this text.
        InsertText(String),
        /// [`Desktop::notify`] was called with this title and body.
        Notify(String, String),
        /// [`Desktop::speak`] was called with this text.
        Speak(String),
        /// [`Desktop::selected_text`] was called.
        SelectedText,
        /// [`Desktop::copy_text`] was called with this text.
        CopyText(String),
    }

    /// Records every call made to it and, optionally, fails every call with
    /// a fixed error — useful for exercising the tool layer's error handling.
    #[derive(Default)]
    pub struct MockDesktop {
        calls: Mutex<Vec<Call>>,
        active_window: Option<RunningAppInfo>,
        selection: Option<String>,
        secure_input: bool,
        should_fail: bool,
    }

    impl MockDesktop {
        /// A mock that succeeds on every call and reports no active window.
        pub fn new() -> Self {
            MockDesktop::default()
        }

        /// A mock that reports `info` from [`Desktop::active_window`].
        #[must_use]
        pub fn with_active_window(mut self, info: RunningAppInfo) -> Self {
            self.active_window = Some(info);
            self
        }

        /// A mock that reports secure input as active, like a focused
        /// password field.
        #[must_use]
        pub fn with_secure_input(mut self) -> Self {
            self.secure_input = true;
            self
        }

        /// A mock whose [`Desktop::selected_text`] returns `text`.
        #[must_use]
        pub fn with_selection(mut self, text: &str) -> Self {
            self.selection = Some(text.to_string());
            self
        }

        /// A mock where every action fails.
        #[must_use]
        pub fn failing() -> Self {
            MockDesktop { should_fail: true, ..Default::default() }
        }

        /// The calls made to this mock, in order.
        pub fn calls(&self) -> Vec<Call> {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: Call) -> Result<(), DesktopError> {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            self.calls.lock().unwrap().push(call);
            if self.should_fail {
                Err(DesktopError::Macos(eva_macos::MacosError::NotificationFailed(
                    "mock configured to fail".to_string(),
                )))
            } else {
                Ok(())
            }
        }
    }

    impl Desktop for MockDesktop {
        fn open_app(&self, name: &str) -> Result<(), DesktopError> {
            self.record(Call::OpenApp(name.to_string()))
        }

        fn close_app(&self, name: &str) -> Result<(), DesktopError> {
            self.record(Call::CloseApp(name.to_string()))
        }

        fn open_url(&self, url: &str) -> Result<(), DesktopError> {
            self.record(Call::OpenUrl(url.to_string()))
        }

        fn insert_text(&self, text: &str) -> Result<(), DesktopError> {
            self.record(Call::InsertText(text.to_string()))
        }

        fn active_window(&self) -> Option<RunningAppInfo> {
            self.active_window.clone()
        }

        fn notify(&self, title: &str, body: &str) -> Result<(), DesktopError> {
            self.record(Call::Notify(title.to_string(), body.to_string()))
        }

        fn speak(&self, text: &str) -> Result<(), DesktopError> {
            self.record(Call::Speak(text.to_string()))
        }

        fn selected_text(&self) -> Result<Option<String>, DesktopError> {
            self.record(Call::SelectedText)?;
            Ok(self.selection.clone())
        }

        fn copy_text(&self, text: &str) -> Result<(), DesktopError> {
            self.record(Call::CopyText(text.to_string()))
        }

        fn secure_input_active(&self) -> bool {
            self.secure_input
        }
    }
}
