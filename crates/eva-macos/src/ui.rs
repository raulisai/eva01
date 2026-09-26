//! Finding and pressing things in the window in front by the label a person
//! would read on them — "Suscribirse", "Buscar" — through the Accessibility
//! tree, with no picture of the screen involved: the same permission that
//! lets EVA01 paste. It is what lets "haz clic en …" work in an app that has
//! no shortcut, and "busca …" find the search box of a page or app that gives
//! it none.
//!
//! A web page only shows its tree to assistive tools once asked
//! (`AXManualAccessibility`, as the selection reader does), and a busy page
//! has thousands of nodes, so the walk has a budget: it looks at the window in
//! front, breadth first, for at most [`MAX_NODES`] elements or [`MAX_TIME`].

use crate::ax::{attribute, set_timeout, string_attribute};
use crate::error::MacosError;
use eva_text::fold_diacritics;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString, CFType};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The most elements looked at in one search.
const MAX_NODES: usize = 6_000;
/// The longest a search looks.
const MAX_TIME: Duration = Duration::from_millis(2_500);

/// What kind of thing is being looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Something to press: a button, a link, a menu entry, a tab, a checkbox.
    Pressable,
    /// A place to type: a text field, a text area, a search field, a combo box.
    Field,
}

/// Roles that can be pressed.
const PRESSABLE_ROLES: &[&str] = &[
    "AXButton",
    "AXLink",
    "AXMenuItem",
    "AXMenuBarItem",
    "AXMenuButton",
    "AXPopUpButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXTab",
    "AXDisclosureTriangle",
    "AXCell",
    "AXRow",
    "AXImage",
    "AXStaticText",
    "AXGroup",
];

/// Roles that take typing.
const FIELD_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXSearchField", "AXComboBox"];

/// The attributes an element may carry its label in, most telling first.
const LABEL_ATTRIBUTES: &[&str] = &["AXTitle", "AXDescription", "AXPlaceholderValue", "AXValue", "AXHelp"];

/// What was found.
pub struct Target {
    element: CFRetained<AXUIElement>,
    /// Its role ("AXButton").
    pub role: String,
    /// The label it matched by.
    pub label: String,
}

