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
#[derive(Debug, Clone)]
pub struct AppIndex {
    apps: Vec<AppEntry>,
}

/// The Spanish names people say for Apple's own apps, whose files keep their
/// English names whatever language macOS is in ("abre la calculadora" must
/// find `Calculator.app`). Folded: no accents, lowercase.
const SPANISH_NAMES: &[(&str, &[&str])] = &[
    ("Notes", &["notas"]),
    ("Calendar", &["calendario", "agenda"]),
    ("Calculator", &["calculadora"]),
    (
        "System Settings",
        &["ajustes", "ajustes del sistema", "configuracion", "configuracion del sistema", "preferencias del sistema"],
    ),
    ("Mail", &["correo", "correo electronico"]),
    ("Messages", &["mensajes"]),
    ("Photos", &["fotos"]),
    ("Music", &["musica"]),
    ("Maps", &["mapas"]),
    ("Reminders", &["recordatorios"]),
    ("Contacts", &["contactos"]),
    ("Preview", &["vista previa"]),
    ("FindMy", &["encontrar", "find my", "buscar mi"]),
    ("Books", &["libros"]),
    ("News", &["noticias"]),
    ("Stocks", &["bolsa"]),
    ("Weather", &["tiempo", "clima"]),
    ("Clock", &["reloj"]),
    ("Voice Memos", &["notas de voz"]),
    ("Screenshot", &["captura de pantalla"]),
    ("Activity Monitor", &["monitor de actividad"]),
    ("Disk Utility", &["utilidad de discos"]),
    ("TextEdit", &["editor de texto", "editor de textos"]),
    ("Dictionary", &["diccionario"]),
    ("Chess", &["ajedrez"]),
    ("Shortcuts", &["atajos"]),
    ("Home", &["casa"]),
    ("Passwords", &["contrasenas"]),
    ("Keychain Access", &["llaveros", "acceso a llaveros"]),
    ("Console", &["consola"]),
    ("Script Editor", &["editor de scripts"]),
    ("Journal", &["diario"]),
    ("Tips", &["consejos"]),
];

/// What people call some popular apps instead of their full names. Applies to
/// the app and its variants: "Visual Studio Code - Insiders" is "code" too.
/// Without this, "abre code" opened "Claude Code URL Handler" (the word
/// "code" is in both names).
const NICKNAMES: &[(&str, &[&str])] = &[
    ("Visual Studio Code", &["code", "vscode", "vs code", "visual studio"]),
    ("Google Chrome", &["chrome"]),
    ("Microsoft Word", &["word"]),
    ("Microsoft Excel", &["excel"]),
    ("Microsoft PowerPoint", &["powerpoint"]),
    ("Microsoft Outlook", &["outlook"]),
    ("Microsoft Teams", &["teams"]),
    ("zoom.us", &["zoom"]),
    ("iTerm", &["iterm", "i term"]),
    ("IntelliJ IDEA", &["intellij"]),
];

/// Words that say nothing about which app is meant ("Final Cut Pro", "Zoom.us").
const GENERIC_WORDS: &[&str] = &["app", "the", "for", "de", "pro", "us", "mac", "macos", "for mac", "desktop", "beta"];

/// How good a match is, best first; within a tier the higher score wins.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
enum Tier {
    /// A misheard name: close in spelling, same first letter.
    Fuzzy,
    /// The query is a distinctive word (or run of words) of the name:
    /// "chrome" in "Google Chrome", "code" in "Visual Studio Code".
    Word,
    /// The whole name or one of its aliases.
    Exact,
}

/// Fuzzy matches need both measures: Jaro-Winkler alone rated "chrome" as
/// close to "home" (0.89) as a real mishearing like "gosty" to "ghostty"
/// (0.91) — found when "abre chrome" opened the Home app.
const FUZZY_JARO_WINKLER: f64 = 0.88;
const FUZZY_LEVENSHTEIN: f64 = 0.70;

impl AppIndex {
    /// Builds an index from a list of applications, adding the Spanish
    /// names of Apple's own apps.
    pub fn new(apps: Vec<AppEntry>) -> Self {
        let apps = apps
            .into_iter()
            .map(|app| {
                let spanish = SPANISH_NAMES.iter().filter(|(name, _)| *name == app.canonical_name);
                let nicknames = NICKNAMES.iter().filter(|(name, _)| is_variant_of(&app.canonical_name, name));
                let extra: Vec<&str> = spanish.chain(nicknames).flat_map(|(_, names)| names.iter().copied()).collect();
                app.with_aliases(extra)
            })
            .collect();
        AppIndex { apps }
    }

