//! What was just done, so the next thing said can lean on it: after "abre
//! YouTube", "ahora busca Naruto" means *search YouTube for Naruto*, and
//! "ciérralo" means close what was just opened. Nothing is inferred beyond
//! what the last command plainly was, and it is only ever consulted while the
//! conversation is open (the worker's few seconds after a command).

use crate::intent::Intent;
use crate::sites::{self, Site};
use eva_text::fold_diacritics;

/// The last thing done: the app and/or website it was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recall {
    app: Option<String>,
    site: Option<&'static Site>,
}

impl Recall {
    /// What `intent`, once done, leaves to lean on: opening an app or an
    /// address does; anything else (a search, a task, a dictation) does not.
    pub fn of(intent: &Intent) -> Option<Recall> {
        match intent {
            Intent::OpenApp { app } => Some(Recall { app: Some(app.clone()), site: sites::by_spoken_name(app) }),
            Intent::OpenUrl { url } => Some(Recall { app: None, site: sites::by_url(url) }),
            _ => None,
        }
    }

    /// Whether there is anything a follow-up could use.
    pub fn is_useful(&self) -> bool {
        self.app.is_some() || self.site.is_some()
    }

    /// "Busca `query`" where this was left: the site's own search, if it has
    /// one — and unless the person named somewhere else ("en Google").
    pub fn search(&self, query: &str) -> Option<Intent> {
        let site = self.site?;
        let query = names_another_place(query, site)?;
        let url = site.search_url(&query)?;
        // Where the site is open decides which shortcut reaches its search box.
        let focus = if self.app.is_some() { site.focus_app } else { site.focus_web };
        Some(match focus {
            Some(focus) => Intent::SearchInSite { site: site.name.to_string(), query, url, focus: focus.to_string() },
            None => Intent::OpenUrl { url },
        })
    }
}

/// `query` without a trailing "en <this site>", or `None` if it ends in "en"
/// and somewhere else (another site, Google, the web).
fn names_another_place(query: &str, site: &Site) -> Option<String> {
    let folded = fold_diacritics(query).to_lowercase();
    let Some(at) = folded.rfind(" en ") else { return Some(query.trim().to_string()) };
    let place = folded[at + 4..].trim_end_matches(|c: char| !c.is_alphanumeric()).to_string();
    if site.names.contains(&place.as_str()) {
        // Folding keeps one character per character, so `at` is the same in `query`.
        let cut = query.char_indices().nth(folded[..at].chars().count()).map_or(query.len(), |(i, _)| i);
        return Some(query[..cut].trim().to_string());
    }
    let elsewhere = sites::by_spoken_name(&place).is_some()
        || ["google", "internet", "la web", "el navegador", "un navegador", "chrome", "safari", "brave"]
            .contains(&place.as_str());
    (!elsewhere).then(|| query.trim().to_string())
}

/// Words that lead into a follow-up without being part of it.
const LEADS: &[&str] = &["ahora", "y", "luego", "despues", "entonces", "ok", "okay", "pues", "oye", "tambien", "por"];

/// Verbs that ask to look something up or play it.
const SEARCH_VERBS: &[&str] =
    &["busca", "buscar", "buscame", "busque", "pon", "ponme", "reproduce", "reproduceme", "muestrame", "encuentra"];

/// Whether `verb` is one of the search verbs, or one letter off "busca" — how
/// speech recognition writes it in a short phrase ("buscan", "buscas").
fn asks_to_search(verb: &str) -> bool {
    SEARCH_VERBS.contains(&verb) || (verb.starts_with('b') && strsim::levenshtein(verb, "busca") <= 1)
}