/// `text` as words: no accents, lowercase, split on anything not a letter or digit.
fn words(text: &str) -> Vec<String> {
    fold_diacritics(text)
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// How well `label` answers to what was asked for (`wanted`, already split
/// into words): the whole label, its beginning, all the words or nothing.
/// Higher is better; a shorter label beats a longer one on equal terms.
fn score(wanted: &[String], label: &str) -> Option<u32> {
    let label_words = words(label);
    if wanted.is_empty() || label_words.is_empty() {
        return None;
    }
    let base = if label_words == wanted {
        1_000
    } else if label_words.starts_with(wanted) {
        800
    } else if label_words.windows(wanted.len()).any(|run| run == wanted) {
        600
    } else if wanted.iter().all(|w| label_words.contains(w)) {
        400
    } else {
        return None;
    };
    // A label that is mostly something else says less about this element.
    Some(base - label_words.len().min(50) as u32)
}

/// Whether an element of `role` (or `subrole`) is what `want` asks for.
fn role_fits(want: Want, role: &str, subrole: Option<&str>) -> bool {
    match want {
        Want::Pressable => PRESSABLE_ROLES.contains(&role),
        Want::Field => FIELD_ROLES.contains(&role) || subrole == Some("AXSearchField"),
    }
}

/// Looks in the window in front of the app `pid` for the element whose label
/// best matches any of `labels` ("search", "buscar").
///
/// # Errors
/// [`MacosError::UiNotAllowed`] without the Accessibility permission.
pub fn find(pid: i32, labels: &[&str], want: Want) -> Result<Option<Target>, MacosError> {
    if !crate::is_accessibility_trusted() {
        return Err(MacosError::UiNotAllowed);
    }
    let wanted: Vec<Vec<String>> = labels.iter().map(|l| words(l)).filter(|w| !w.is_empty()).collect();
    // SAFETY: a plain constructor for the app element of a process id.
    let app = unsafe { AXUIElement::new_application(pid) };
    set_timeout(&app);
    // SAFETY: `kCFBooleanTrue` is a valid `CFType` for this attribute.
    unsafe {
        let _ = app.set_attribute_value(&CFString::from_str("AXManualAccessibility"), CFBoolean::new(true));
    }
    let Some(window) = front_window(&app) else { return Ok(None) };

    let started = Instant::now();
    let mut queue = VecDeque::from([window]);
    let mut looked_at = 0;
    let mut best: Option<(u32, Target)> = None;
    while let Some(element) = queue.pop_front() {
        looked_at += 1;
        if looked_at > MAX_NODES || started.elapsed() > MAX_TIME {
            break;
        }
        set_timeout(&element);
        let role = string_attribute(&element, "AXRole").unwrap_or_default();
        let subrole = string_attribute(&element, "AXSubrole");
        if role_fits(want, &role, subrole.as_deref()) {
            for attribute_name in LABEL_ATTRIBUTES {
                let Some(label) = string_attribute(&element, attribute_name) else { continue };
                let points = wanted.iter().filter_map(|w| score(w, &label)).max();
                if let Some(points) = points.filter(|p| best.as_ref().is_none_or(|(b, _)| p > b)) {
                    best = Some((points, Target { element: element.clone(), role: role.clone(), label }));
                }
            }
        }
        if let Some(children) = attribute(&element, "AXChildren") {
            if let Some(children) = children.downcast_ref::<CFArray>() {
                // SAFETY: `AXChildren` is documented to hold `AXUIElementRef` entries.
                let children: &CFArray<AXUIElement> = unsafe { children.cast_unchecked() };
                queue.extend(children.iter());
            }
        }
    }
    Ok(best.map(|(_, target)| target))
}

/// The window in front of `app`: its focused one, else its main one.
fn front_window(app: &AXUIElement) -> Option<CFRetained<AXUIElement>> {
    ["AXFocusedWindow", "AXMainWindow"].iter().find_map(|name| {
        let window: CFRetained<CFType> = attribute(app, name)?;
        window.downcast_ref::<AXUIElement>().map(CFRetained::from)
    })
}

/// Presses `target`, as a click would.
///
/// # Errors
/// [`MacosError::UiActionFailed`] if the element does not take the press.
pub fn press(target: &Target) -> Result<(), MacosError> {
    // SAFETY: a plain call on a valid element with a constant action name.
    let error = unsafe { target.element.perform_action(&CFString::from_str("AXPress")) };
    if error == objc2_application_services::AXError::Success {
        Ok(())
    } else {
        Err(MacosError::UiActionFailed(target.label.clone()))
    }
}

/// Moves the keyboard focus to `target`.
///
/// # Errors
/// [`MacosError::UiActionFailed`] if the element does not take focus.
pub fn focus(target: &Target) -> Result<(), MacosError> {
    // SAFETY: `kCFBooleanTrue` is a valid `CFType` for this attribute.
    let error = unsafe { target.element.set_attribute_value(&CFString::from_str("AXFocused"), CFBoolean::new(true)) };
    if error == objc2_application_services::AXError::Success {
        Ok(())
    } else {
        Err(MacosError::UiActionFailed(target.label.clone()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn points(wanted: &str, label: &str) -> Option<u32> {
        score(&words(wanted), label)
    }

    #[test]
    fn a_label_is_matched_by_its_words_whatever_the_accents_or_case() {
        assert!(points("suscribirse", "Suscribirse").is_some());
        assert!(points("buscar", "BÚSQUEDA").is_none(), "a different word is a different word");
        assert!(points("busqueda", "Búsqueda").is_some());
        assert!(points("ajustes", "Ajustes del sistema").is_some());
    }

    #[test]
    fn the_whole_label_beats_its_beginning_beats_a_part_beats_scattered_words() {
        let exact = points("search", "Search").unwrap();
        let begins = points("search", "Search YouTube").unwrap();
        let inside = points("search", "Type to search").unwrap();
        let scattered = points("search box", "box to search").unwrap();
        assert!(exact > begins && begins > inside && inside > scattered, "{exact} {begins} {inside} {scattered}");
        assert_eq!(points("search", "Settings"), None);
        assert_eq!(points("", "Search"), None);
    }

    #[test]
    fn of_two_matches_the_shorter_label_is_the_more_exact_one() {
        assert!(points("search", "Search") > points("search", "Search for videos, channels and playlists"));
    }

    #[test]
    fn a_field_is_a_place_to_type_and_a_button_is_a_place_to_press() {
        assert!(role_fits(Want::Field, "AXTextField", None));
        assert!(role_fits(Want::Field, "AXGroup", Some("AXSearchField")));
        assert!(!role_fits(Want::Field, "AXButton", None));
        assert!(role_fits(Want::Pressable, "AXButton", None));
        assert!(role_fits(Want::Pressable, "AXLink", None));
        assert!(!role_fits(Want::Pressable, "AXTextField", None));
    }

    #[test]
    #[ignore = "needs the Accessibility permission and an app in front — run by hand"]
    fn the_app_in_front_can_be_looked_into_without_crashing() {
        let pid = crate::frontmost_app().map(|app| app.pid).unwrap();
        let _ = find(pid, &["search", "buscar"], Want::Field);
    }
}
