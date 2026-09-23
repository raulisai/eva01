//! Turning the config's key names into something to register, and into the
//! symbols the overlay shows ("⌘⏎ sí · ⌘⎋ no").

use global_hotkey::hotkey::HotKey;
use std::str::FromStr;

/// The dictation key: the `fn` / 🌐 key held down, or a key combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationKey {
    /// Hold the `fn` / 🌐 key (`eva_macos::FnKeyMonitor`).
    Fn,
    /// Hold a combination (`global-hotkey`).
    Combo(HotKey),
}

/// Parses `hotkey.dictation` from the config.
///
/// # Errors
/// A message naming the setting and what was wrong with it.
pub fn parse_dictation(spec: &str) -> Result<DictationKey, String> {
    if is_fn(spec) {
        return Ok(DictationKey::Fn);
    }
    parse_combo(spec).map(DictationKey::Combo).map_err(|e| format!("hotkey.dictation: {e}"))
}

/// Whether `spec` names the `fn` / 🌐 key.
pub fn is_fn(spec: &str) -> bool {
    matches!(spec.trim().to_lowercase().as_str(), "fn" | "globe" | "🌐")
}

/// Parses a key combination such as `cmd+shift+space` or `cmd+return`.
/// `global-hotkey` spells Return as `enter`; people write both.
///
/// # Errors
/// A message saying which key was not understood.
pub fn parse_combo(spec: &str) -> Result<HotKey, String> {
    let normalized: Vec<String> = spec
        .split('+')
        .map(|part| match part.trim().to_lowercase().as_str() {
            "return" => "enter".to_string(),
            other => other.to_string(),
        })
        .collect();
    HotKey::from_str(&normalized.join("+")).map_err(|e| format!("no entiendo la combinación «{spec}» ({e})"))
}

/// The symbols for `spec`: `cmd+shift+space` → `⌘⇧Space`, `cmd+return` →
/// `⌘⏎`. What is not a modifier or a special key is shown as typed.
pub fn label(spec: &str) -> String {
    if is_fn(spec) {
        return "fn".to_string();
    }
    spec.split('+')
        .map(|part| match part.trim().to_lowercase().as_str() {
            "cmd" | "command" | "super" | "meta" => "⌘".to_string(),
            "shift" => "⇧".to_string(),
            "alt" | "option" | "opt" => "⌥".to_string(),
            "ctrl" | "control" => "⌃".to_string(),
            "return" | "enter" => "⏎".to_string(),
            "escape" | "esc" => "⎋".to_string(),
            "space" => "Space".to_string(),
            "tab" => "⇥".to_string(),
            other => {
                let mut chars = other.chars();
                chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn fn_and_globe_are_the_function_key() {
        for spec in ["fn", "FN", " globe ", "🌐"] {
            assert_eq!(parse_dictation(spec), Ok(DictationKey::Fn), "{spec}");
        }
    }

    #[test]
    fn combinations_parse_in_the_ways_people_write_them() {
        for spec in
            ["cmd+shift+space", "Cmd+Shift+Space", "super+shift+space", "alt+space", "option+space", "ctrl+alt+d"]
        {
            assert!(matches!(parse_dictation(spec), Ok(DictationKey::Combo(_))), "{spec}");
        }
    }

    #[test]
    fn return_is_accepted_even_though_the_library_calls_it_enter() {
        assert!(parse_combo("cmd+return").is_ok());
        assert_eq!(parse_combo("cmd+return").unwrap(), parse_combo("cmd+enter").unwrap());
    }

    #[test]
    fn nonsense_is_an_error_that_names_the_setting() {
        let error = parse_dictation("cmd+bananas").expect_err("must not parse");
        assert!(error.starts_with("hotkey.dictation:"), "{error}");
        assert!(error.contains("cmd+bananas"));
    }

    #[test]
    fn labels_use_the_symbols_macos_users_know() {
        assert_eq!(label("cmd+shift+space"), "⌘⇧Space");
        assert_eq!(label("cmd+return"), "⌘⏎");
        assert_eq!(label("cmd+escape"), "⌘⎋");
        assert_eq!(label("ctrl+alt+d"), "⌃⌥D");
        assert_eq!(label("fn"), "fn");
    }
}
