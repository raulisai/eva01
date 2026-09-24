//! Safety rules no configuration can loosen. The config sets a policy per
//! action kind; these set a *floor* for specific, dangerous arguments, and the
//! gateway takes the stricter of the two. A config that says
//! `open_url = "auto"` still gets asked about a `smb://` link.

use crate::action::Action;
use eva_config::{ActionKind, Policy};

/// Apps that must never be quit by a voice command or an agent: closing them
/// takes the desktop, the session, or EVA itself down.
const PROTECTED_APPS: &[&str] =
    &["finder", "dock", "loginwindow", "systemuiserver", "windowserver", "eva01", "eva-shell"];

/// URL schemes safe to open without asking. `macappstore` only opens the App
/// Store on a search: what "abre X → no está instalada" offers.
const SAFE_SCHEMES: &[&str] = &["http", "https", "mailto", "macappstore"];

/// URL schemes that execute code in whatever handles them.
const DANGEROUS_SCHEMES: &[&str] = &["javascript", "data", "vbscript"];

/// The minimum policy for `action` regardless of configuration, with the
/// reason when it is above `Auto`.
pub fn safety_floor(action: &Action) -> (Policy, Option<&'static str>) {
    match action.kind {
        ActionKind::CloseApp if is_protected_app(&action.subject) => {
            (Policy::Block, Some("cerrar esa aplicación tumbaría el escritorio o a EVA misma"))
        }
        ActionKind::OpenUrl => url_floor(&action.subject),
        ActionKind::InsertText if action.subject.contains(['\n', '\r']) => {
            (Policy::Confirm, Some("un texto de varias líneas pegado en una terminal ejecutaría cada línea"))
        }
        _ => (Policy::Auto, None),
    }
}

fn is_protected_app(name: &str) -> bool {
    let name = name.trim().trim_end_matches(".app").to_lowercase();
    PROTECTED_APPS.contains(&name.as_str())
}

fn url_floor(url: &str) -> (Policy, Option<&'static str>) {
    match scheme_of(url) {
        // "github.com/foo" — no scheme; opening adds https.
        None => (Policy::Auto, None),
        Some(scheme) if SAFE_SCHEMES.contains(&scheme.as_str()) => (Policy::Auto, None),
        Some(scheme) if DANGEROUS_SCHEMES.contains(&scheme.as_str()) => {
            (Policy::Block, Some("ese tipo de enlace ejecuta código"))
        }
        Some(_) => (Policy::Confirm, Some("no es un enlace web: podría abrir o ejecutar algo local")),
    }
}

/// The URL that will actually be opened for `input` — the one the gateway
/// must rule on, so what the user confirms is what opens. Kept as it is when
/// it already has a scheme (`mailto:`, `x-apple.systempreferences:` have no
/// `//`, and prefixing `https://` broke them); `http://` for a local address,
/// because a dev server on `https://localhost:3000` fails to connect; and
/// `https://` for everything else.
pub fn normalize_url(input: &str) -> String {
    let url = input.trim();
    if scheme_of(url).is_some() {
        return url.to_string();
    }
    let host_and_port = url.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match host_and_port.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host_and_port,
    };
    let scheme = if is_local_host(host) { "http" } else { "https" };
    format!("{scheme}://{url}")
}

/// Loopback, a `.local` name, or a private IPv4 address: the machine itself
/// or its own network, where development servers run without TLS.
fn is_local_host(host: &str) -> bool {
    let host = host.to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host == "[::1]" {
        return true;
    }
    let octets: Vec<u8> = host.split('.').filter_map(|part| part.parse().ok()).collect();
    match octets.as_slice() {
        [127, ..] | [10, ..] | [192, 168, ..] | [0, 0, 0, 0] => octets.len() == 4,
        [172, second, ..] => octets.len() == 4 && (16..=31).contains(second),
        _ => false,
    }
}