    /// An index of `names` (the apps installed on this Mac) where "el
    /// navegador" and "el correo" mean whatever `default_app_for` says opens
    /// links (`https://…`) and mail (`mailto:…`) — passed in, so this crate
    /// stays free of macOS.
    pub fn for_this_mac(names: Vec<String>, default_app_for: impl Fn(&str) -> Option<String>) -> Self {
        let mut index = AppIndex::new(names.into_iter().map(AppEntry::new).collect());
        for (url, aliases) in [
            ("https://example.com", &["navegador", "el navegador", "browser", "internet"][..]),
            ("mailto:alguien@example.com", &["correo", "correo electronico", "email"][..]),
        ] {
            if let Some(app) = default_app_for(url) {
                for alias in aliases {
                    index = index.with_exclusive_alias(&app, alias);
                }
            }
        }
        index
    }

    /// The canonical names of every app in the index, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.apps.iter().map(|a| a.canonical_name.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Every way `canonical_name` can be said (its name and aliases).
    pub fn spoken_names(&self, canonical_name: &str) -> Vec<&str> {
        self.apps
            .iter()
            .find(|a| a.canonical_name == canonical_name)
            .map(|a| a.match_keys().collect())
            .unwrap_or_default()
    }

    /// Makes `alias` mean `canonical_name` and no other app — "navegador"
    /// for whichever browser is the default, "correo" for the mail app.
    #[must_use]
    pub fn with_exclusive_alias(mut self, canonical_name: &str, alias: &str) -> Self {
        let folded = fold_diacritics(alias);
        for app in &mut self.apps {
            app.aliases.retain(|existing| fold_diacritics(existing) != folded);
            if app.canonical_name == canonical_name {
                app.aliases.push(alias.to_string());
            }
        }
        self
    }

    /// Returns `true` if the index has no applications.
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Finds the application `query` names, accent- and case-insensitively:
    /// by its whole name or an alias, else by a distinctive word of its name,
    /// else — only for a near miss that starts with the same letter — by
    /// spelling. `None` rather than a guess: a wrong app opening is a worse
    /// outcome than "didn't understand".
    pub fn find(&self, query: &str) -> Option<&AppEntry> {
        self.find_guess(query).map(|(app, _)| app)
    }

    /// Like [`AppIndex::find`], also saying whether the match is only a guess
    /// from spelling (a misheard name) — the case worth asking about.
    pub fn find_guess(&self, query: &str) -> Option<(&AppEntry, bool)> {
        let query = words(query);
        if query.is_empty() {
            return None;
        }
        self.apps
            .iter()
            .filter_map(|app| {
                let (tier, score) = app.match_keys().filter_map(|key| rank(&query, &words(key))).max_by(compare)?;
                Some((app, tier, score))
            })
            .max_by(|a, b| {
                compare(&(a.1, a.2), &(b.1, b.2))
                    // Equally good: the shorter name is the more exact one
                    // ("Visual Studio Code" over "…Code - Insiders").
                    .then_with(|| b.0.canonical_name.len().cmp(&a.0.canonical_name.len()))
            })
            .map(|(app, tier, _)| (app, tier == Tier::Fuzzy))
    }

    /// Makes `heard` an exact name of `canonical_name` from now on — how the
    /// user says it, confirmed. `false` if no such app is installed.
    pub fn teach(&mut self, heard: &str, canonical_name: &str) -> bool {
        let Some(app) = self.apps.iter_mut().find(|a| a.canonical_name == canonical_name) else { return false };
        if !app.match_keys().any(|key| words(key) == words(heard)) {
            app.aliases.push(heard.to_string());
        }
        true
    }
}

/// Whether `name` is `base` or a variant of it ("Visual Studio Code - Insiders",
/// "iTerm2"), compared without case.
fn is_variant_of(name: &str, base: &str) -> bool {
    let (name, base) = (name.to_lowercase(), base.to_lowercase());
    name == base || name.strip_prefix(&base).is_some_and(|rest| rest.starts_with([' ', '-', '2', '3']))
}

/// Folded words: no accents, lowercase, split on anything not alphanumeric.
fn words(text: &str) -> Vec<String> {
    fold_diacritics(text).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn compare(a: &(Tier, f64), b: &(Tier, f64)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.total_cmp(&b.1))
}

