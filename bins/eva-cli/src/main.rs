//! `eva`: the CLI from `docs/PLAN.md` §3.1 — "toda orden de voz debe ser
//! expresable como comando... esto es lo que hace testeable la capa de
//! intención sin grabar audio." Talking to EVA01 without a microphone
//! (`intent`, `dictionary`, `tasks`), setting it up (`model`, `config`,
//! `startup`), and finding out why something does not work (`doctor`,
//! `audit`).

mod analysis;
mod commands;
mod doctor;
mod model;
mod panel;
mod render;
mod session;
mod startup;

use clap::{Parser, Subcommand};
use eva_config::{support_dir, Config};
use eva_ipc::ShellToWorker;
use std::time::Duration;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "eva", about = "EVA01 — la capa de voz, desde la línea de comandos", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Cuánto esperar una respuesta antes de rendirse, en segundos. Las
    /// tareas de agente pueden tardar minutos de verdad; los comandos
    /// simples (intent, health, dictionary) responden casi al instante.
    #[arg(long, global = true, default_value_t = 1800)]
    timeout_secs: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Dice cómo se entendería un texto si acabara de ser transcrito — el mismo
    /// camino que usa el dictado real, sin audio y, por defecto, sin hacer nada.
    Intent {
        /// El texto a interpretar, p. ej. "Adán, abre brave".
        text: String,
        /// Además lo ejecuta de verdad: abre la app, pega el texto, lanza el agente.
        #[arg(long)]
        run: bool,
    },
    /// Las órdenes que EVA01 entiende: las integradas, las tuyas y tus aplicaciones.
    Commands {
        #[command(subcommand)]
        action: Option<CommandsAction>,
    },
    /// Revisa que este equipo esté listo para EVA01 y dice cómo arreglar lo que no.
    Doctor {
        /// Además, hace que cada agente responda de verdad (tarda unos segundos).
        #[arg(long)]
        smoke: bool,
        /// El reporte como un objeto JSON (lo lee la ventana Doctor de la app).
        #[arg(long)]
        json: bool,
    },
    /// Datos del panel de la app: una petición JSON por stdin, una respuesta por stdout.
    #[command(hide = true)]
    Panel {
        /// La ruta, p. ej. `history` o `config.set`.
        route: String,
    },
    /// Calibra EVA01 a tu voz: te pide unas frases, oye cómo salen y aprende cómo
    /// se escribe tu palabra de activación y tus aplicaciones (no ejecuta nada).
    Calibrate,
    /// Escucha una y otra vez y dice lo que oyó, sin ejecutar nada (lo usa el
    /// panel para enseñarle a una orden tuya las formas de pedirla).
    #[command(hide = true)]
    Teach,
    /// Pide el reporte de salud de eva-worker.
    Health,
    /// Muestra las tareas de agente en curso y las recientes.
    Tasks,
    /// Muestra lo que el gateway dejó pasar, preguntó o rechazó.
    Audit {
        /// Cuántas entradas mostrar.
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: u32,
    },
    /// Administra el diccionario personal.
    Dictionary {
        #[command(subcommand)]
        action: DictionaryAction,
    },
    /// Cambia la palabra de activación (aplica al próximo inicio).
    WakeWord {
        /// La nueva palabra de activación.
        word: String,
    },
    /// Instala y prueba el modelo de voz.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// La configuración (config.toml).
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Los dictados recientes y cuáles marcaste como mal transcritos.
    History {
        /// Cuántos mostrar.
        #[arg(long, default_value_t = 15)]
        limit: u32,
        /// Solo los que marcaste como mal transcritos (el corpus de pruebas).
        #[arg(long)]
        flagged: bool,
    },
    /// Abrir EVA01 al iniciar sesión.
    Startup {
        #[command(subcommand)]
        action: StartupAction,
    },
}

#[derive(Subcommand)]
enum CommandsAction {
    /// Las órdenes integradas y las tuyas (es lo que hace `eva commands` a secas).
    List,
    /// Las aplicaciones detectadas en este Mac y cómo decir cada una.
    Apps,
    /// Dónde poner tus órdenes (carpeta `commands/`); la crea con un ejemplo si no existe.
    Path,
}

#[derive(Subcommand)]
enum DictionaryAction {
    /// Agrega una palabra.
    Add {
        /// La palabra a agregar, en su forma preferida (p. ej. "García").
        word: String,
    },
    /// Quita una palabra.
    Remove {
        /// La palabra a quitar.
        word: String,
    },
    /// Lista todas las palabras del diccionario.
    List,
}

#[derive(Subcommand)]
enum ModelAction {
    /// Los modelos que se pueden instalar y cuáles ya lo están.
    List,
    /// Descarga un modelo y comprueba que funcione.
    Install {
        /// canary-1b-flash (recomendado) o canary-180m-flash.
        #[arg(default_value = "canary-1b-flash")]
        id: String,
        /// Vuelve a descargar todo aunque ya esté.
        #[arg(long)]
        force: bool,
    },
    /// Carga el modelo y transcribe una frase dicha por macOS.
    Verify {
        /// El modelo a probar; sin nombre, el que EVA01 usaría.
        id: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Dónde está el archivo.
    Path,
    /// La configuración que se está usando (con los valores por defecto incluidos).
    Show,
    /// Abre el archivo en tu editor, creándolo con ayuda si no existe.
    Edit,
}

#[derive(Subcommand)]
enum StartupAction {
    /// Abrir EVA01 al iniciar sesión.
    Enable,
    /// Dejar de abrirlo al iniciar sesión.
    Disable,
    /// Si está activado.
    Status,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let timeout = Duration::from_secs(cli.timeout_secs);

