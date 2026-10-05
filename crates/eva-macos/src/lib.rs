#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]
#![cfg(target_os = "macos")]

//! Everything in `docs/PLAN.md`'s crate list that has to talk to AppKit
//! directly: the overlay panel, reliable(-ish) paste, frontmost-app
//! detection and app control, and secure-input detection. See
//! `docs/PLAN.md` §3 and `docs/ENGINEERING.md` #5 (why the AppKit-touching
//! parts sit behind traits with mocks, not just get called from everywhere).
//!
//! This crate only compiles on macOS — Windows support is `docs/PLAN.md`
//! fase 10 "later" work, and there is no value in a stub implementation
//! that would compile elsewhere but panic or no-op at runtime.

pub mod ax;
pub mod duck;
pub mod error;
pub mod fnkey;
pub mod keys;
pub mod media;
pub mod notification;
pub mod overlay;
pub mod paste;
pub mod secure_input;
pub mod ui;
pub mod workspace;

pub use ax::{
    focused_text, focused_window_title, prompt_for_accessibility, selected_text, text_target_focused, watch_paste,
    FocusText, Landing,
};
pub use error::MacosError;
pub use fnkey::{FnKeyEvent, FnKeyMonitor};
pub use keys::press_combo;
pub use media::MediaCommand;
pub use overlay::{Activity, Choice, Icon, Overlay, OverlayContent, Tone, SEND_TAIL_SECS};
pub use paste::{copy_selection, copy_text, paste_text, DEFAULT_RESTORE_DELAY};
pub use secure_input::{is_accessibility_trusted, is_secure_input_enabled};
pub use workspace::{
    app_bundle_path, close_app, default_app_for, frontmost_app, installed_apps, is_app_running, open_app, open_url,
    RunningAppInfo,
};

/// The text selected in the frontmost app: read through Accessibility when
/// the app exposes it, otherwise by asking the app to copy it
/// ([`copy_selection`]) — which works in nearly anything with a Copy command
/// (terminals, most Electron apps) at the cost of borrowing the clipboard for
/// a few milliseconds. `Ok(None)` means nothing is selected.
///
/// # Errors
/// [`MacosError::SynthesizeKeystrokeFailed`] if the copy fallback was needed
/// and its keystroke could not be posted.
pub fn read_selection() -> Result<Option<String>, MacosError> {
    let frontmost_pid = frontmost_app().map(|app| app.pid);
    if is_accessibility_trusted() {
        if let Some(text) = selected_text(frontmost_pid) {
            return Ok(Some(text));
        }
    }
    copy_selection()
}
