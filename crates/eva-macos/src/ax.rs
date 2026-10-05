//! Accessibility (`AXUIElement`) queries: the title of the focused window
//! (which project is the user looking at — `docs/PLAN.md` fase 6), the text
//! they have selected (fase 9's edit mode), and the permission prompt itself.
//!
//! Every call carries a short messaging timeout: an app that is beachballing
//! must cost a query 400ms and an answer of "unknown", never a hung worker.
//! All of these need the same Accessibility permission the synthesized paste
//! does, and return `None` (not an error) without it — "nothing readable
//! here" is a normal answer, and the callers all degrade on it.

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use std::ptr::NonNull;

/// How long any single accessibility query waits on an unresponsive app.
const MESSAGING_TIMEOUT_SECS: f32 = 0.4;

/// Asks macOS to show its Accessibility permission dialog (once, if the
/// permission is missing) and reports whether it is already granted. This is
/// the piece that used to be missing: posting a `CGEvent` never triggers the
/// prompt by itself, so without this the user only ever learned about the
/// permission from a notification.
pub fn prompt_for_accessibility() -> bool {
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a valid, immortal `CFString`
    // constant, and the dictionary is exactly the `{prompt: true}` shape the
    // API documents; `AXIsProcessTrustedWithOptions` retains nothing beyond
    // the call.
    unsafe {
        let key: &CFString = objc2_application_services::kAXTrustedCheckOptionPrompt;
        let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
        objc2_application_services::AXIsProcessTrustedWithOptions(Some(options.as_opaque()))
    }
}

/// The title of the focused window of the application with process id `pid`.
pub fn focused_window_title(pid: i32) -> Option<String> {
    // SAFETY: `new_application` is passed a pid AppKit itself just reported;
    // an app that has since quit yields an element whose queries fail, which
    // every call below turns into `None`.
    let app = unsafe { AXUIElement::new_application(pid) };
    set_timeout(&app);
    let window = attribute(&app, "AXFocusedWindow")?;
    let window = window.downcast_ref::<AXUIElement>()?;
    string_attribute(window, "AXTitle")
}

/// The text currently selected in the focused UI element of whatever app is
/// in front, if that app exposes it. Many Chromium/Electron apps only build
/// their accessibility tree once asked to (`AXManualAccessibility`), so that
/// is set on the app first — harmless where unsupported.
pub fn selected_text(frontmost_pid: Option<i32>) -> Option<String> {
    if let Some(pid) = frontmost_pid {
        // SAFETY: as in `focused_window_title`.
        let app = unsafe { AXUIElement::new_application(pid) };
        set_timeout(&app);
        // SAFETY: `kCFBooleanTrue` is a valid `CFType` for this attribute.
        unsafe {
            let _ = app.set_attribute_value(&CFString::from_str("AXManualAccessibility"), CFBoolean::new(true));
        }
    }

    // SAFETY: the system-wide element takes no input.
    let system = unsafe { AXUIElement::new_system_wide() };
    set_timeout(&system);
    let focused = attribute(&system, "AXFocusedUIElement")?;
    let focused = focused.downcast_ref::<AXUIElement>()?;
    string_attribute(focused, "AXSelectedText").filter(|text| !text.is_empty())
}

/// Roles that are never somewhere to type: pasting into one goes nowhere.
/// Deliberately a list of what is certainly not text, not of what is: an
/// editor in a browser or an Electron app reports roles no list could
/// foresee, and a paste wrongly refused loses the user's words.
const NON_TEXT_ROLES: &[&str] = &[
    "AXButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXPopUpButton",
    "AXMenuButton",
    "AXMenuBar",
    "AXMenuItem",
    "AXStaticText",
    "AXImage",
    "AXList",
    "AXTable",
    "AXOutline",
    "AXBrowser",
    "AXRow",
    "AXColumn",
    "AXCell",
    "AXToolbar",
    "AXTabGroup",
    "AXSlider",
    "AXDockItem",
    "AXWindow",
    "AXApplication",
];

