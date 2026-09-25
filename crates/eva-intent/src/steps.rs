//! The steps a command is made of, as one line each — the format of a
//! `[[commands]]` `open` list, of what the panel's wizard writes, and of what
//! the local model plans:
//!
//! | line                          | does                                              |
//! |-------------------------------|---------------------------------------------------|
//! | `Brave Browser`, `github.com` | opens an app or an address                        |
//! | `buscar: clima de hoy`        | searches the web                                  |
//! | `youtube: música chill`       | plays the first YouTube video of the search       |
//! | `reproducir: spotify:playlist:…` | plays a Spotify link or an Apple Music playlist |
//! | `reproducir:`                 | resumes the music                                 |
//! | `pausar:`, `siguiente:`, `anterior:` | pause, next track, previous track         |
//!
//! A line with no known verb is something to open, so every list written before
//! the verbs existed still means what it did.

use eva_text::fold_diacritics;

/// One step of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Open an app or a web address.
    Open(String),
    /// Search the web.
    Search(String),
    /// Play the first YouTube video of this search.
    Youtube(String),
    /// Play a Spotify link or an Apple Music playlist; `None` resumes.
    Play(Option<String>),
    /// Pause the music.
    Pause,
    /// The next track.
    Next,
    /// The previous track.
    Previous,
}

impl Step {
    /// The step a line writes down. Never fails: what has no verb (or a verb
    /// without what it needs, like `buscar:`) is something to open, which the
    /// caller then finds it cannot.
    pub fn parse(line: &str) -> Step {
        let line = line.trim();
        let Some((verb, rest)) = line.split_once(':') else { return Step::Open(line.to_string()) };
        let rest = rest.trim();
        match (fold_diacritics(verb.trim()).to_lowercase().as_str(), rest.is_empty()) {
            ("buscar", false) => Step::Search(rest.to_string()),
            ("youtube", false) => Step::Youtube(rest.to_string()),
            ("reproducir", empty) => Step::Play((!empty).then(|| rest.to_string())),
            ("pausar", _) => Step::Pause,
            ("siguiente", _) => Step::Next,
            ("anterior", _) => Step::Previous,
            _ => Step::Open(line.to_string()),
        }
    }

    /// The line that writes this step down: `Step::parse(&step.line())` is the step.
    pub fn line(&self) -> String {
        match self {
            Step::Open(what) => what.clone(),
            Step::Search(query) => format!("buscar: {query}"),
            Step::Youtube(query) => format!("youtube: {query}"),
            Step::Play(Some(target)) => format!("reproducir: {target}"),
            Step::Play(None) => "reproducir:".to_string(),
            Step::Pause => "pausar:".to_string(),
            Step::Next => "siguiente:".to_string(),
            Step::Previous => "anterior:".to_string(),
        }
    }

    /// What it does, in a few words for the user ("Abrir Brave").
    pub fn describe(&self) -> String {
        match self {
            Step::Open(what) => format!("Abrir {what}"),
            Step::Search(query) => format!("Buscar «{query}» en la web"),
            Step::Youtube(query) => format!("Poner «{query}» en YouTube"),
            Step::Play(Some(target)) => format!("Reproducir {target}"),
            Step::Play(None) => "Reanudar la música".to_string(),
            Step::Pause => "Pausar la música".to_string(),
            Step::Next => "Siguiente canción".to_string(),
            Step::Previous => "Canción anterior".to_string(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn every_verb_reads_back_the_same() {
        for step in [
            Step::Open("Brave Browser".into()),
            Step::Open("https://ejemplo.com/a".into()),
            Step::Search("clima de hoy".into()),
            Step::Youtube("música chill".into()),
            Step::Play(Some("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M".into())),
            Step::Play(None),
            Step::Pause,
            Step::Next,
            Step::Previous,
        ] {
            assert_eq!(Step::parse(&step.line()), step, "{step:?}");
        }
    }

    #[test]
    fn verbs_ignore_case_accents_and_spaces_and_a_link_is_not_a_verb() {
        assert_eq!(Step::parse("  Buscar :  gatos "), Step::Search("gatos".into()));
        assert_eq!(Step::parse("YouTube: lofi"), Step::Youtube("lofi".into()));
        assert_eq!(
            Step::parse("spotify:playlist:abc"),
            Step::Open("spotify:playlist:abc".into()),
            "only a link, to open"
        );
        assert_eq!(Step::parse("https://youtube.com"), Step::Open("https://youtube.com".into()));
    }

    #[test]
    fn a_verb_without_what_it_needs_is_left_as_something_to_open() {
        assert_eq!(Step::parse("buscar:"), Step::Open("buscar:".into()));
        assert_eq!(Step::parse("youtube:  "), Step::Open("youtube:".into()));
    }
}
