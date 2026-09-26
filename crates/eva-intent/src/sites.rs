//! The websites people ask for by name ("abre YouTube") and how to search
//! inside each one. A name that no installed app answers to and that is one
//! of these opens the site instead of offering the App Store; and knowing a
//! site's own search address is what lets "ahora busca Naruto" search *there*
//! (see [`crate::context`]) without driving its search box.

use crate::intent::Intent;
use eva_text::fold_diacritics;

/// A website EVA01 knows by name.
#[derive(Debug, PartialEq, Eq)]
pub struct Site {
    /// How it is called, as it is written on the island ("YouTube").
    pub name: &'static str,
    /// How it is said, folded (no accents, lowercase).
    pub names: &'static [&'static str],
    /// Where "abre `<site>`" goes.
    pub home: &'static str,
    /// Hosts that are this site (an address ending in one of them is it).
    pub hosts: &'static [&'static str],
    /// Its search address, with `{q}` where the (encoded) search goes.
    pub search: Option<&'static str>,
    /// The shortcut that moves the keyboard to its search box when it is open
    /// as a page in a browser (a lone "/" for many sites).
    pub focus_web: Option<&'static str>,
    /// The same when it is open as an app of its own (a web app, or a native one).
    pub focus_app: Option<&'static str>,
}

