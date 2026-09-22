//! Detects macOS's "secure event input" mode (active while a password field
//! is focused, or in Terminal.app with Secure Keyboard Entry on), which
//! blocks synthesized keystrokes system-wide — including the Cmd+V
//! [`crate::paste`] relies on. `eva-worker` checks this before attempting to
//! paste, so a blocked paste is reported as "can't paste here right now"
//! instead of silently doing nothing.
//!
//! `IsSecureEventInputEnabled` has no binding in `objc2-core-graphics` or
//! `objc2-app-kit` (it is not part of either framework's Objective-C
//! surface — it is a plain C function from `Carbon/HIToolbox`, still fully
//! supported today despite living under a "Carbon" header). No crate wraps
//! this one function, so it is declared here directly via `extern "C"`,
//! linked against the `Carbon` framework.

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    /// Returns whether secure event input is currently enabled, system-wide.
    fn IsSecureEventInputEnabled() -> bool;
}

/// Returns `true` if macOS is currently blocking synthesized keystrokes
/// (a password field is focused, Terminal has Secure Keyboard Entry on, etc.).
pub fn is_secure_input_enabled() -> bool {
    // SAFETY: `IsSecureEventInputEnabled` takes no arguments, returns a
    // plain `Boolean`, and has no documented preconditions or side effects —
    // it is a pure query, safe to call from any thread at any time.
    unsafe { IsSecureEventInputEnabled() }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn calling_it_does_not_panic_or_crash() {
        // The actual value is entirely a function of whatever has focus on
        // the real desktop this test runs on, so there is nothing stable to
        // assert about *which* boolean comes back — only that the FFI call
        // itself is wired correctly and returns without incident.
        let _ = is_secure_input_enabled();
    }
}
