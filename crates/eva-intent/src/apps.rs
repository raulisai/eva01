//! A fuzzy-matchable index of installed applications and their spoken
//! aliases ("code" → Visual Studio Code, "brave" → Brave Browser).
//!
//! Scanning `/Applications` is a macOS concern and lives in `eva-macos`
//! (per `docs/ENGINEERING.md` #5: OS-specific work stays behind a boundary
//! this crate doesn't cross). `AppIndex` only knows how to match a spoken
//! name against a list it is given, so it is fully testable without a real
//! filesystem or a real Mac.

use eva_text::fold_diacritics;

/// One installed application, as far as the intent parser needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// The application's real name, exactly as the OS shows it (e.g. `"Visual Studio Code"`).
    pub canonical_name: String,
    /// Extra spoken forms that should also resolve to this app (e.g. `["code", "vscode"]`).
    pub aliases: Vec<String>,
}

impl AppEntry {
    /// Builds an entry with no extra aliases beyond its canonical name.
    pub fn new(canonical_name: impl Into<String>) -> Self {
        AppEntry { canonical_name: canonical_name.into(), aliases: Vec::new() }
    }

    /// Adds spoken aliases, builder-style.
    #[must_use]
    pub fn with_aliases<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.aliases.extend(aliases.into_iter().map(Into::into));
        self
    }

    /// All the names this entry can be matched by: its canonical name plus every alias.
    fn match_keys(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.canonical_name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// A queryable set of applications.
pub struct AppIndex {
    apps: Vec<AppEntry>,
}

/// Below this similarity, [`AppIndex::find`] reports no match rather than
/// guessing — a wrong app opening is a worse outcome than "didn't understand".
const MATCH_THRESHOLD: f64 = 0.80;

impl AppIndex {
    /// Builds an index from a list of applications.
    pub fn new(apps: Vec<AppEntry>) -> Self {
        AppIndex { apps }
    }

    /// Returns `true` if the index has no applications.
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Finds the application whose canonical name or an alias best matches
    /// `query`, accent- and case-insensitively. Returns `None` if nothing
    /// clears `MATCH_THRESHOLD`.
    pub fn find(&self, query: &str) -> Option<&AppEntry> {
        let folded_query = fold_diacritics(query);
        if folded_query.is_empty() {
            return None;
        }

        self.apps
            .iter()
            .filter_map(|app| {
                let best_key_score = app
                    .match_keys()
                    .map(|key| strsim::jaro_winkler(&folded_query, &fold_diacritics(key)))
                    .fold(0.0_f64, f64::max);
                (best_key_score >= MATCH_THRESHOLD).then_some((app, best_key_score))
            })
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(app, _)| app)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn sample_index() -> AppIndex {
        AppIndex::new(vec![
            AppEntry::new("Visual Studio Code").with_aliases(["code", "vscode"]),
            AppEntry::new("Brave Browser").with_aliases(["brave"]),
            AppEntry::new("Ghostty"),
        ])
    }

    #[test]
    fn matches_by_canonical_name() {
        let index = sample_index();
        assert_eq!(index.find("Ghostty").map(|a| a.canonical_name.as_str()), Some("Ghostty"));
    }

    #[test]
    fn matches_by_alias() {
        let index = sample_index();
        assert_eq!(index.find("code").map(|a| a.canonical_name.as_str()), Some("Visual Studio Code"));
        assert_eq!(index.find("brave").map(|a| a.canonical_name.as_str()), Some("Brave Browser"));
    }

    #[test]
    fn matches_are_accent_and_case_insensitive() {
        let index = sample_index();
        assert_eq!(index.find("BRAVE").map(|a| a.canonical_name.as_str()), Some("Brave Browser"));
    }

    #[test]
    fn tolerates_a_small_mispronunciation() {
        let index = sample_index();
        // "gosty" is close enough to "Ghostty" to clear the threshold.
        assert_eq!(index.find("gosty").map(|a| a.canonical_name.as_str()), Some("Ghostty"));
    }

    #[test]
    fn returns_none_for_something_unrelated() {
        let index = sample_index();
        assert_eq!(index.find("una cosa completamente distinta"), None);
    }

    #[test]
    fn empty_query_never_matches() {
        let index = sample_index();
        assert_eq!(index.find(""), None);
    }

    #[test]
    fn empty_index_never_matches_anything() {
        let index = AppIndex::new(Vec::new());
        assert!(index.is_empty());
        assert_eq!(index.find("Brave"), None);
    }

    proptest::proptest! {
        #[test]
        fn find_never_panics_on_arbitrary_queries(query in ".*") {
            let index = sample_index();
            let _ = index.find(&query);
        }
    }
}
