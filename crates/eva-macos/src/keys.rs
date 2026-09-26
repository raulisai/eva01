//! Pressing a key or a shortcut on the user's behalf ("/", "cmd+l", "return"),
//! as a person would: what focuses the search box of the page in front so a
//! search can be typed *there*. Needs the same Accessibility permission the
//! paste does.
//!
//! A shortcut is written the way the config writes them: modifiers joined
//! with `+` and the key last (`cmd+a`). A lone printable character (`/`) is
//! sent as that *character*, not as a key position, so it is the same on a
//! Spanish keyboard, where "/" is not where an American one has it.

use crate::error::MacosError;

/// Virtual keycodes of a US layout (`kVK_ANSI_*` in `Carbon/HIToolbox/Events.h`),
/// for the letters, digits and named keys shortcuts use.
const KEYCODES: &[(&str, u16)] = &[
    ("a", 0x00),
    ("s", 0x01),
    ("d", 0x02),
    ("f", 0x03),
    ("h", 0x04),
    ("g", 0x05),
    ("z", 0x06),
    ("x", 0x07),
    ("c", 0x08),
    ("v", 0x09),
    ("b", 0x0B),
    ("q", 0x0C),
    ("w", 0x0D),
    ("e", 0x0E),
    ("r", 0x0F),
    ("y", 0x10),
    ("t", 0x11),
    ("o", 0x1F),
    ("u", 0x20),
    ("i", 0x22),
    ("p", 0x23),
    ("l", 0x25),
    ("j", 0x26),
    ("k", 0x28),
    ("n", 0x2D),
    ("m", 0x2E),
    ("return", 0x24),
    ("enter", 0x24),
    ("tab", 0x30),
    ("space", 0x31),
    ("escape", 0x35),
    ("esc", 0x35),
];

/// One shortcut, understood.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Combo {
    /// A printable character on its own, sent as that character.
    Character(char),
    /// A key with modifiers held.
    Key { keycode: u16, command: bool, shift: bool, option: bool, control: bool },
}

/// `text` as a [`Combo`]: "cmd+l", "shift+tab", "return", "/".
fn parse(text: &str) -> Result<Combo, MacosError> {
    let lowered = text.trim().to_lowercase();
    let mut chars = lowered.chars();
    if let (Some(only), None) = (chars.next(), chars.next()) {
        if !only.is_alphanumeric() && !only.is_whitespace() {
            return Ok(Combo::Character(only));
        }
    }
    let parts: Vec<&str> = lowered.split('+').map(str::trim).collect();
    let (key, modifiers) = parts.split_last().ok_or(MacosError::SynthesizeKeystrokeFailed)?;
    let keycode = KEYCODES
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, code)| *code)
        .ok_or(MacosError::SynthesizeKeystrokeFailed)?;
    let mut combo = Combo::Key { keycode, command: false, shift: false, option: false, control: false };
    for modifier in modifiers {
        if let Combo::Key { command, shift, option, control, .. } = &mut combo {
            match *modifier {
                "cmd" | "command" | "⌘" => *command = true,
                "shift" | "⇧" => *shift = true,
                "alt" | "option" | "opt" | "⌥" => *option = true,
                "ctrl" | "control" | "⌃" => *control = true,
                _ => return Err(MacosError::SynthesizeKeystrokeFailed),
            }
        }
    }
    Ok(combo)
}

/// Presses `combo` — down, then up — to whatever app has the keyboard.
///
/// # Errors
/// [`MacosError::SynthesizeKeystrokeFailed`] if the shortcut is not one
/// this understands or macOS refuses the event.
pub fn press_combo(combo: &str) -> Result<(), MacosError> {
    use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};

    let combo = parse(combo)?;
    let source =
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok_or(MacosError::SynthesizeKeystrokeFailed)?;
    let (keycode, flags, character) = match &combo {
        // Key 0 with the character attached: the receiving page reads the character.
        Combo::Character(c) => (0, CGEventFlags::empty(), Some(*c)),
        Combo::Key { keycode, command, shift, option, control } => {
            let mut flags = CGEventFlags::empty();
            for (held, mask) in [
                (*command, CGEventFlags::MaskCommand),
                (*shift, CGEventFlags::MaskShift),
                (*option, CGEventFlags::MaskAlternate),
                (*control, CGEventFlags::MaskControl),
            ] {
                if held {
                    flags |= mask;
                }
            }
            (*keycode, flags, None)
        }
    };
    for down in [true, false] {
        let event =
            CGEvent::new_keyboard_event(Some(&source), keycode, down).ok_or(MacosError::SynthesizeKeystrokeFailed)?;
        CGEvent::set_flags(Some(&event), flags);
        if let Some(character) = character {
            let mut buffer = [0u16; 2];
            let units = character.encode_utf16(&mut buffer);
            // SAFETY: `units` is a live buffer of exactly `units.len()` UTF-16 units.
            unsafe { CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as u64, units.as_ptr()) };
        }
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn a_lone_symbol_is_a_character_and_not_a_key_position() {
        assert_eq!(parse("/").unwrap(), Combo::Character('/'));
        assert_eq!(parse(" / ").unwrap(), Combo::Character('/'));
    }

    #[test]
    fn a_shortcut_has_its_modifiers_and_its_key() {
        assert_eq!(
            parse("cmd+l").unwrap(),
            Combo::Key { keycode: 0x25, command: true, shift: false, option: false, control: false }
        );
        assert_eq!(
            parse("Shift + Tab").unwrap(),
            Combo::Key { keycode: 0x30, command: false, shift: true, option: false, control: false }
        );
        assert_eq!(
            parse("return").unwrap(),
            Combo::Key { keycode: 0x24, command: false, shift: false, option: false, control: false }
        );
    }

    #[test]
    fn what_it_does_not_understand_is_an_error_not_a_guess() {
        for bad in ["", "cmd+", "hyper+l", "cmd+ñ", "f13", "cmd++"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