/// What the focused element holds, as far as it says: its text (`AXValue`)
/// and whether it has a text cursor. It is how a paste is checked afterwards —
/// the text either shows up in the value or it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusText {
    /// The element's whole text, when it exposes one.
    pub value: Option<String>,
    /// Whether it reports a selection range (a text cursor).
    pub has_cursor: bool,
    /// Its accessibility role, when it says.
    pub role: Option<String>,
}

/// The focused element of whatever app is in front, read for a paste: `None`
/// when it cannot be read at all (no permission, an app that hides its tree,
/// nothing focused).
pub fn focused_text(frontmost_pid: Option<i32>) -> Option<FocusText> {
    if !crate::is_accessibility_trusted() {
        return None;
    }
    if let Some(pid) = frontmost_pid {
        // SAFETY: as in `focused_window_title`.
        let app = unsafe { AXUIElement::new_application(pid) };
        set_timeout(&app);
        // SAFETY: `kCFBooleanTrue` is a valid `CFType` for this attribute.
        unsafe {
            let _ = app.set_attribute_value(&CFString::from_str("AXManualAccessibility"), CFBoolean::new(true));
        }
    }
    // SAFETY: the system-wide element takes no input.
    let system = unsafe { AXUIElement::new_system_wide() };
    set_timeout(&system);
    let focused = attribute(&system, "AXFocusedUIElement")?;
    let focused = focused.downcast_ref::<AXUIElement>()?;
    Some(FocusText {
        value: string_attribute(focused, "AXValue"),
        has_cursor: attribute(focused, "AXSelectedTextRange").is_some(),
        role: string_attribute(focused, "AXRole"),
    })
}

/// Whether what has the focus could take text at all. `false` when it is
/// certainly not a place for text — a list, a button, a page with no field
/// focused: it has no text of its own and no cursor, or it is a plain widget.
/// `true` when it is one or might be.
fn can_take_text(focus: &FocusText) -> bool {
    if focus.has_cursor || focus.value.is_some() {
        return true;
    }
    // Readable, no cursor, no text: nothing here to type into. (An editor that
    // hides all of that answers `None` from `focused_text`, not this; a role it
    // does not name is given the benefit of the doubt.)
    focus.role.is_none()
}

/// Whether a text can be pasted where the keyboard focus is: `Some(false)`
/// when the focused element is certainly not a place for text (a list, a
/// button, the bare desktop, a page with nothing focused), `Some(true)` when
/// it is one or might be, and `None` when that cannot be told (no permission,
/// nothing reported) — which callers treat as "try the paste", as before.
pub fn text_target_focused(frontmost_pid: Option<i32>) -> Option<bool> {
    let focus = focused_text(frontmost_pid)?;
    if focus.role.as_deref().is_some_and(|role| NON_TEXT_ROLES.contains(&role)) {
        return Some(false);
    }
    Some(can_take_text(&focus))
}

/// What became of a paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Landing {
    /// The text showed up where the cursor was.
    Landed,
    /// The field was readable and its text did not change: it went nowhere.
    NotLanded,
    /// It cannot be told (the app hides its text, or there is no permission).
    #[default]
    Unknown,
}

/// Compares the focused element's text before a paste with what it is now.
fn landing(before: &FocusText, after: Option<&FocusText>) -> Landing {
    let Some(before_text) = &before.value else { return Landing::Unknown };
    match after.and_then(|a| a.value.as_ref()) {
        Some(after_text) if after_text != before_text => Landing::Landed,
        Some(_) => Landing::NotLanded,
        None => Landing::Unknown,
    }
}