/// The URL's scheme, lowercased, if it has one. `localhost:3000` and
/// `github.com:443/x` look like `scheme:rest` but are hosts with ports, so
/// a prefix that is `localhost` or contains a dot, followed by nothing but a
/// port number, is a host. Anything else with a valid scheme shape is a
/// scheme — including `tel:123` (digits, but no host-like prefix) and
/// `x-apple.systempreferences:foo` (a dot, but not a port after it).
fn scheme_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once(':')?;
    let valid_shape = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let port = rest.split('/').next().unwrap_or_default();
    let is_host_with_port = (scheme.eq_ignore_ascii_case("localhost") || scheme.contains('.'))
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit());
    (valid_shape && !is_host_with_port).then(|| scheme.to_lowercase())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use eva_config::Origin;

    fn floor(kind: ActionKind, subject: &str) -> Policy {
        safety_floor(&Action::new(kind, Origin::Voice, subject)).0
    }

    #[test]
    fn web_links_and_bare_domains_need_no_confirmation() {
        for url in ["https://github.com/x", "http://localhost:3000", "github.com/foo", "www.apple.com", "mailto:a@b.co"]
        {
            assert_eq!(floor(ActionKind::OpenUrl, url), Policy::Auto, "{url}");
        }
    }

    #[test]
    fn a_host_with_a_port_is_not_mistaken_for_a_scheme() {
        for url in ["localhost:3000", "127.0.0.1:8080/api", "github.com:443/x"] {
            assert_eq!(floor(ActionKind::OpenUrl, url), Policy::Auto, "{url}");
        }
    }

    #[test]
    fn non_web_schemes_must_be_confirmed() {
        for url in ["file:///etc/hosts", "smb://servidor/x", "ssh://host", "x-apple.systempreferences:foo", "tel:123"] {
            assert_eq!(floor(ActionKind::OpenUrl, url), Policy::Confirm, "{url}");
        }
    }

    #[test]
    fn code_executing_schemes_are_blocked_outright() {
        for url in ["javascript:alert(1)", "JAVASCRIPT:alert(1)", "data:text/html,<script>", " vbscript:x"] {
            assert_eq!(floor(ActionKind::OpenUrl, url), Policy::Block, "{url}");
        }
    }

    #[test]
    fn the_desktop_and_eva_itself_cannot_be_quit() {
        for app in ["Finder", "finder", "Dock", "loginwindow", "SystemUIServer", "EVA01", "EVA01.app"] {
            assert_eq!(floor(ActionKind::CloseApp, app), Policy::Block, "{app}");
        }
        assert_eq!(floor(ActionKind::CloseApp, "Spotify"), Policy::Auto);
    }

    #[test]
    fn multiline_text_pasted_for_an_agent_is_confirmed_single_line_is_not() {
        assert_eq!(floor(ActionKind::InsertText, "ls -la\nrm -rf ~"), Policy::Confirm);
        assert_eq!(floor(ActionKind::InsertText, "una sola línea"), Policy::Auto);
    }

    #[test]
    fn a_url_is_normalized_to_what_will_really_be_opened() {
        for (said, opened) in [
            ("github.com", "https://github.com"),
            ("www.google.com/search?q=x", "https://www.google.com/search?q=x"),
            ("localhost", "http://localhost"),
            ("localhost:3000", "http://localhost:3000"),
            ("127.0.0.1:8080/api", "http://127.0.0.1:8080/api"),
            ("192.168.1.20:5000", "http://192.168.1.20:5000"),
            ("mi-mac.local:8000", "http://mi-mac.local:8000"),
            ("172.20.0.3", "http://172.20.0.3"),
            ("172.40.0.3", "https://172.40.0.3"),
            ("https://github.com/x", "https://github.com/x"),
            ("http://localhost:3000", "http://localhost:3000"),
            ("mailto:ana@ejemplo.com", "mailto:ana@ejemplo.com"),
            ("macappstore://search.itunes.apple.com/x?q=a", "macappstore://search.itunes.apple.com/x?q=a"),
            (
                "x-apple.systempreferences:com.apple.preference.security",
                "x-apple.systempreferences:com.apple.preference.security",
            ),
            ("file:///etc/hosts", "file:///etc/hosts"),
        ] {
            assert_eq!(normalize_url(said), opened, "{said}");
        }
    }

    #[test]
    fn the_floor_of_a_normalized_local_address_is_still_auto() {
        assert_eq!(floor(ActionKind::OpenUrl, &normalize_url("localhost:3000")), Policy::Auto);
        assert_eq!(floor(ActionKind::OpenUrl, &normalize_url("mailto:a@b.co")), Policy::Auto);
        assert_eq!(floor(ActionKind::OpenUrl, "macappstore://search.itunes.apple.com/x?q=a"), Policy::Auto);
    }

    #[test]
    fn a_reason_accompanies_every_floor_above_auto() {
        let (policy, reason) = safety_floor(&Action::new(ActionKind::OpenUrl, Origin::Voice, "file:///x"));
        assert_eq!(policy, Policy::Confirm);
        assert!(reason.is_some());
        assert_eq!(safety_floor(&Action::new(ActionKind::Notify, Origin::Voice, "hola")), (Policy::Auto, None));
    }
}