    let exit_code = match cli.command {
        Command::Intent { text, run: false } => {
            talk(|request_id| ShellToWorker::InterpretText { request_id, text }, timeout).await
        }
        Command::Intent { text, run: true } => {
            talk(|request_id| ShellToWorker::RunIntentText { request_id, text }, timeout).await
        }
        Command::Calibrate => talk(|request_id| ShellToWorker::Calibrate { request_id }, timeout).await,
        Command::Teach => talk(|request_id| ShellToWorker::Teach { request_id }, Duration::from_secs(TEACH_SECS)).await,
        Command::Health => talk(|request_id| ShellToWorker::HealthCheck { request_id }, timeout).await,
        Command::Tasks => talk(|request_id| ShellToWorker::ListTasks { request_id }, timeout).await,
        Command::WakeWord { word } => talk(|request_id| ShellToWorker::SetWakeWord { request_id, word }, timeout).await,
        Command::Dictionary { action } => match action {
            DictionaryAction::Add { word } => {
                talk(|request_id| ShellToWorker::AddCustomWord { request_id, word }, timeout).await
            }
            DictionaryAction::Remove { word } => {
                talk(|request_id| ShellToWorker::RemoveCustomWord { request_id, word }, timeout).await
            }
            DictionaryAction::List => talk(|request_id| ShellToWorker::ListCustomWords { request_id }, timeout).await,
        },
        Command::Commands { action } => commands::run(action.unwrap_or(CommandsAction::List)),
        Command::Panel { route } => panel::run(&route),
        Command::Doctor { smoke, json } => doctor::run(smoke, json).await,
        Command::Audit { limit } => audit(limit),
        Command::History { limit, flagged } => history(limit, flagged),
        Command::Model { action } => {
            run_blocking(move || match action {
                ModelAction::List => model::list(),
                ModelAction::Install { id, force } => model::install(&id, force),
                ModelAction::Verify { id } => model::verify(id.as_deref()),
            })
            .await
        }
        Command::Config { action } => config(&action),
        Command::Startup { action } => match action {
            StartupAction::Enable => startup::enable(),
            StartupAction::Disable => startup::disable(),
            StartupAction::Status => startup::status(),
        },
    };
    std::process::exit(exit_code);
}

/// The longest a teaching session runs before it ends by itself.
const TEACH_SECS: u64 = 900;

/// Sends one command to a fresh worker and prints what comes back.
async fn talk(command: impl FnOnce(Uuid) -> ShellToWorker, timeout: Duration) -> i32 {
    let request_id = Uuid::new_v4();
    session::run(command(request_id), request_id, timeout).await
}

/// Runs blocking work (`curl`, model inference) off the async runtime.
async fn run_blocking(work: impl FnOnce() -> i32 + Send + 'static) -> i32 {
    tokio::task::spawn_blocking(work).await.unwrap_or(2)
}

fn config(action: &ConfigAction) -> i32 {
    let loaded = Config::load();
    match action {
        ConfigAction::Path => {
            println!("{}", loaded.path.display());
            0
        }
        ConfigAction::Show => {
            for warning in &loaded.warnings {
                eprintln!("aviso: {warning}");
            }
            match toml::to_string(&loaded.config) {
                Ok(text) => {
                    print!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("no se pudo mostrar la configuración: {e}");
                    1
                }
            }
        }
        ConfigAction::Edit => {
            if let Err(e) = Config::ensure_file(&loaded.path) {
                eprintln!("no se pudo crear {}: {e}", loaded.path.display());
                return 1;
            }
            match std::process::Command::new("open").arg(&loaded.path).status() {
                Ok(status) if status.success() => {
                    println!("{}", loaded.path.display());
                    0
                }
                _ => {
                    eprintln!("no se pudo abrir el editor; el archivo está en {}", loaded.path.display());
                    1
                }
            }
        }
    }
}

/// `eva history`: the recent dictations, straight from the database — the same
/// list the flag hotkey works on, and the "retrabajos" count of
/// `docs/PLAN.md` §7.
fn history(limit: u32, flagged_only: bool) -> i32 {
    let support = support_dir();
    let path = support.join("eva.sqlite3");
    if !path.exists() {
        println!("(todavía no hay historial: {} no existe)", path.display());
        return 0;
    }
    let read = eva_store::Store::open(&path).and_then(|store| {
        let records = if flagged_only { store.transcripts_marked_bad()? } else { store.recent_transcripts(limit)? };
        Ok((records, store.transcripts_marked_bad_in_last(24)?))
    });
    match read {
        Ok((records, flagged_today)) => {
            println!("{}", render::render_history(&records, flagged_today, &support.join("harvest")));
            0
        }
        Err(e) => {
            eprintln!("no se pudo leer el historial: {e}");
            1
        }
    }
}

/// `eva audit`: reads the database directly — no worker needed to see what
/// the gateway did.
fn audit(limit: u32) -> i32 {
    let path = support_dir().join("eva.sqlite3");
    if !path.exists() {
        println!("(todavía no hay historial: {} no existe)", path.display());
        return 0;
    }
    match eva_store::Store::open(&path).and_then(|store| store.recent_audit(limit)) {
        Ok(records) => {
            println!("{}", render::render_audit(&records));
            0
        }
        Err(e) => {
            eprintln!("no se pudo leer el historial: {e}");
            1
        }
    }
}
