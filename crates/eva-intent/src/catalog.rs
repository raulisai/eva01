//! What EVA01 understands out of the box, in one list — the source for
//! `eva commands` and the README. Each entry carries an example that a test
//! feeds to the real parser and checks against the kind it says it produces,
//! so this list cannot drift from what the parser does.

/// One built-in command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    /// How it is said, with `<…>` for the part that varies.
    pub say: &'static str,
    /// What it does, in a line.
    pub does: &'static str,
    /// A complete example, after the wake word.
    pub example: &'static str,
    /// The `kind` the parser gives the example (see `Intent`).
    pub kind: &'static str,
}

/// The built-in commands, in the order the parser tries them.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        say: "abre <app>",
        does: "abre una aplicación instalada (también por su nombre en español: «notas», «calculadora»)",
        example: "abre Spotify",
        kind: "open_app",
    },
    Builtin {
        say: "cierra <app>",
        does: "cierra una aplicación (pide confirmación; Finder y el escritorio están protegidos)",
        example: "cierra Spotify",
        kind: "close_app",
    },
    Builtin {
        say: "abre <dirección>",
        does: "abre una página en el navegador: «github.com», «google punto com», «localhost tres mil»",
        example: "abre github punto com",
        kind: "open_url",
    },
    Builtin {
        say: "abre <app que no tienes>",
        does: "avisa de que no está instalada y ofrece buscarla en la App Store",
        example: "abre Photoshop",
        kind: "app_not_found",
    },
    Builtin { say: "busca <algo>", does: "busca en la web", example: "busca el clima de mañana", kind: "web_search" },
    Builtin {
        say: "usa Claude / Codex y <tarea>",
        does: "manda la tarea a ese agente, en una rama nueva de git",
        example: "usa Claude y arregla el login",
        kind: "agent_task",
    },
    Builtin {
        say: "continúa [y <más>]",
        does: "retoma la última tarea de agente del proyecto",
        example: "continúa y agrega tests",
        kind: "continue_agent_task",
    },
    Builtin {
        say: "hazlo <cómo> (con texto seleccionado)",
        does: "reescribe el texto seleccionado: «hazlo más formal», «acórtalo», «tradúcelo al inglés»",
        example: "hazlo más formal",
        kind: "edit_selection",
    },
    Builtin {
        say: "<cualquier otra cosa>",
        does: "se la da a un agente como tarea, con las herramientas de EVA a mano",
        example: "agrega tests al login",
        kind: "agent_task",
    },
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::{apps::AppEntry, AppIndex};

    #[test]
    fn every_example_is_parsed_into_the_kind_the_list_promises() {
        let index = AppIndex::new(vec![AppEntry::new("Spotify"), AppEntry::new("GitHub Desktop")]);
        for builtin in BUILTINS {
            let intent = crate::intent::parse(builtin.example, &index);
            let json = serde_json::to_value(&intent).unwrap();
            assert_eq!(json["kind"], builtin.kind, "«{}» ({})", builtin.example, builtin.say);
        }
    }

    #[test]
    fn every_entry_says_what_it_does() {
        assert!(BUILTINS.iter().all(|b| !b.say.is_empty() && b.does.len() > 10 && !b.example.is_empty()));
    }
}
