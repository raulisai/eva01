//! The [`Desktop`] boundary: every action an MCP tool call can trigger on
//! the real machine, behind one trait, per `docs/ENGINEERING.md` #5. The
//! MCP tool-routing logic in [`crate::server`] is tested against
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
}

/// The real, production [`Desktop`]: `eva-macos` for app/window/paste
/// control, `notify-rust` for notifications, and the system `say` command
/// for speech — the voice chosen in `docs/PLAN.md` fase 7 (Mónica, one of
/// the two "premium" Spanish voices confirmed installed via `say -v '?'`
/// on this Mac, as opposed to the "novelty" ones that also match `es_ES`).
pub struct SystemDesktop;

/// The `say` voice used for [`Desktop::speak`]. A fixed choice for now;
/// making it configurable is a small follow-up once settings exist.
const SPEAK_VOICE: &str = "Mónica";

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
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show()
            .map(|_handle| ())
            .map_err(|e| DesktopError::NotifyFailed(e.to_string()))
    }

    fn speak(&self, text: &str) -> Result<(), DesktopError> {
        // Fire-and-forget: speaking should not block the MCP tool call for
        // the duration of the speech.
        std::process::Command::new("say")
            .arg("-v")
            .arg(SPEAK_VOICE)
            .arg(text)
            .spawn()
            .map(|_child| ())
            .map_err(DesktopError::from)
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
    }

    /// Records every call made to it and, optionally, fails every call with
    /// a fixed error — useful for exercising the tool layer's error handling.
    #[derive(Default)]
    pub struct MockDesktop {
        calls: Mutex<Vec<Call>>,
        active_window: Option<RunningAppInfo>,
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
                Err(DesktopError::NotifyFailed("mock configured to fail".to_string()))
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
    }
}