/// What `text` — said with no wake word, in the seconds after a command —
/// means given `recall`, if it is one of the few follow-ups that mean
/// something only there: a search where the last command left off, or
/// closing what it opened. `None` for anything else: it is dictation.
pub fn follow_up(text: &str, recall: &Recall) -> Option<Intent> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let folded = |word: &str| fold_diacritics(word.trim_matches(|c: char| !c.is_alphanumeric())).to_lowercase();
    // "por" is only a lead as "por favor"; on its own it starts a phrase.
    let mut start = 0;
    while let Some(word) = words.get(start) {
        let lead = folded(word);
        let please = lead == "por" && words.get(start + 1).is_some_and(|next| folded(next) == "favor");
        if !LEADS.contains(&lead.as_str()) || (lead == "por" && !please) {
            break;
        }
        start += if please { 2 } else { 1 };
    }
    let verb = folded(words.get(start)?);
    let rest = words[start + 1..].join(" ");
    let rest = rest.trim().trim_end_matches(['.', '!', '?']).trim();
    let rest = rest.strip_suffix("por favor").map_or(rest, str::trim_end);

    if asks_to_search(&verb) && !rest.is_empty() {
        return recall.search(rest);
    }
    let closes_it = matches!(verb.as_str(), "cierralo" | "cierrala") && rest.is_empty()
        || verb == "cierra" && matches!(fold_diacritics(rest).to_lowercase().as_str(), "eso" | "esto" | "eso mismo");
    if closes_it {
        return recall.app.clone().map(|app| Intent::CloseApp { app });
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn opened_youtube() -> Recall {
        Recall::of(&Intent::OpenUrl { url: "https://www.youtube.com".to_string() }).unwrap()
    }

    /// The search page a follow-up ends up at, whether it is typed in place or opened.
    fn url(intent: Option<Intent>) -> Option<String> {
        match intent {
            Some(Intent::OpenUrl { url } | Intent::SearchInSite { url, .. }) => Some(url),
            _ => None,
        }
    }

    #[test]
    fn only_opening_something_leaves_anything_to_lean_on() {
        assert!(Recall::of(&Intent::OpenApp { app: "Notes".to_string() }).is_some());
        assert!(Recall::of(&Intent::OpenUrl { url: "https://ejemplo.com".to_string() }).is_some());
        assert_eq!(Recall::of(&Intent::WebSearch { query: "x".to_string() }), None);
        assert_eq!(Recall::of(&Intent::CloseApp { app: "Notes".to_string() }), None);
    }

    #[test]
    fn after_opening_youtube_a_search_is_youtubes_own() {
        let expected = Some("https://www.youtube.com/results?search_query=Naruto".to_string());
        for said in ["ahora busca Naruto", "busca Naruto", "Y busca Naruto.", "búscame Naruto", "pon Naruto por favor"]
        {
            assert_eq!(url(follow_up(said, &opened_youtube())), expected, "{said}");
        }
        assert_eq!(
            url(follow_up("busca Naruto en YouTube", &opened_youtube())),
            expected,
            "\"en YouTube\" is where it already is"
        );
    }

    #[test]
    fn a_search_verb_one_letter_off_is_still_a_search() {
        // What the speech model really wrote for "busca Naruto" in a test with a voice.
        let searched = follow_up("buscan al otro", &opened_youtube());
        assert_eq!(url(searched).as_deref(), Some("https://www.youtube.com/results?search_query=al%20otro"));
        assert!(follow_up("buscas Naruto", &opened_youtube()).is_some());
        assert_eq!(follow_up("bueno Naruto", &opened_youtube()), None, "a different word is a different word");
    }

    #[test]
    fn naming_another_place_keeps_the_search_out_of_the_site() {
        for said in ["busca Naruto en Google", "busca recetas en internet", "busca Naruto en Netflix"] {
            assert_eq!(follow_up(said, &opened_youtube()), None, "{said}");
        }
        assert_eq!(
            url(follow_up("busca el opening de Naruto en HD", &opened_youtube())).as_deref(),
            Some("https://www.youtube.com/results?search_query=el%20opening%20de%20Naruto%20en%20HD")
        );
    }

    #[test]
    fn where_the_site_is_open_decides_how_the_search_reaches_it() {
        // YouTube open as an app of its own (a web app): "/" moves to its search box.
        let app = Recall::of(&Intent::OpenApp { app: "YouTube".to_string() }).unwrap();
        assert_eq!(
            app.search("Naruto"),
            Some(Intent::SearchInSite {
                site: "YouTube".to_string(),
                query: "Naruto".to_string(),
                url: "https://www.youtube.com/results?search_query=Naruto".to_string(),
                focus: "/".to_string(),
            })
        );
        // Spotify's app has ⌘L for it; opened in a browser it has nothing (⌘L is the address bar).
        let spotify_app = Recall::of(&Intent::OpenApp { app: "Spotify".to_string() }).unwrap();
        assert!(
            matches!(spotify_app.search("Bad Bunny"), Some(Intent::SearchInSite { focus, .. }) if focus == "cmd+l")
        );
        let spotify_web = Recall::of(&Intent::OpenUrl { url: "https://open.spotify.com".to_string() }).unwrap();
        assert!(matches!(spotify_web.search("Bad Bunny"), Some(Intent::OpenUrl { .. })));
    }

    #[test]
    fn an_app_that_is_also_a_site_searches_there() {
        let spotify = Recall::of(&Intent::OpenApp { app: "Spotify".to_string() }).unwrap();
        assert_eq!(
            url(follow_up("busca Bad Bunny", &spotify)).as_deref(),
            Some("https://open.spotify.com/search/Bad%20Bunny")
        );
    }

    #[test]
    fn closing_what_was_just_opened_needs_an_app() {
        let notes = Recall::of(&Intent::OpenApp { app: "Notes".to_string() }).unwrap();
        for said in ["ciérralo", "cierra eso.", "Ciérrala"] {
            assert_eq!(follow_up(said, &notes), Some(Intent::CloseApp { app: "Notes".to_string() }), "{said}");
        }
        assert_eq!(follow_up("ciérralo", &opened_youtube()), None, "a website is not an app to close");
    }

    #[test]
    fn anything_else_said_in_the_window_is_dictation() {
        let youtube = opened_youtube();
        for said in [
            "hola cómo estás",
            "mañana busca a Naruto en la biblioteca y me avisas",
            "busca",
            "",
            "por favor",
            "cierra la puerta",
        ] {
            let found = follow_up(said, &youtube);
            // Only a search phrase may match — and "mañana busca…" starts with something else.
            assert!(found.is_none() || said.starts_with("busca"), "{said}: {found:?}");
        }
        assert_eq!(follow_up("mañana busca a Naruto en la biblioteca", &youtube), None);
        assert_eq!(follow_up("busca", &youtube), None, "a search for nothing");
    }

    #[test]
    fn with_no_site_search_a_follow_up_search_is_left_to_the_normal_path() {
        let notes = Recall::of(&Intent::OpenApp { app: "Notes".to_string() }).unwrap();
        assert_eq!(follow_up("busca Naruto", &notes), None);
    }
}
