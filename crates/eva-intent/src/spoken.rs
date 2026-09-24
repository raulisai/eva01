//! Addresses as they are *said*, turned into what they are *written* as.
//!
//! The speech model writes what it hears: "google punto com punto mx",
//! "localhost tres mil". Found checking commands against the real binary:
//! both went to an agent (slow, and it costs quota) instead of opening the
//! browser, and "github punto com" opened the GitHub app instead of the site.

use eva_text::fold_diacritics;

/// Top-level domains said at the end of a spoken address. A dictated "punto"
/// only means "." when the address ends in one of these — "abre el punto de
/// venta" stays an app or an agent task.
const TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "ai", "co", "mx", "es", "ar", "cl", "pe", "uy", "ve", "ec", "bo", "py",
    "us", "uk", "de", "fr", "it", "edu", "gob", "gov", "info", "me", "tv", "xyz", "so", "fm", "ly", "gg", "sh",
];

/// What the address is, written out: `google.com.mx`, `localhost:3000`, or
/// `None` when `text` is not a spoken address.
pub fn spoken_url(text: &str) -> Option<String> {
    local_address(text).or_else(|| spoken_domain(text))
}

/// "localhost", optionally with a port said as digits or words, after
/// ":", "puerto" or "dos puntos": "localhost tres mil" → `localhost:3000`.
fn local_address(text: &str) -> Option<String> {
    let folded = fold_diacritics(text.trim());
    let rest = folded.strip_prefix("localhost")?;
    if rest.chars().next().is_some_and(char::is_alphanumeric) {
        return None; // "localhosting"
    }
    let rest = rest.trim_start_matches([':', ' ']);
    let rest = rest.strip_prefix("dos puntos").or_else(|| rest.strip_prefix("puerto")).unwrap_or(rest).trim();
    if rest.is_empty() {
        return Some("localhost".to_string());
    }
    let port = rest.parse::<u32>().ok().or_else(|| spanish_number(rest))?;
    (port <= 65_535).then(|| format!("localhost:{port}"))
}

/// "google punto com punto mx" → `google.com.mx`; the words of one label are
/// joined ("mercado libre punto com" → `mercadolibre.com`).
fn spoken_domain(text: &str) -> Option<String> {
    let folded = fold_diacritics(text.trim());
    let labels: Vec<String> = folded.split(" punto ").map(|label| label.split_whitespace().collect()).collect();
    let (tld, _) = labels.split_last()?;
    let well_formed = labels.len() >= 2
        && TLDS.contains(&tld.as_str())
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    well_formed.then(|| labels.join("."))
}

/// A whole number from 0 to 99 999 said in Spanish words ("tres mil",
/// "ocho mil ochenta", "cinco mil ciento setenta y tres"), or `None` if any
/// word is not part of one.
pub fn spanish_number(text: &str) -> Option<u32> {
    let folded = fold_diacritics(text);
    let words: Vec<&str> = folded.split_whitespace().filter(|w| *w != "y").collect();
    if words.is_empty() {
        return None;
    }
    let (mut total, mut current) = (0u32, 0u32);
    for word in words {
        if word == "mil" {
            total += current.max(1) * 1_000;
            current = 0;
        } else {
            current += word_value(word)?;
        }
    }
    let number = total + current;
    (number < 100_000).then_some(number)
}

fn word_value(word: &str) -> Option<u32> {
    let value = match word {
        "cero" => 0,
        "un" | "uno" | "una" => 1,
        "dos" => 2,
        "tres" => 3,
        "cuatro" => 4,
        "cinco" => 5,
        "seis" => 6,
        "siete" => 7,
        "ocho" => 8,
        "nueve" => 9,
        "diez" => 10,
        "once" => 11,
        "doce" => 12,
        "trece" => 13,
        "catorce" => 14,
        "quince" => 15,
        "dieciseis" => 16,
        "diecisiete" => 17,
        "dieciocho" => 18,
        "diecinueve" => 19,
        "veinte" => 20,
        "veintiun" | "veintiuno" => 21,
        "veintidos" => 22,
        "veintitres" => 23,
        "veinticuatro" => 24,
        "veinticinco" => 25,
        "veintiseis" => 26,
        "veintisiete" => 27,
        "veintiocho" => 28,
        "veintinueve" => 29,
        "treinta" => 30,
        "cuarenta" => 40,
        "cincuenta" => 50,
        "sesenta" => 60,
        "setenta" => 70,
        "ochenta" => 80,
        "noventa" => 90,
        "cien" | "ciento" => 100,
        "doscientos" | "doscientas" => 200,
        "trescientos" | "trescientas" => 300,
        "cuatrocientos" | "cuatrocientas" => 400,
        "quinientos" | "quinientas" => 500,
        "seiscientos" | "seiscientas" => 600,
        "setecientos" | "setecientas" => 700,
        "ochocientos" | "ochocientas" => 800,
        "novecientos" | "novecientas" => 900,
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn spoken_domains_become_written_ones() {
        assert_eq!(spoken_url("google punto com punto mx").as_deref(), Some("google.com.mx"));
        assert_eq!(spoken_url("github punto com").as_deref(), Some("github.com"));
        assert_eq!(spoken_url("www punto apple punto com").as_deref(), Some("www.apple.com"));
        assert_eq!(spoken_url("Mercado Libre punto com punto mx").as_deref(), Some("mercadolibre.com.mx"));
    }

    #[test]
    fn a_punto_that_is_not_an_address_is_left_alone() {
        assert_eq!(spoken_url("el punto de venta"), None);
        assert_eq!(spoken_url("la app punto de venta punto final"), None, "no known domain at the end");
        assert_eq!(spoken_url("brave"), None);
    }

    #[test]
    fn localhost_with_a_port_in_digits_or_words() {
        assert_eq!(spoken_url("localhost").as_deref(), Some("localhost"));
        assert_eq!(spoken_url("localhost 3000").as_deref(), Some("localhost:3000"));
        assert_eq!(spoken_url("localhost:8080").as_deref(), Some("localhost:8080"));
        assert_eq!(spoken_url("localhost tres mil").as_deref(), Some("localhost:3000"));
        assert_eq!(spoken_url("localhost puerto ocho mil ochenta").as_deref(), Some("localhost:8080"));
        assert_eq!(
            spoken_url("localhost dos puntos cinco mil ciento setenta y tres").as_deref(),
            Some("localhost:5173")
        );
        assert_eq!(spoken_url("localhost de mi proyecto"), None);
        assert_eq!(spoken_url("localhost 99999"), None, "not a port");
    }

    #[test]
    fn spanish_numbers_up_to_tens_of_thousands() {
        for (said, value) in [
            ("cero", 0),
            ("quince", 15),
            ("veintitrés", 23),
            ("cuarenta y dos", 42),
            ("cien", 100),
            ("ciento uno", 101),
            ("mil", 1000),
            ("tres mil", 3000),
            ("ocho mil ochenta", 8080),
            ("cinco mil ciento setenta y tres", 5173),
            ("veinte mil", 20_000),
        ] {
            assert_eq!(spanish_number(said), Some(value), "{said}");
        }
        assert_eq!(spanish_number("tres gatos"), None);
        assert_eq!(spanish_number(""), None);
    }
}
