//! Talking with EVA rather than giving her something to do: a question to
//! answer out loud ("¿qué es un worktree?", "hola, ¿cómo estás?") and a
//! goodbye that ends the conversation ("gracias", "eso es todo"). Everything
//! else said to her is still a command or a task.

use crate::intent::normalize_phrase;

/// Words that open a question.
const QUESTION_WORDS: &[&str] = &[
    "que",
    "como",
    "cual",
    "cuales",
    "cuando",
    "donde",
    "adonde",
    "quien",
    "quienes",
    "cuanto",
    "cuanta",
    "cuantos",
    "cuantas",
    "sabes",
    "conoces",
    "explicame",
    "dime",
    "cuentame",
    "recuerdame",
];

/// Greetings and small talk, as whole phrases.
const SMALL_TALK: &[&str] = &[
    "hola",
    "hola como estas",
    "como estas",
    "que tal",
    "hola que tal",
    "buenos dias",
    "buenas tardes",
    "buenas noches",
    "como te va",
    "que haces",
    "estas ahi",
    "me escuchas",
    "me oyes",
];

/// Leads that come before a question without changing it.
const LEADS: &[&str] = &["oye", "eva", "y", "pues", "entonces", "ahora", "bueno", "una", "pregunta", "oiga"];

/// Words that make a question one about the user's own work — which only an
/// agent with the project in front of it can answer, never a small model
/// talking from memory.
const ABOUT_THEIR_WORK: &[&str] = &[
    "proyecto",
    "proyectos",
    "repo",
    "repos",
    "repositorio",
    "repositorios",
    "codigo",
    "archivo",
    "archivos",
    "carpeta",
    "carpetas",
    "rama",
    "ramas",
    "commit",
    "commits",
    "test",
    "tests",
    "prueba",
    "pruebas",
    "bug",
    "bugs",
    "error",
    "errores",
    "funcion",
    "funciones",
    "clase",
    "modulo",
    "servidor",
    "deploy",
    "build",
    "compila",
    "compilar",
    "issue",
    "issues",
    "pr",
    "tarea",
    "tareas",
    "mi",
    "mis",
    "este",
    "esta",
    "esto",
];

/// Longer than this many words is a request with a story, not a question to
/// answer in a sentence.
const MAX_QUESTION_WORDS: usize = 25;

/// Whether `text` (said to EVA) is a question or small talk to answer out
/// loud, rather than work for an agent.
pub fn looks_like_question(text: &str) -> bool {
    let normalized = normalize_phrase(text);
    let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
    if words.is_empty() || words.len() > MAX_QUESTION_WORDS {
        return false;
    }
    let start = words.iter().position(|w| !LEADS.contains(w)).unwrap_or(words.len());
    let rest = &words[start..];
    if rest.is_empty() {
        return false;
    }
    if SMALL_TALK.contains(&rest.join(" ").as_str()) {
        return true;
    }
    if rest.iter().any(|w| ABOUT_THEIR_WORK.contains(w)) {
        return false;
    }
    let asks = text.contains('?') || text.contains('¿');
    let opens_like_one = QUESTION_WORDS.contains(&rest[0]) || (rest[0] == "por" && rest.get(1) == Some(&"que"));
    asks || opens_like_one
}

/// Ways of ending the conversation, as whole phrases.
const GOODBYES: &[&str] = &[
    "gracias",
    "muchas gracias",
    "gracias eva",
    "eso es todo",
    "es todo",
    "seria todo",
    "eso seria todo",
    "eso es todo gracias",
    "nada mas",
    "nada mas gracias",
    "listo gracias",
    "ya esta",
    "ya esta gracias",
    "ya no",
    "ya no gracias",
    "adios",
    "adios eva",
    "hasta luego",
    "nos vemos",
    "chao",
    "bye",
    "termina",
    "terminamos",
    "salir",
    "descansa",
];

/// Whether `text` ends the conversation with EVA. `wake_word` may be in it
/// ("gracias, Eva").
pub fn is_goodbye(text: &str, wake_word: &str) -> bool {
    let wake = normalize_phrase(wake_word);
    let words: Vec<String> =
        normalize_phrase(text).split(' ').filter(|w| !w.is_empty() && *w != wake).map(str::to_string).collect();
    if words.is_empty() {
        return false;
    }
    let phrase = words.join(" ");
    if GOODBYES.contains(&phrase.as_str()) {
        return true;
    }
    // Thanks around a goodbye: "gracias, eso es todo", "ya está, muchas gracias".
    let mut rest = phrase.as_str();
    for lead in ["ok", "vale", "perfecto", "listo", "muchas gracias", "gracias"] {
        if let Some(after) = rest.strip_prefix(lead).and_then(|r| r.strip_prefix(' ')) {
            rest = after;
        }
    }
    if rest != phrase && GOODBYES.contains(&rest) {
        return true;
    }
    for thanks in ["muchas gracias", "gracias"] {
        if let Some(before) = rest.strip_suffix(thanks).and_then(|r| r.strip_suffix(' ')) {
            rest = before;
        }
    }
    rest != phrase && GOODBYES.contains(&rest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn a_question_or_small_talk_is_answered_out_loud() {
        for said in [
            "¿qué es un worktree?",
            "qué hora es",
            "cuál es la capital de Australia",
            "oye, ¿cómo funciona la fotosíntesis?",
            "hola, ¿cómo estás?",
            "buenos días",
            "por qué el cielo es azul",
            "explícame qué es Rust",
        ] {
            assert!(looks_like_question(said), "{said}");
        }
    }

    #[test]
    fn work_and_questions_about_their_own_work_go_to_an_agent() {
        for said in [
            "agrega tests al login",
            "¿qué proyectos tengo activos?",
            "¿por qué falla el build?",
            "¿qué hace esta función?",
            "revisa mis commits de hoy",
            "",
        ] {
            assert!(!looks_like_question(said), "{said}");
        }
        let long = format!("¿qué {}?", "palabra ".repeat(30));
        assert!(!looks_like_question(&long), "a story, not a question");
    }

    #[test]
    fn a_goodbye_ends_it_with_or_without_the_wake_word() {
        for said in [
            "gracias",
            "Gracias, Eva.",
            "eso es todo",
            "Eva, nada más",
            "adiós",
            "ya no, gracias",
            "gracias, eso es todo",
            "perfecto, muchas gracias",
            "ok, eso sería todo, gracias",
        ] {
            assert!(is_goodbye(said, "Eva"), "{said}");
        }
        for said in ["gracias por abrir YouTube y ahora busca anime", "abre Spotify", "hola", "perfecto", "listo abre Spotify"] {
            assert!(!is_goodbye(said, "Eva"), "{said}");
        }
    }
}
