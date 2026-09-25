//! Turns a request that no rule understood ("abre YouTube y busca música
//! chill") into a list of steps, by asking the small local model. The model
//! only *proposes*: it is told the few kinds of step that exist, what comes
//! back is checked line by line by the caller, the user is asked before
//! anything runs, and every step still goes through the gateway. It cannot
//! run a command, write a file or reach a service — those steps do not exist.
//!
//! Like [`crate::AppResolver`], only an address on this Mac is accepted, so
//! what the user said never leaves it.

use serde_json::json;
use std::time::Duration;

/// A cold model loads for several seconds; a warm one answers in about one.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// More steps than this is a model rambling, not a plan.
pub const MAX_STEPS: usize = 6;

const INSTRUCTIONS: &str = "Eres el planificador de un asistente de voz para Mac. Conviertes lo que pidió el usuario \
en una lista corta de pasos. Solo existen estos pasos, una línea cada uno:\n\
- Nombre exacto de una app de la lista, o una dirección web (ejemplo.com): la abre.\n\
- buscar: <consulta>  → busca en Google.\n\
- youtube: <consulta>  → busca en YouTube y pone el primer video.\n\
- reproducir: <enlace de Spotify o nombre de una playlist de Música>  → solo si el usuario lo dijo; nunca inventes enlaces.\n\
- pausar:   siguiente:   anterior:   reproducir:  → controlan la música que ya suena.\n\
Reglas: usa los nombres de app EXACTOS de la lista dada. Si pide música o un video sin decir dónde, usa youtube:. \
«abre YouTube y busca X» es solo «youtube: X». No repitas pasos. Si lo pedido NO se puede hacer con estos pasos \
(programar, escribir código, borrar, enviar mensajes, preguntas, etc.) responde con una lista vacía.\n\
Responde SOLO con JSON: {\"pasos\": [\"...\"]}\n\
Ejemplos:\n\
«abre YouTube y busca música chill» → {\"pasos\": [\"youtube: música chill\"]}\n\
«pon música para concentrarme» → {\"pasos\": [\"youtube: música para concentrarme\"]}\n\
«abre Spotify» → {\"pasos\": [\"Spotify\"]}\n\
«abre Brave y busca el clima de mañana» → {\"pasos\": [\"Brave Browser\", \"buscar: clima de mañana\"]}\n\
«abre Spotify y pon la siguiente canción» → {\"pasos\": [\"Spotify\", \"siguiente:\"]}\n\
«pausa la música y abre Notas» → {\"pasos\": [\"pausar:\", \"Notes\"]}\n\
«agrega tests al login» → {\"pasos\": []}\n\
«corrige el bug y súbelo a git» → {\"pasos\": []}\n\
Nunca añadas pasos que el usuario no pidió.";

/// A local OpenAI-compatible model (Ollama) that plans steps.
#[derive(Debug, Clone)]
pub struct Planner {
    base_url: String,
    model: String,
}

impl Planner {
    /// A planner for `base_url`, or `None` if that address is not on this Mac.
    pub fn new(base_url: &str, model: &str) -> Option<Planner> {
        let rest = base_url.strip_prefix("http://")?;
        let host = rest.split(['/', ':']).next().unwrap_or("");
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
            .then(|| Planner { base_url: base_url.trim_end_matches('/').to_string(), model: model.to_string() })
    }

    /// The lines of the plan for `said`, given the apps installed; empty when
    /// the model says it cannot be done, does not answer, or answers with
    /// something that is not a plan.
    pub fn plan(&self, said: &str, apps: &[String]) -> Vec<String> {
        if said.trim().is_empty() {
            return Vec::new();
        }
        let agent = ureq::Agent::config_builder().timeout_global(Some(REQUEST_TIMEOUT)).build().new_agent();
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "max_tokens": 300,
            "response_format": { "type": "json_object" },
            "messages": [
                {"role": "system", "content": INSTRUCTIONS},
                {"role": "user", "content": format!("Apps instaladas: {}\nPidió: «{}»", apps.join(", "), said.trim())},
            ],
        });
        let answer = agent
            .post(&format!("{}/chat/completions", self.base_url))
            .send_json(&body)
            .ok()
            .and_then(|mut response| response.body_mut().read_json::<serde_json::Value>().ok())
            .and_then(|reply| reply.pointer("/choices/0/message/content").and_then(|c| c.as_str().map(str::to_string)));
        answer.map(|text| parse_plan(&text)).unwrap_or_default()
    }
}

