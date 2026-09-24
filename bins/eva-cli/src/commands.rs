//! `eva commands`: what can be said — the built-in commands, the user's own
//! (from `config.toml` and `commands/`), and the apps found on this Mac.

use crate::CommandsAction;
use eva_config::Config;
use eva_intent::apps::AppIndex;
use eva_intent::catalog::BUILTINS;

/// A commented starting file for `commands/`, written by `eva commands path`.
const EXAMPLE: &str = "# Tus órdenes, una por archivo: copia este archivo, cámbiale el nombre y edítalo.\n\
# Cada [[commands]] hace UNA cosa: `insert` (pega un texto), `open` (apps y direcciones)\n\
# o `task` (se lo da a un agente). `also` son otras formas de decir lo mismo.\n\
# Compruébala sin ejecutarla: eva intent \"Adán, mi correo\"\n\
\n\
[[commands]]\n\
say = \"mi correo\"\n\
also = [\"mi mail\"]\n\
insert = \"yo@ejemplo.com\"\n";

/// The file `eva commands path` creates.
const EXAMPLE_FILE: &str = "ejemplo.toml.disabled";

pub fn run(action: CommandsAction) -> i32 {
    match action {
        CommandsAction::List => list(),
        CommandsAction::Apps => apps(),
        CommandsAction::Path => path(),
    }
}

fn list() -> i32 {
    let loaded = Config::load();
    for warning in &loaded.warnings {
        eprintln!("aviso: {warning}");
    }
    println!("Integradas (después de la palabra de activación «{}»):", loaded.config.wake_word.0);
    for builtin in BUILTINS {
        println!("  {:<28} {}", builtin.say, builtin.does);
        println!("  {:<28} p. ej.: {}", "", builtin.example);
    }
    println!("\nTuyas (config.toml y {}):", Config::commands_dir().display());
    let mut any = false;
    for command in loaded.config.custom_commands() {
        any = true;
        println!("  {:<28} {}   [{}]", command.say, describe(command), command.origin());
        for other in &command.also {
            println!("  {:<28} igual que «{}»", format!("  o: {other}"), command.say);
        }
    }
    if !any {
        println!("  (ninguna todavía; `eva commands path` te dice dónde ponerlas)");
    }
    0
}

/// What a custom command does, in a line.
fn describe(command: &eva_config::CommandConfig) -> String {
    use eva_config::CommandAction;
    match command.action() {
        Ok(CommandAction::Insert(text)) => format!("pega «{text}»"),
        Ok(CommandAction::Open(targets)) => format!("abre {}", targets.join(", ")),
        Ok(CommandAction::Task(prompt)) => format!("tarea para el agente: {prompt}"),
        Err(why) => why,
    }
}

fn apps() -> i32 {
    let names = eva_macos::installed_apps();
    let index = AppIndex::for_this_mac(names, eva_macos::default_app_for);
    let mut installed = index.names();
    installed.sort_unstable_by_key(|name| name.to_lowercase());
    println!("{} aplicaciones detectadas; di «Adán, abre <nombre>»:", installed.len());
    for name in installed {
        let others: Vec<&str> = index.spoken_names(name).into_iter().filter(|spoken| *spoken != name).collect();
        if others.is_empty() {
            println!("  {name}");
        } else {
            println!("  {name}   (también: {})", others.join(", "));
        }
    }
    println!("\nSi no está aquí, EVA01 dice que no está instalada y ofrece buscarla en la App Store.");
    0
}

fn path() -> i32 {
    let dir = Config::commands_dir();
    let example = dir.join(EXAMPLE_FILE);
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| {
        if example.exists() {
            Ok(())
        } else {
            std::fs::write(&example, EXAMPLE)
        }
    }) {
        eprintln!("no se pudo preparar {}: {e}", dir.display());
        return 1;
    }
    println!("{}", dir.display());
    eprintln!(
        "Cada archivo *.toml de esa carpeta se carga al iniciar (hay un ejemplo: {EXAMPLE_FILE}; quítale «.disabled»)."
    );
    0
}