/// How `query` matches the name `key` (both already split into words).
fn rank(query: &[String], key: &[String]) -> Option<(Tier, f64)> {
    if query == key {
        return Some((Tier::Exact, 1.0));
    }
    let distinctive = query.iter().any(|w| w.len() >= 3 && !GENERIC_WORDS.contains(&w.as_str()));
    let contained = key.windows(query.len()).any(|run| run == query);
    if distinctive && contained {
        // A larger share of the name said is a better match.
        return Some((Tier::Word, query.len() as f64 / key.len() as f64));
    }
    let (said, name) = (query.join(" "), key.join(" "));
    let same_start = said.chars().next() == name.chars().next();
    let jaro_winkler = strsim::jaro_winkler(&said, &name);
    let close = same_start
        && jaro_winkler >= FUZZY_JARO_WINKLER
        && strsim::normalized_levenshtein(&said, &name) >= FUZZY_LEVENSHTEIN;
    close.then_some((Tier::Fuzzy, jaro_winkler))
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

    /// The apps of the Mac this was measured on, where "abre chrome" opened
    /// Home and "abre finder" opened Find My.
    fn real_index() -> AppIndex {
        AppIndex::new(
            [
                "Google Chrome",
                "Home",
                "Brave Browser",
                "FindMy",
                "Finder",
                "Visual Studio Code - Insiders",
                "Notes",
                "Calendar",
                "Calculator",
                "System Settings",
                "Mail",
                "Photos",
                "Zoom.us",
                "Microsoft Teams",
                "Terminal",
            ]
            .into_iter()
            .map(AppEntry::new)
            .collect(),
        )
    }

    fn found(index: &AppIndex, query: &str) -> Option<String> {
        index.find(query).map(|a| a.canonical_name.clone())
    }

    #[test]
    fn a_distinctive_word_of_the_name_beats_a_lookalike() {
        let index = real_index();
        assert_eq!(found(&index, "chrome").as_deref(), Some("Google Chrome"), "not Home");
        assert_eq!(found(&index, "finder").as_deref(), Some("Finder"), "not Find My");
        assert_eq!(found(&index, "visual studio code").as_deref(), Some("Visual Studio Code - Insiders"));
        assert_eq!(found(&index, "zoom").as_deref(), Some("Zoom.us"));
        assert_eq!(found(&index, "teams").as_deref(), Some("Microsoft Teams"));
    }

    #[test]
    fn nicknames_reach_the_app_and_its_variants() {
        let index = AppIndex::new(vec![
            AppEntry::new("Claude Code URL Handler"),
            AppEntry::new("Visual Studio Code - Insiders"),
            AppEntry::new("iTerm2"),
        ]);
        assert_eq!(found(&index, "code").as_deref(), Some("Visual Studio Code - Insiders"), "not the URL handler");
        assert_eq!(found(&index, "vscode").as_deref(), Some("Visual Studio Code - Insiders"));
        assert_eq!(found(&index, "iterm").as_deref(), Some("iTerm2"));
        assert!(!is_variant_of("Codex Helper", "Code"), "a longer word is not a variant");
    }

    #[test]
    fn a_lookalike_with_a_different_first_letter_is_not_a_mishearing() {
        let index = AppIndex::new(vec![AppEntry::new("Home")]);
        assert_eq!(found(&index, "chrome"), None, "better not to understand than to open the wrong app");
    }

    #[test]
    fn apples_apps_answer_to_their_spanish_names() {
        let index = real_index();
        for (said, app) in [
            ("notas", "Notes"),
            ("calendario", "Calendar"),
            ("calculadora", "Calculator"),
            ("ajustes", "System Settings"),
            ("configuración del sistema", "System Settings"),
            ("correo", "Mail"),
            ("fotos", "Photos"),
        ] {
            assert_eq!(found(&index, said).as_deref(), Some(app), "{said}");
        }
    }

    #[test]
    fn an_exclusive_alias_moves_to_the_app_it_names() {
        let index = real_index().with_exclusive_alias("Google Chrome", "navegador");
        assert_eq!(found(&index, "navegador").as_deref(), Some("Google Chrome"));
        let index = index.with_exclusive_alias("Brave Browser", "navegador");
        assert_eq!(found(&index, "navegador").as_deref(), Some("Brave Browser"), "only one app answers to it");
        let index = real_index().with_exclusive_alias("Microsoft Teams", "correo");
        assert_eq!(found(&index, "correo").as_deref(), Some("Microsoft Teams"), "taken from Mail");
    }

    proptest::proptest! {
        #[test]
        fn find_never_panics_on_arbitrary_queries(query in ".*") {
            let index = sample_index();
            let _ = index.find(&query);
        }
    }
}
