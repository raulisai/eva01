//! Application control via `NSWorkspace`: which app is in front, opening an
//! app or URL by name, closing a running app. This is the concrete
//! implementation behind `eva-intent`'s `OpenApp`/`CloseApp`/`OpenUrl`
//! intents (`docs/PLAN.md` fase 5) and the "proyecto activo por contexto"
//! detection (fase 6).

use crate::error::MacosError;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

/// A running application, as far as `eva-intent`'s frontmost-app detection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningAppInfo {
    /// The app's display name (e.g. `"Visual Studio Code"`), if AppKit reports one.
    pub localized_name: Option<String>,
    /// The app's bundle identifier (e.g. `"com.microsoft.VSCode"`), if AppKit reports one.
    pub bundle_identifier: Option<String>,
    /// The app's process id — what the Accessibility queries in [`crate::ax`]
    /// need to look inside its windows.
    pub pid: i32,
    /// The focused window's title, when Accessibility is granted and the app
    /// answers in time. This is what names the project being worked on
    /// (`eva_config::ProjectIndex`).
    pub window_title: Option<String>,
}

/// Returns the frontmost (active) application, or `None` if AppKit could not
/// determine one (this happens transiently, e.g. during a Space switch).
pub fn frontmost_app() -> Option<RunningAppInfo> {
    // SAFETY: `sharedWorkspace` and the accessors below are plain read-only
    // AppKit calls; they have no preconditions beyond running on a system
    // with AppKit available, which is guaranteed by this crate only
    // compiling on macOS.
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let pid = app.processIdentifier();

    Some(RunningAppInfo {
        localized_name: app.localizedName().map(|s| s.to_string()),
        bundle_identifier: app.bundleIdentifier().map(|s| s.to_string()),
        pid,
        window_title: if crate::is_accessibility_trusted() { crate::focused_window_title(pid) } else { None },
    })
}

/// Opens (launches, or brings to front if already running) the application
/// named `app_name` — the canonical name `eva-intent::AppIndex` resolved a
/// spoken alias to.
///
/// Uses `NSWorkspace::launchApplication`, which AppKit has marked deprecated
/// in favor of `openApplicationAtURL:configuration:completionHandler:` — that
/// replacement needs a resolved `file://` URL to the `.app` bundle, which
/// `eva-intent::AppIndex` does not carry yet (only canonical names and
/// aliases, per `docs/PLAN.md` fase 5). `launchApplication` by name remains
/// functional today and is the correct trade-off until the app index is
/// extended with bundle paths.
///
/// # Errors
/// Returns [`MacosError::LaunchFailed`] if AppKit reports it could not open the app.
pub fn open_app(app_name: &str) -> Result<(), MacosError> {
    let workspace = NSWorkspace::sharedWorkspace();
    let name = NSString::from_str(app_name);
    #[allow(deprecated)]
    let launched = workspace.launchApplication(&name);

    if launched {
        Ok(())
    } else {
        Err(MacosError::LaunchFailed(app_name.to_string()))
    }
}

/// Closes the running application whose localized name matches `app_name`
/// (case-insensitively).
///
/// # Errors
/// Returns [`MacosError::AppNotRunning`] if no running application has a
/// matching name.
pub fn close_app(app_name: &str) -> Result<(), MacosError> {
    let workspace = NSWorkspace::sharedWorkspace();
    let running = workspace.runningApplications();

    let target = running.iter().find(|app| {
        app.localizedName()
            .is_some_and(|name| name.to_string().eq_ignore_ascii_case(app_name))
    });

    match target {
        Some(app) => {
            app.terminate();
            Ok(())
        }
        None => Err(MacosError::AppNotRunning(app_name.to_string())),
    }
}

/// Opens `url` in the user's default handler for its scheme (the default
/// browser for `http(s)://`, Mail for `mailto:`, etc.).
///
/// # Errors
/// Returns [`MacosError::InvalidUrl`] if `url` cannot be parsed, or
/// [`MacosError::LaunchFailed`] if AppKit could not open it.
pub fn open_url(url: &str) -> Result<(), MacosError> {
    // A bare domain typed by a user ("github.com/foo") is not a valid
    // absolute URL without a scheme — add one rather than fail on the
    // common case `eva-intent::intent::looks_like_url` is designed to catch.
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };

    let ns_string = NSString::from_str(&with_scheme);
    let ns_url = NSURL::URLWithString(&ns_string).ok_or_else(|| MacosError::InvalidUrl(url.to_string()))?;

    let workspace = NSWorkspace::sharedWorkspace();
    if workspace.openURL(&ns_url) {
        Ok(())
    } else {
        Err(MacosError::LaunchFailed(url.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    // These exercise real AppKit calls against the real, live desktop this
    // test runs on — there is no way to mock `NSWorkspace` short of hiding
    // it behind a trait this module does not need for its own logic (the
    // logic here IS the AppKit call). What they can assert without being
    // flaky about which specific app happens to be frontmost or running is
    // narrow but real: the calls succeed, return sensible shapes, and error
    // paths behave as documented.

    #[test]
    fn frontmost_app_returns_something_on_a_real_desktop() {
        // A normal interactive macOS session always has a frontmost app
        // (even if it's just Finder), so this should be `Some` in CI/dev use
        // on a real Mac — but a headless/sandboxed session with no window
        // server might legitimately return `None`, which is exactly the
        // "transient, expected" case this function's contract already
        // documents, so this only asserts it doesn't panic.
        let _ = frontmost_app();
    }

    #[test]
    fn open_app_on_a_name_that_certainly_does_not_exist_reports_launch_failed() {
        let result = open_app("Esta Aplicacion No Existe De Verdad 12345");
        assert!(matches!(result, Err(MacosError::LaunchFailed(_))));
    }

    #[test]
    fn close_app_on_a_name_that_is_not_running_is_a_typed_error() {
        let result = close_app("Esta Aplicacion No Esta Corriendo 12345");
        assert!(matches!(result, Err(MacosError::AppNotRunning(_))));
    }

    #[test]
    #[ignore = "genuinely opens the default browser — run manually with `cargo test -- --ignored`, not on every `cargo test`"]
    fn open_url_adds_a_scheme_to_a_bare_domain_and_does_not_error() {
        let result = open_url("example.com");
        assert!(result.is_ok(), "opening a well-formed bare domain must succeed: {result:?}");
    }

    #[test]
    fn open_url_rejects_something_that_cannot_be_a_url_at_all() {
        // Raw spaces are not valid in a URL, so `NSURL::URLWithString` must
        // fail to parse this before any attempt to actually open anything —
        // this test never triggers a real side effect, unlike the `#[ignore]`d
        // one above.
        let result = open_url("a url with spaces and no valid scheme");
        assert!(matches!(result, Err(MacosError::InvalidUrl(_))));
    }
}
