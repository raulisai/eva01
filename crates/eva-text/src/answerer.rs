//! Short spoken answers in conversation: "Eva, ¿qué es un worktree?" gets a
//! sentence or two out loud from the small local model, instead of a coding
//! agent spun up in a project for something that was only a question.
//!
//! Like [`crate::Planner`], only a model on this Mac is used, so what the user
//! says never leaves it; and the answer is checked before it is spoken: plain
//! sentences, short, no code, no lists — something that sounds like speech.

use serde_json::json;
use std::time::Duration;

/// A warm small model answers in about a second; a cold one loads first.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Longer than this is a lecture, not an answer to say out loud.
const MAX_ANSWER_CHARS: usize = 420;

const INSTRUCTIONS: &str = "Eres Eva, una asistente de voz en una Mac. Hablas con el usuario en español, de tú, \
con naturalidad y calidez, como una persona. Respondes en voz alta: una o dos frases cortas, sin listas, sin \
markdown, sin emojis, sin código. Si no sabes algo, dilo con sencillez. Si te piden hacer algo en la \
computadora o en un proyecto de código, di en una frase que se lo pasas a un agente. No inventes datos \
del usuario ni de la hora actual.";

/// A local OpenAI-compatible model (Ollama) that answers short questions.
#[derive(Debug, Clone)]
pub struct Answerer {
    base_url: String,
    model: String,
}

impl Answerer {
    /// An answerer for `base_url`, or `None` if that address is not on this Mac.
    pub fn new(base_url: &str, model: &str) -> Option<Answerer> {
        let rest = base_url.strip_prefix("http://")?;
        let host = rest.split(['/', ':']).next().unwrap_or("");
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
            .then(|| Answerer { base_url: base_url.trim_end_matches('/').to_string(), model: model.to_string() })
    }

    /// A spoken answer to `said`, given `earlier` turns of this conversation
    /// (oldest first, as `(said, answered)`) and `now` — the date and time in
    /// words, which the model cannot know; `None` when the model does not
    /// answer or answers with something that would not sound like speech.
    pub fn answer(&self, said: &str, earlier: &[(String, String)], now: &str) -> Option<String> {
        let said = said.trim();
        if said.is_empty() {
            return None;
        }
        let system = if now.is_empty() { INSTRUCTIONS.to_string() } else { format!("{INSTRUCTIONS} {now}") };
        let mut messages = vec![json!({"role": "system", "content": system})];
        for (user, assistant) in earlier {
            messages.push(json!({"role": "user", "content": user}));
            messages.push(json!({"role": "assistant", "content": assistant}));
        }
        messages.push(json!({"role": "user", "content": said}));
        let body = json!({
            "model": self.model,
            "temperature": 0.4,
            "max_tokens": 160,
            "messages": messages,
        });
        let agent = ureq::Agent::config_builder().timeout_global(Some(REQUEST_TIMEOUT)).build().new_agent();
        let text = agent
            .post(&format!("{}/chat/completions", self.base_url))
            .send_json(&body)
            .ok()
            .and_then(|mut response| response.body_mut().read_json::<serde_json::Value>().ok())
            .and_then(|reply| {
                reply.pointer("/choices/0/message/content").and_then(|c| c.as_str().map(str::to_string))
            })?;
        speakable(&text)
    }

    /// Loads the model so the first question does not wait for it.
    pub fn warm_up(&self) -> bool {
        self.answer("hola", &[], "").is_some()
    }
}

/// `text` as something to say out loud, or `None` if it is not that: markdown
/// marks and emoji-free, on one line, and cut at a sentence end if long.
pub fn speakable(text: &str) -> Option<String> {
    if text.contains("```") {
        return None;
    }
    let plain: String = text
        .chars()
        .filter(|c| {
            !matches!(c, '*' | '#' | '`' | '_' | '>' | '|')
                && (c.is_alphanumeric() || c.is_ascii() || is_spanish_mark(*c))
        })
        .collect();
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let plain = plain.trim_start_matches(|c: char| c == '-' || c.is_whitespace()).trim();
    if plain.is_empty() {
        return None;
    }
    if plain.chars().count() <= MAX_ANSWER_CHARS {
        return Some(plain.to_string());
    }
    // Long: up to the last sentence end that fits.
    let head: String = plain.chars().take(MAX_ANSWER_CHARS).collect();
    let end = head.rfind(['.', '?', '!'])?;
    Some(head[..=end].trim().to_string())
}

/// Spanish punctuation and accented letters that are not ASCII but are speech.
fn is_spanish_mark(c: char) -> bool {
    matches!(c, '¿' | '¡' | '«' | '»' | '—' | '–' | '…' | '“' | '”' | '‘' | '’')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A fake local model answering `content`, remembering the last request body.
    fn serve(content: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let keep = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = vec![0u8; 65536];
                let n = stream.read(&mut buffer).unwrap_or(0);
                *keep.lock().unwrap() = String::from_utf8_lossy(&buffer[..n]).into_owned();
                let payload = json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len())
                        .as_bytes(),
                );
            }
        });
        (url, seen)
    }

    #[test]
    fn only_an_address_on_this_mac_is_used() {
        assert!(Answerer::new("http://127.0.0.1:11434/v1", "m").is_some());
        assert!(Answerer::new("https://api.openai.com/v1", "m").is_none());
    }

    #[test]
    fn an_answer_comes_back_as_plain_speech_and_the_earlier_turns_are_sent() {
        let (url, seen) = serve("**Un worktree** es una copia de trabajo extra de tu repositorio.");
        let answerer = Answerer::new(&url, "m").unwrap();
        let earlier = vec![("¿qué es git?".to_string(), "Es un control de versiones.".to_string())];
        let answer = answerer.answer("¿y un worktree?", &earlier, "Ahora son las 14:05.").unwrap();
        assert_eq!(answer, "Un worktree es una copia de trabajo extra de tu repositorio.");
        let request = seen.lock().unwrap().clone();
        assert!(request.contains("Es un control de versiones.") && request.contains("y un worktree"), "{request}");
        assert!(request.contains("Ahora son las 14:05."), "the time is given to the model: {request}");
    }

    #[test]
    fn code_or_nothing_is_not_spoken() {
        assert_eq!(speakable("```rust\nfn main() {}\n```"), None);
        assert_eq!(speakable("   "), None);
        assert_eq!(speakable("- **Hola**, ¿cómo estás? 😀"), Some("Hola, ¿cómo estás?".to_string()));
    }

    #[test]
    fn a_long_answer_is_cut_at_a_sentence_end() {
        let long = format!("{} Fin de la idea. {}", "palabra ".repeat(40), "más texto ".repeat(40));
        let spoken = speakable(&long).unwrap();
        assert!(spoken.ends_with("Fin de la idea."), "{spoken}");
        assert!(spoken.chars().count() <= MAX_ANSWER_CHARS);
    }

    #[test]
    fn no_model_running_is_no_answer() {
        let answerer = Answerer::new("http://127.0.0.1:9/v1", "m").unwrap();
        assert_eq!(answerer.answer("hola", &[], ""), None);
    }
}
