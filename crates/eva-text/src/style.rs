//! Per-app formatting styles (`docs/PLAN.md` fase 9): the same dictation is
//! punctuated differently in a chat, an email and a terminal. A [`Style`]
//! changes conventions only — capitalization and closing punctuation — never
//! the words, which is what keeps the on-device model's hallucination guard
//! (`crate::apple_intelligence`) valid for every style. They are applied by
//! deterministic rules after the model has run, never described to the model
//! itself — a style hint in its prompt made it answer the dictation like a
//! chatbot instead of formatting it.

/// How the text should be presented in the app it is going into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Ordinary prose: capitalized, closed with punctuation.
    #[default]
    Default,
    /// Chat (Slack, Messages, WhatsApp, Discord…): capitalized, but a lone
    /// sentence is not closed with a period.
    Casual,
    /// Email and documents: complete, punctuated sentences.
    Formal,
    /// A terminal or command line: words only — no capital, no punctuation.
    Terminal,
}

impl Style {
    /// Parses the names used in the config file.
    pub fn from_name(name: &str) -> Option<Style> {
        match name.trim().to_lowercase().as_str() {
            "default" => Some(Style::Default),
            "casual" => Some(Style::Casual),
            "formal" => Some(Style::Formal),
            "terminal" => Some(Style::Terminal),
            _ => None,
        }
    }

    /// The name used in the config file.
    pub fn name(self) -> &'static str {
        match self {
            Style::Default => "default",
            Style::Casual => "casual",
            Style::Formal => "formal",
            Style::Terminal => "terminal",
        }
    }

    /// The style for the app with this bundle identifier: the user's own
    /// `overrides` first, then the built-in table, then [`Style::Default`].
    pub fn for_bundle_id(bundle_id: Option<&str>, overrides: &[(String, Style)]) -> Style {
        let Some(bundle_id) = bundle_id else { return Style::Default };
        if let Some((_, style)) = overrides.iter().find(|(id, _)| id.eq_ignore_ascii_case(bundle_id)) {
            return *style;
        }
        BUILT_IN.iter().find(|(id, _)| id.eq_ignore_ascii_case(bundle_id)).map_or(Style::Default, |(_, style)| *style)
    }
}

/// Apps whose conventions are unambiguous enough to style by default.
const BUILT_IN: &[(&str, Style)] = &[
    // Chat.
    ("com.tinyspeck.slackmacgap", Style::Casual),
    ("com.apple.MobileSMS", Style::Casual),
    ("net.whatsapp.WhatsApp", Style::Casual),
    ("com.hnc.Discord", Style::Casual),
    ("ru.keepcoder.Telegram", Style::Casual),
    ("org.telegram.desktop", Style::Casual),
    ("com.microsoft.teams2", Style::Casual),
    // Email and documents.
    ("com.apple.mail", Style::Formal),
    ("com.microsoft.Outlook", Style::Formal),
    ("com.readdle.smartemail-Mac", Style::Formal),
    ("com.apple.iWork.Pages", Style::Formal),
    ("com.microsoft.Word", Style::Formal),
    // Terminals.
    ("com.apple.Terminal", Style::Terminal),
    ("com.googlecode.iterm2", Style::Terminal),
    ("com.mitchellh.ghostty", Style::Terminal),
    ("dev.warp.Warp-Stable", Style::Terminal),
    ("net.kovidgoyal.kitty", Style::Terminal),
    ("com.github.wez.wezterm", Style::Terminal),
    ("io.alacritty", Style::Terminal),
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_built_in_table_covers_the_three_cases_the_plan_names() {
        assert_eq!(Style::for_bundle_id(Some("com.tinyspeck.slackmacgap"), &[]), Style::Casual);
        assert_eq!(Style::for_bundle_id(Some("com.apple.mail"), &[]), Style::Formal);
        assert_eq!(Style::for_bundle_id(Some("com.mitchellh.ghostty"), &[]), Style::Terminal);
    }

    #[test]
    fn an_unknown_app_or_no_app_is_the_default_style() {
        assert_eq!(Style::for_bundle_id(Some("com.example.Unknown"), &[]), Style::Default);
        assert_eq!(Style::for_bundle_id(None, &[]), Style::Default);
    }

    #[test]
    fn the_users_overrides_win_over_the_built_in_table() {
        let overrides = vec![("com.apple.mail".to_string(), Style::Casual)];
        assert_eq!(Style::for_bundle_id(Some("com.apple.mail"), &overrides), Style::Casual);
    }

    #[test]
    fn bundle_ids_match_case_insensitively() {
        assert_eq!(Style::for_bundle_id(Some("COM.APPLE.MAIL"), &[]), Style::Formal);
    }

    #[test]
    fn names_round_trip() {
        for style in [Style::Default, Style::Casual, Style::Formal, Style::Terminal] {
            assert_eq!(Style::from_name(style.name()), Some(style));
        }
        assert_eq!(Style::from_name(" Casual "), Some(Style::Casual));
        assert_eq!(Style::from_name("pirata"), None);
    }

    #[test]
    fn no_bundle_id_appears_twice_in_the_built_in_table() {
        let mut seen = std::collections::HashSet::new();
        for (id, _) in BUILT_IN {
            assert!(seen.insert(id.to_lowercase()), "{id} is listed twice");
        }
    }
}