/// The steps in the model's `answer`: an object with a `pasos` list of
/// strings. Anything else — prose, a wrong shape, control characters, too
/// many steps — is no plan at all, never a partial one.
pub fn parse_plan(answer: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(answer.trim()) else { return Vec::new() };
    let Some(list) = value.get("pasos").and_then(|p| p.as_array()) else { return Vec::new() };
    let lines: Option<Vec<String>> = list
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::trim)
                .filter(|l| !l.is_empty() && l.chars().count() <= 200 && !l.chars().any(char::is_control))
                .map(str::to_string)
        })
        .collect();
    match lines {
        Some(lines) if lines.len() <= MAX_STEPS => lines,
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve(content: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 16384];
                let _ = stream.read(&mut buffer);
                let payload = json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len())
                        .as_bytes(),
                );
            }
        });
        url
    }

    #[test]
    fn only_an_address_on_this_mac_is_accepted() {
        assert!(Planner::new("http://127.0.0.1:11434/v1", "m").is_some());
        assert!(Planner::new("http://localhost:11434/v1", "m").is_some());
        assert!(Planner::new("https://api.openai.com/v1", "m").is_none());
        assert!(Planner::new("http://192.168.1.5:11434/v1", "m").is_none());
    }

    #[test]
    fn a_well_formed_plan_is_its_lines() {
        assert_eq!(
            parse_plan(r#"{"pasos": ["Brave Browser", "youtube: música chill"]}"#),
            ["Brave Browser", "youtube: música chill"]
        );
        assert!(parse_plan(r#"{"pasos": []}"#).is_empty());
    }

    #[test]
    fn anything_that_is_not_a_plan_is_nothing() {
        for bad in [
            "abre YouTube",
            r#"{"steps": ["x"]}"#,
            r#"{"pasos": "youtube: x"}"#,
            r#"{"pasos": [1, 2]}"#,
            r#"{"pasos": ["ok", "mal\ncon salto"]}"#,
            r#"{"pasos": ["a","b","c","d","e","f","g"]}"#,
            "",
        ] {
            assert!(parse_plan(bad).is_empty(), "{bad}");
        }
    }

    #[test]
    fn the_model_is_asked_and_its_plan_comes_back() {
        let url = serve(r#"{"pasos": ["youtube: música chill"]}"#);
        let planner = Planner::new(&url, "m").unwrap();
        assert_eq!(
            planner.plan("abre youtube y busca música chill", &["Brave Browser".to_string()]),
            ["youtube: música chill"]
        );
        assert!(planner.plan("   ", &[]).is_empty());
    }

    #[test]
    fn a_model_that_is_not_there_gives_no_plan() {
        let planner = Planner::new("http://127.0.0.1:9/v1", "m").unwrap();
        assert!(planner.plan("abre youtube", &[]).is_empty());
    }

    /// Against the real local model: `cargo test -p eva-text --lib real_model -- --ignored --nocapture`.
    /// Not run by default (needs Ollama with the model), which is why it only prints.
    #[test]
    #[ignore = "needs Ollama running with qwen2.5:3b"]
    fn real_model_plans_what_people_say() {
        let planner = Planner::new("http://127.0.0.1:11434/v1", "qwen2.5:3b").unwrap();
        let apps: Vec<String> =
            ["Brave Browser", "Spotify", "Safari", "Notes", "Google Chrome", "Calculator"].map(String::from).to_vec();
        for said in [
            "abre YouTube y busca música chill",
            "ponme música tranquila para trabajar",
            "abre Spotify y pon la siguiente canción",
            "abre Brave y busca el clima de mañana",
            "busca una playlist de jazz en youtube",
            "abre la calculadora",
            "agrega tests al login y corrígelo",
            "cuéntame un chiste",
        ] {
            let started = std::time::Instant::now();
            println!("{said:?} -> {:?}  ({} ms)", planner.plan(said, &apps), started.elapsed().as_millis());
        }
    }
}