/// The known sites.
pub const SITES: &[Site] = &[
    Site {
        name: "YouTube",
        names: &["youtube", "you tube", "yutub", "yutube"],
        home: "https://www.youtube.com",
        hosts: &["youtube.com", "youtu.be"],
        search: Some("https://www.youtube.com/results?search_query={q}"),
        focus_web: Some("/"),
        focus_app: Some("/"),
    },
    Site {
        name: "Spotify",
        names: &["spotify"],
        home: "https://open.spotify.com",
        hosts: &["spotify.com"],
        search: Some("https://open.spotify.com/search/{q}"),
        focus_web: None,
        focus_app: Some("cmd+l"),
    },
    Site {
        name: "Gmail",
        names: &["gmail"],
        home: "https://mail.google.com",
        hosts: &["mail.google.com"],
        search: Some("https://mail.google.com/mail/u/0/#search/{q}"),
        focus_web: Some("/"),
        focus_app: Some("/"),
    },
    Site {
        name: "Google Drive",
        names: &["google drive", "drive"],
        home: "https://drive.google.com",
        hosts: &["drive.google.com"],
        search: Some("https://drive.google.com/drive/search?q={q}"),
        focus_web: Some("/"),
        focus_app: Some("/"),
    },
    Site {
        name: "Google Maps",
        names: &["google maps"],
        home: "https://www.google.com/maps",
        hosts: &["google.com/maps", "maps.google.com"],
        search: Some("https://www.google.com/maps/search/{q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "GitHub",
        names: &["github", "git hub"],
        home: "https://github.com",
        hosts: &["github.com"],
        search: Some("https://github.com/search?q={q}"),
        focus_web: Some("/"),
        focus_app: Some("/"),
    },
    Site {
        name: "Stack Overflow",
        names: &["stack overflow", "stackoverflow"],
        home: "https://stackoverflow.com",
        hosts: &["stackoverflow.com"],
        search: Some("https://stackoverflow.com/search?q={q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "Wikipedia",
        names: &["wikipedia"],
        home: "https://es.wikipedia.org",
        hosts: &["wikipedia.org"],
        search: Some("https://es.wikipedia.org/w/index.php?search={q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "Netflix",
        names: &["netflix"],
        home: "https://www.netflix.com",
        hosts: &["netflix.com"],
        search: Some("https://www.netflix.com/search?q={q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "Twitch",
        names: &["twitch"],
        home: "https://www.twitch.tv",
        hosts: &["twitch.tv"],
        search: Some("https://www.twitch.tv/search?term={q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "Reddit",
        names: &["reddit"],
        home: "https://www.reddit.com",
        hosts: &["reddit.com"],
        search: Some("https://www.reddit.com/search/?q={q}"),
        focus_web: Some("/"),
        focus_app: Some("/"),
    },
    Site {
        name: "Amazon",
        names: &["amazon"],
        home: "https://www.amazon.com.mx",
        hosts: &["amazon.com", "amazon.com.mx"],
        search: Some("https://www.amazon.com.mx/s?k={q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "X",
        names: &["twitter"],
        home: "https://x.com",
        hosts: &["x.com", "twitter.com"],
        search: Some("https://x.com/search?q={q}"),
        focus_web: Some("/"),
        focus_app: Some("/"),
    },
    Site {
        name: "LinkedIn",
        names: &["linkedin", "linked in"],
        home: "https://www.linkedin.com",
        hosts: &["linkedin.com"],
        search: Some("https://www.linkedin.com/search/results/all/?keywords={q}"),
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "Instagram",
        names: &["instagram"],
        home: "https://www.instagram.com",
        hosts: &["instagram.com"],
        search: None,
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "Facebook",
        names: &["facebook"],
        home: "https://www.facebook.com",
        hosts: &["facebook.com"],
        search: None,
        focus_web: None,
        focus_app: None,
    },
    Site {
        name: "ChatGPT",
        names: &["chatgpt", "chat gpt"],
        home: "https://chatgpt.com",
        hosts: &["chatgpt.com"],
        search: None,
        focus_web: None,
        focus_app: None,
    },
];

/// The site called `name` ("YouTube", "you tube"), accents and case aside.
pub fn by_spoken_name(name: &str) -> Option<&'static Site> {
    let folded = fold_diacritics(name).to_lowercase();
    let folded = folded.trim().trim_end_matches(|c: char| !c.is_alphanumeric());
    SITES.iter().find(|site| site.names.contains(&folded))
}

/// The site an address belongs to (`https://www.youtube.com/watch?v=…`).
pub fn by_url(url: &str) -> Option<&'static Site> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest).to_lowercase();
    let rest = rest.strip_prefix("www.").unwrap_or(&rest);
    let host_and_path = rest.split(['?', '#']).next().unwrap_or(rest);
    let host = host_and_path.split('/').next().unwrap_or(host_and_path);
    SITES.iter().find(|site| {
        site.hosts.iter().any(|h| match h.split_once('/') {
            // A site that lives in a path of another ("google.com/maps").
            Some(_) => host_and_path.starts_with(h),
            None => host == *h || host.ends_with(&format!(".{h}")),
        })
    })
}

/// "Abre YouTube" with no app by that name comes out of the parser as an app
/// that is not installed; if it is a website EVA01 knows, it is opened
/// instead. Kept apart from the parser so that an app installed since the
/// last look (found by looking again) is still preferred to the site.
pub fn open_by_name(intent: Intent) -> Intent {
    match &intent {
        Intent::AppNotFound { name, opening: true } => match by_spoken_name(name) {
            Some(site) => Intent::OpenUrl { url: site.home.to_string() },
            None => intent,
        },
        _ => intent,
    }
}

impl Site {
    /// The address that searches this site for `query`, if it can.
    pub fn search_url(&self, query: &str) -> Option<String> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        self.search.map(|template| template.replace("{q}", &encode(query)))
    }
}

/// `text` as it goes in a web address: everything but letters, digits and
/// `-_.~` is written as its UTF-8 bytes in `%XX`.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn a_site_is_found_by_the_way_it_is_said() {
        for said in ["YouTube", "you tube", "  youtube. ", "Youtube"] {
            assert_eq!(by_spoken_name(said).map(|s| s.home), Some("https://www.youtube.com"), "{said}");
        }
        assert_eq!(by_spoken_name("Stack Overflow").map(|s| s.home), Some("https://stackoverflow.com"));
        assert_eq!(by_spoken_name("una tienda cualquiera"), None);
    }

    #[test]
    fn an_address_is_matched_to_its_site_by_host_not_by_a_word_in_it() {
        let youtube = |url: &str| by_url(url).map(|s| s.home);
        assert_eq!(youtube("https://www.youtube.com/watch?v=abc"), Some("https://www.youtube.com"));
        assert_eq!(youtube("m.youtube.com/results?search_query=x"), Some("https://www.youtube.com"));
        assert_eq!(youtube("https://youtu.be/abc"), Some("https://www.youtube.com"));
        assert_eq!(youtube("https://ejemplo.com/youtube.com"), None, "a path is not a host");
        assert_eq!(youtube("https://notyoutube.com"), None);
        assert_eq!(by_url("https://www.google.com/maps/place/x").map(|s| s.home), Some("https://www.google.com/maps"));
        assert_eq!(by_url("https://www.google.com/search?q=maps"), None);
    }

    #[test]
    fn a_search_is_written_into_the_sites_own_search_address() {
        let youtube = by_spoken_name("youtube").unwrap();
        assert_eq!(
            youtube.search_url("Naruto shippuden").as_deref(),
            Some("https://www.youtube.com/results?search_query=Naruto%20shippuden")
        );
        assert_eq!(
            youtube.search_url("canción de niño & más").as_deref(),
            Some("https://www.youtube.com/results?search_query=canci%C3%B3n%20de%20ni%C3%B1o%20%26%20m%C3%A1s")
        );
        assert_eq!(youtube.search_url("   "), None);
        assert_eq!(by_spoken_name("instagram").unwrap().search_url("algo"), None, "no search address known");
    }
}
