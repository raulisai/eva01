//! Asks a small local model which installed app a misheard name means, for the
//! last case: "abre Breve" when no rule, spelling or sound matched. The model
//! only chooses from the list it is given, and its answer is checked against
//! that list, so it cannot invent an app — the worst it can do is pick the
//! wrong one, which is why the caller asks before trusting a far-fetched pick.
//!
//! Only an address on this Mac is accepted: what the user said is never sent
//! anywhere else.

use crate::normalize::fold_diacritics;
use serde_json::json;
use std::time::Duration;

/// A cold model takes a few seconds to load; a warm one answers in well under one.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

const INSTRUCTIONS: &str = "Eres un ayudante que empareja lo que un usuario dijo con una app instalada en su Mac. \
El reconocedor de voz escribe mal los nombres (\"breve\" por \"Brave\", \"zafarí\" por \"Safari\", \"wasap\" por \
\"WhatsApp\"). Recibes lo que se oyó y la lista de apps. Responde SOLO con el nombre exacto de UNA app de la lista, \
o con la palabra NINGUNA si ninguna se le parece de verdad. Sin explicaciones.";

/// A model the local server has downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledModel {
    /// Its name, as it is asked for (`qwen2.5:3b`).
    pub name: String,
    /// Size on disk, in bytes.
    pub bytes: u64,
    /// Whether it is loaded in memory right now.
    pub loaded: bool,
}

/// A local OpenAI-compatible model (Ollama) used to resolve app names.
#[derive(Debug, Clone)]
pub struct AppResolver {
    base_url: String,
    model: String,
}

impl AppResolver {
    /// A resolver for `base_url`, or `None` if that address is not on this Mac.
    pub fn new(base_url: &str, model: &str) -> Option<AppResolver> {
        let rest = base_url.strip_prefix("http://")?;
        let host = rest.split(['/', ':']).next().unwrap_or("");
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
            .then(|| AppResolver { base_url: base_url.trim_end_matches('/').to_string(), model: model.to_string() })
    }

    /// The models the local server has, or `None` if nothing answers. Asks
    /// Ollama's own `/api/tags` and `/api/ps`, one level above the OpenAI root.
    pub fn installed_models(&self) -> Option<Vec<InstalledModel>> {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(2))).build().new_agent();
        let get = |path: &str| -> Option<serde_json::Value> {
            agent.get(&format!("{root}{path}")).call().ok()?.body_mut().read_json().ok()
        };
        let tags = get("/api/tags")?;
        let loaded: Vec<String> = get("/api/ps")
            .and_then(|ps| {
                ps["models"]
                    .as_array()
                    .map(|m| m.iter().filter_map(|x| x["name"].as_str().map(str::to_string)).collect())
            })
            .unwrap_or_default();
        Some(
            tags["models"]
                .as_array()?
                .iter()
                .filter_map(|m| {
                    let name = m["name"].as_str()?.to_string();
                    Some(InstalledModel {
                        bytes: m["size"].as_u64().unwrap_or(0),
                        loaded: loaded.contains(&name),
                        name,
                    })
                })
                .collect(),
        )
    }

    /// The app in `apps` that `heard` means, or `None` if the model says none,
    /// answers with something not in the list, or does not answer.
    pub fn resolve(&self, heard: &str, apps: &[String]) -> Option<String> {
        if heard.trim().is_empty() || apps.is_empty() {
            return None;
        }
        let agent = ureq::Agent::config_builder().timeout_global(Some(REQUEST_TIMEOUT)).build().new_agent();
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "max_tokens": 24,
            "messages": [
                {"role": "system", "content": INSTRUCTIONS},
                {"role": "user", "content": format!("Se oyó: «{heard}»\nApps: {}", apps.join(", "))},
            ],
        });
        let mut response = agent.post(&format!("{}/chat/completions", self.base_url)).send_json(&body).ok()?;
        let reply: serde_json::Value = response.body_mut().read_json().ok()?;
        let answer = reply.pointer("/choices/0/message/content")?.as_str()?;
        pick(answer, apps)
    }
}

/// The app the model's `answer` names, compared without case, accents or
/// quotes; anything that is not exactly an app of the list is nothing.
fn pick(answer: &str, apps: &[String]) -> Option<String> {
    let clean = |t: &str| fold_diacritics(t.trim().trim_matches(|c: char| !c.is_alphanumeric())).to_lowercase();
    let wanted = clean(answer.lines().next().unwrap_or(""));
    if wanted.is_empty() || wanted == "ninguna" {
        return None;
    }
    apps.iter().find(|app| clean(app) == wanted).cloned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn apps() -> Vec<String> {
        ["Brave Browser", "Safari", "Spotify"].map(String::from).to_vec()
    }

    /// A fake model on localhost that answers every request with `content`.
    fn serve(content: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 8192];
                let _ = stream.read(&mut buffer);
                let payload = json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    )
                    .as_bytes(),
                );
            }
        });
        url
    }

    #[test]
    fn only_an_address_on_this_mac_is_accepted() {
        assert!(AppResolver::new("http://127.0.0.1:11434/v1", "m").is_some());
        assert!(AppResolver::new("http://localhost:11434/v1", "m").is_some());
        assert!(AppResolver::new("https://api.openai.com/v1", "m").is_none());
        assert!(AppResolver::new("http://192.168.1.20:11434/v1", "m").is_none());
        assert!(AppResolver::new("http://127.0.0.1.evil.com/v1", "m").is_none());
    }

    #[test]
    fn the_answer_must_be_an_app_of_the_list() {
        assert_eq!(pick("Brave Browser", &apps()).as_deref(), Some("Brave Browser"));
        assert_eq!(pick(" \"safari\".\n(porque suena parecido)", &apps()).as_deref(), Some("Safari"));
        assert_eq!(pick("NINGUNA", &apps()), None);
        assert_eq!(pick("Photoshop", &apps()), None, "an app that is not in the list is invented");
        assert_eq!(pick("", &apps()), None);
    }

    #[test]
    fn a_model_that_names_an_app_resolves_it_and_one_that_does_not_is_ignored() {
        let named = AppResolver::new(&serve("Brave Browser"), "m").unwrap();
        assert_eq!(named.resolve("breve", &apps()).as_deref(), Some("Brave Browser"));
        let none = AppResolver::new(&serve("NINGUNA"), "m").unwrap();
        assert_eq!(none.resolve("egipto", &apps()), None);
    }

    #[test]
    fn the_models_the_server_has_are_listed_with_the_loaded_one_marked() {
        use std::io::{Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 4096];
                let n = stream.read(&mut buffer).unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..n]).to_string();
                let payload = if head.contains("/api/ps") {
                    json!({"models": [{"name": "qwen2.5:3b"}]})
                } else {
                    json!({"models": [{"name": "qwen2.5:3b", "size": 1_900_000_000u64}, {"name": "llama3.2:3b", "size": 2_000_000_000u64}]})
                }
                .to_string();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    )
                    .as_bytes(),
                );
            }
        });
        let models = AppResolver::new(&url, "m").unwrap().installed_models().unwrap();
        assert_eq!(models.len(), 2);
        assert!(models[0].loaded && !models[1].loaded);
        assert_eq!(models[1].bytes, 2_000_000_000);
    }

    #[test]
    fn no_model_running_is_no_answer_not_an_error() {
        let resolver = AppResolver::new("http://127.0.0.1:1/v1", "m").unwrap();
        assert_eq!(resolver.resolve("breve", &apps()), None);
    }
}