/// Watches the focused field after a paste for up to `wait`: [`Landing::Landed`]
/// as soon as its text changes; [`Landing::NotLanded`] if it stays the same;
/// [`Landing::Unknown`] when the field's text cannot be read. `before` is
/// [`focused_text`] taken just before the paste.
pub fn watch_paste(frontmost_pid: Option<i32>, before: Option<&FocusText>, wait: std::time::Duration) -> Landing {
    let Some(before) = before else { return Landing::Unknown };
    let started = std::time::Instant::now();
    loop {
        let now = focused_text(frontmost_pid);
        match landing(before, now.as_ref()) {
            Landing::NotLanded if started.elapsed() < wait => {}
            verdict => return verdict,
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

pub(crate) fn set_timeout(element: &AXUIElement) {
    // SAFETY: a plain call on a valid element with a finite timeout.
    unsafe {
        let _ = element.set_messaging_timeout(MESSAGING_TIMEOUT_SECS);
    }
}

/// Reads one attribute, following the Copy rule (the caller owns the
/// returned reference). `None` for any failure — no such attribute, no
/// permission, an app that did not answer in time.
pub(crate) fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut value: *const CFType = std::ptr::null();
    // SAFETY: `value` points at a live, writable pointer for the call.
    let error = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut value)) };
    if error != AXError::Success {
        return None;
    }
    let value = NonNull::new(value.cast_mut())?;
    // SAFETY: `AXUIElementCopyAttributeValue` follows the Copy rule — on
    // success `value` is a +1 retained object, which `from_raw` takes over.
    Some(unsafe { CFRetained::from_raw(value) })
}

pub(crate) fn string_attribute(element: &AXUIElement, name: &str) -> Option<String> {
    let value = attribute(element, name)?;
    value.downcast_ref::<CFString>().map(ToString::to_string)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_roles_that_rule_text_out_are_the_plain_widgets_not_the_editors() {
        for editor in
            ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField", "AXWebArea", "AXGroup", "AXScrollArea"]
        {
            assert!(!NON_TEXT_ROLES.contains(&editor), "{editor} may hold text");
        }
        assert!(NON_TEXT_ROLES.contains(&"AXButton") && NON_TEXT_ROLES.contains(&"AXList"));
    }

    fn focus(value: Option<&str>, has_cursor: bool, role: Option<&str>) -> FocusText {
        FocusText { value: value.map(String::from), has_cursor, role: role.map(String::from) }
    }

    #[test]
    fn a_field_with_a_cursor_or_text_takes_text_and_a_bare_readable_thing_does_not() {
        assert!(can_take_text(&focus(Some(""), false, Some("AXTextField"))));
        assert!(can_take_text(&focus(None, true, Some("AXWebArea"))), "a cursor is enough");
        assert!(!can_take_text(&focus(None, false, Some("AXWebArea"))), "a page with no field focused");
        assert!(!can_take_text(&focus(None, false, Some("AXGroup"))));
        assert!(can_take_text(&focus(None, false, None)), "unreadable role: try the paste");
    }

    #[test]
    fn a_paste_landed_when_the_fields_text_changed_and_not_when_it_stayed_the_same() {
        let before = focus(Some("hola"), true, Some("AXTextArea"));
        assert_eq!(landing(&before, Some(&focus(Some("hola mundo"), true, None))), Landing::Landed);
        assert_eq!(landing(&before, Some(&focus(Some("hola"), true, None))), Landing::NotLanded);
        assert_eq!(landing(&before, None), Landing::Unknown, "focus gone: cannot tell");
        let unreadable = focus(None, true, Some("AXTextArea"));
        assert_eq!(landing(&unreadable, Some(&focus(None, true, None))), Landing::Unknown, "no text to compare");
    }

    #[test]
    fn the_queries_never_crash_whatever_the_permission_or_focus_state_is() {
        // What comes back depends entirely on the real desktop and on
        // whether this test process was granted Accessibility, so the only
        // stable assertion is that the FFI plumbing returns instead of
        // crashing or hanging (the messaging timeout bounds the latter).
        let _ = focused_window_title(std::process::id() as i32);
        let _ = selected_text(None);
    }

    #[test]
    fn an_impossible_pid_yields_none_instead_of_an_error() {
        assert_eq!(focused_window_title(i32::MAX), None);
    }
}
