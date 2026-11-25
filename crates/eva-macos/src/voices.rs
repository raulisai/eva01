//! Which voice EVA speaks with. macOS ships compact Spanish voices that sound
//! robotic (Mónica, Paulina) and offers far more natural "Premium" and
//! "Enhanced" ones as a free download; `"auto"` picks the best one installed,
//! so getting a better voice is only a matter of downloading it.

/// One voice `say -v '?'` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// The name to pass to `say -v`.
    pub name: String,
    /// Its locale, e.g. `es_MX`.
    pub locale: String,
}

/// The voices in `listing` (the output of `say -v '?'`): each line is the
/// name, spaces, the locale, then `# a sample sentence`.
pub fn parse_listing(listing: &str) -> Vec<Voice> {
    listing
        .lines()
        .filter_map(|line| {
            let before_sample = line.split('#').next()?.trim_end();
            let (name, locale) = before_sample.rsplit_once(char::is_whitespace)?;
            let locale = locale.trim();
            let name = name.trim();
            (!name.is_empty() && locale.contains('_'))
                .then(|| Voice { name: name.to_string(), locale: locale.to_string() })
        })
        .collect()
}

/// Novelty voices macOS lists under every language: never a real choice.
const NOVELTY: &[&str] = &["Eddy", "Flo", "Grandma", "Grandpa", "Reed", "Rocko", "Sandy", "Shelley"];

/// How natural `voice` sounds, for picking: Premium over Enhanced over the
/// compact default, Mexican Spanish over other Spanish; `None` if it is not a
/// Spanish voice worth speaking with.
fn rank(voice: &Voice) -> Option<(u8, u8)> {
    if !voice.locale.starts_with("es") || NOVELTY.iter().any(|n| voice.name.starts_with(n)) {
        return None;
    }
    let name = voice.name.to_lowercase();
    let quality = if name.contains("premium") {
        3
    } else if name.contains("enhanced") || name.contains("mejorada") {
        2
    } else {
        1
    };
    let region = match voice.locale.as_str() {
        "es_MX" => 3,
        "es_US" | "es_419" => 2,
        _ => 1,
    };
    Some((quality, region))
}

/// The most natural Spanish voice among `voices`.
pub fn best_spanish(voices: &[Voice]) -> Option<&Voice> {
    voices.iter().filter_map(|v| rank(v).map(|r| (r, v))).max_by_key(|(r, _)| *r).map(|(_, v)| v)
}

/// Whether `voices` has a Spanish voice better than the compact defaults.
pub fn has_natural_spanish(voices: &[Voice]) -> bool {
    voices.iter().filter_map(rank).any(|(quality, _)| quality >= 2)
}

/// The voices installed on this Mac.
pub fn installed() -> Vec<Voice> {
    std::process::Command::new("say")
        .args(["-v", "?"])
        .output()
        .map(|output| parse_listing(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

/// The voice to speak with for the configured `wanted`: itself, unless it is
/// `"auto"` (or empty), which is the best Spanish voice installed — and
/// Paulina, always present on a Mac, if none could be read.
pub fn resolve(wanted: &str) -> String {
    let wanted = wanted.trim();
    if !wanted.is_empty() && !wanted.eq_ignore_ascii_case("auto") {
        return wanted.to_string();
    }
    best_spanish(&installed()).map_or_else(|| "Paulina".to_string(), |v| v.name.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    /// The listing of this Mac, trimmed, plus the voices a user can download.
    const LISTING: &str = "Eddy (Spanish (Mexico)) es_MX    # ¡Hola! Me llamo Eddy.
Mónica              es_ES    # ¡Hola! Me llamo Mónica.
Paulina             es_MX    # ¡Hola! Me llamo Paulina.
Samantha            en_US    # Hello! My name is Samantha.
Grandma (Spanish (Mexico)) es_MX    # ¡Hola! Me llamo Grandma.";

    #[test]
    fn the_listing_is_read_names_with_spaces_and_parentheses_included() {
        let voices = parse_listing(LISTING);
        assert_eq!(voices.len(), 5);
        assert_eq!(voices[0], Voice { name: "Eddy (Spanish (Mexico))".into(), locale: "es_MX".into() });
        assert_eq!(voices[2], Voice { name: "Paulina".into(), locale: "es_MX".into() });
    }

    #[test]
    fn with_only_compact_voices_the_mexican_one_wins_and_novelty_voices_never_do() {
        let voices = parse_listing(LISTING);
        assert_eq!(best_spanish(&voices).unwrap().name, "Paulina");
        assert!(!has_natural_spanish(&voices));
    }

    #[test]
    fn a_downloaded_premium_voice_beats_everything_else() {
        let listing =
            format!("{LISTING}\nMónica (Premium)    es_ES    # ¡Hola!\nPaulina (Enhanced)  es_MX    # ¡Hola!");
        let voices = parse_listing(&listing);
        assert_eq!(best_spanish(&voices).unwrap().name, "Mónica (Premium)");
        assert!(has_natural_spanish(&voices));
    }

    #[test]
    fn a_named_voice_is_kept_as_is() {
        assert_eq!(resolve("Jorge"), "Jorge");
    }
}
