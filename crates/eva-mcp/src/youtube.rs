//! "Play the first video of this search": asks YouTube for the results page —
//! the same page the user's browser would show, fetched from here — and takes
//! the first video in it, so opening it starts the music instead of showing a
//! list. Anything unexpected (no network, a consent wall, a changed page)
//! falls back to the results page.

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(6);

fn encode(query: &str) -> String {
    let mut out = String::new();
    for byte in query.trim().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The page of results for `query`.
pub fn results_url(query: &str) -> String {
    format!("https://www.youtube.com/results?search_query={}", encode(query))
}

/// The address of the first video for `query`, or the results page.
pub fn watch_url(query: &str) -> String {
    fetch(&results_url(query))
        .and_then(|html| first_video_id(&html))
        .map_or_else(|| results_url(query), |id| format!("https://www.youtube.com/watch?v={id}"))
}

fn fetch(url: &str) -> Option<String> {
    let agent = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().new_agent();
    let mut response = agent
        .get(url)
        .header("Accept-Language", "es")
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15")
        .call()
        .ok()?;
    response.body_mut().read_to_string().ok()
}

/// The first video id in a results page: the first `"videoId":"…"` of the
/// embedded data, which is the top result.
pub fn first_video_id(html: &str) -> Option<String> {
    let after = html.split("\"videoId\":\"").nth(1)?;
    let id: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (id.len() == 11).then_some(id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_first_video_of_the_embedded_data_is_the_top_result() {
        let html = r#"..."videoRenderer":{"videoId":"jfKfPfyJRdk","thumbnail":{}}... "videoId":"5qap5aO4i9A""#;
        assert_eq!(first_video_id(html).as_deref(), Some("jfKfPfyJRdk"));
    }

    #[test]
    fn a_page_without_a_video_or_with_something_odd_is_nothing() {
        assert_eq!(first_video_id("<html>consent</html>"), None);
        assert_eq!(first_video_id(r#""videoId":"corto""#), None);
    }

    #[test]
    fn the_query_is_encoded_into_the_results_address() {
        assert_eq!(
            results_url("música chill & más"),
            "https://www.youtube.com/results?search_query=m%C3%BAsica%20chill%20%26%20m%C3%A1s"
        );
    }
}
