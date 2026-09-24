//! System notifications, under EVA01's own identity.
//!
//! Found by launching the packaged app for real: left to itself,
//! `notify-rust` asks AppleScript for a bundle id on its first notification,
//! and AppleScript answers an unknown application name with a modal "Where
//! is …?" dialog. On the shell's main thread that froze EVA before it had
//! even started the worker. And `notify-rust` does that lookup inside a
//! `Once`, so while the first call hangs every later one waits behind it —
//! in the worker, that would have been every task announcement and every
//! agent's `notify` tool call. Setting the identity explicitly, once, before
//! any notification skips the lookup for the whole process.

use crate::error::MacosError;
use std::sync::Once;

/// The identity notifications go out under: the packaged app's bundle
/// identifier (`packaging/Info.plist`). When the app is not installed (a
/// development build) macOS falls back to a generic sender, without asking.
pub const BUNDLE_ID: &str = "dev.eva01.app";

/// Shows a notification. Synchronous: call it off any thread that must stay
/// responsive.
///
/// # Errors
/// [`MacosError::NotificationFailed`] if macOS refused it.
pub fn show(title: &str, body: &str) -> Result<(), MacosError> {
    static IDENTITY: Once = Once::new();
    IDENTITY.call_once(|| {
        if let Err(e) = notify_rust::set_application(BUNDLE_ID) {
            tracing::debug!("identidad de notificaciones no fijada (¿EVA01.app no instalada?): {e}");
        }
    });
    notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
        .map(|_handle| ())
        .map_err(|e| MacosError::NotificationFailed(e.to_string()))
}
