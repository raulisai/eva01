//! Two macOS permission/state queries with no binding in `objc2-app-kit` or
//! `objc2-core-graphics`, declared here directly via `extern "C"`:
//!
//! - [`is_secure_input_enabled`]: whether macOS is currently blocking
//!   synthesized keystrokes system-wide (a password field is focused, or
//!   Terminal has Secure Keyboard Entry on) — this would silently defeat
//!   the Cmd+V [`crate::paste`] relies on, so `eva-worker` checks it first
//!   and reports "can't paste here right now" instead of doing nothing.
//! - [`is_accessibility_trusted`]: whether the user has granted
//!   Accessibility access, which [`crate::paste`]'s synthesized Cmd+V
//!   silently requires. **Scope, stated plainly**: this only *checks* trust
//!   — it does not call `AXIsProcessTrustedWithOptions` to trigger the
//!   system permission prompt itself, because doing that safely needs
//!   building a `CFDictionary` (the `kAXTrustedCheckOptionPrompt` options)
//!   over raw Core Foundation FFI, which was not something to get right by
//!   guessing in this increment. `eva-shell` uses this check to show a
//!   clear, explicit notification with instructions instead — real,
//!   working guidance today, rather than a prompt trigger risked without
//!   being able to verify its memory safety.

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    /// Returns whether secure event input is currently enabled, system-wide.
    fn IsSecureEventInputEnabled() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    /// Returns whether this process is trusted for Accessibility access.
    fn AXIsProcessTrusted() -> bool;
}

/// Returns `true` if macOS is currently blocking synthesized keystrokes
/// (a password field is focused, Terminal has Secure Keyboard Entry on, etc.).
pub fn is_secure_input_enabled() -> bool {
    // SAFETY: `IsSecureEventInputEnabled` takes no arguments, returns a
    // plain `Boolean`, and has no documented preconditions or side effects —
    // it is a pure query, safe to call from any thread at any time.
    unsafe { IsSecureEventInputEnabled() }
}

/// Returns `true` if the user has granted this process Accessibility
/// access — required for [`crate::paste`]'s synthesized Cmd+V to actually
/// reach anything.
pub fn is_accessibility_trusted() -> bool {
    // SAFETY: `AXIsProcessTrusted` takes no arguments, returns a plain
    // `Boolean`, and is documented as a pure query safe to call at any time.
    unsafe { AXIsProcessTrusted() }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn calling_secure_input_check_does_not_panic_or_crash() {
        // The actual value is entirely a function of whatever has focus on
        // the real desktop this test runs on, so there is nothing stable to
        // assert about *which* boolean comes back — only that the FFI call
        // itself is wired correctly and returns without incident.
        let _ = is_secure_input_enabled();
    }

    #[test]
    fn calling_accessibility_trust_check_does_not_panic_or_crash() {
        let _ = is_accessibility_trusted();
    }
}
