//! `eva doctor`: is this Mac ready to run EVA01, and if not, what exactly to
//! do about it. Every line is a check with a verdict and — when it is not
//! fine — the command or the setting that fixes it. `--smoke` goes further
//! and makes each agent actually do something tiny, because "installed and
//! logged in" is exactly what a broken agent also looks like (found for real:
//! a Codex whose configured model its own outdated CLI rejects passes
//! `codex login status` and fails every task).

use crate::session;
use eva_agents::{AgentOutcome, AgentTask, ProviderStatus};
use eva_config::{models, support_dir, Config};
use eva_ipc::{ShellToWorker, WorkerToShell};
use std::time::Duration;
use uuid::Uuid;

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Fine.
    Ok,
    /// Works, but not as well as it could, or a feature will be missing.
    Warn,
    /// EVA01 will not do its job until this is fixed.
    Fail,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// How it went.
    pub verdict: Verdict,
    /// What was checked.
    pub name: &'static str,
    /// What was found.
    pub detail: String,
    /// What to do about it, when it is not fine.
    pub hint: Option<String>,
    /// A fix the shell's Doctor window can offer as a button (an id it knows,
    /// e.g. `install_model`), when the problem has one.
    pub action: Option<&'static str>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Check {
        Check { verdict: Verdict::Ok, name, detail: detail.into(), hint: None, action: None }
    }

    fn warn(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Check {
        Check { verdict: Verdict::Warn, name, detail: detail.into(), hint: Some(hint.into()), action: None }
    }

    fn fail(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Check {
        Check { verdict: Verdict::Fail, name, detail: detail.into(), hint: Some(hint.into()), action: None }
    }

    fn with_action(mut self, action: &'static str) -> Check {
        self.action = Some(action);
        self
    }

    /// The check as the Doctor window reads it.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "verdict": match self.verdict {
                Verdict::Ok => "ok",
                Verdict::Warn => "warn",
                Verdict::Fail => "fail",
            },
            "name": self.name,
            "detail": self.detail,
            "hint": self.hint,
            "action": self.action,
        })
    }

    /// The report line(s) for this check.
    pub fn render(&self) -> String {
        let mark = match self.verdict {
            Verdict::Ok => "✓",
            Verdict::Warn => "⚠",
            Verdict::Fail => "✗",
        };
        let mut out = format!("{mark} {:<22} {}", self.name, self.detail);
        if let Some(hint) = &self.hint {
            out.push_str(&format!("\n  {:<22} → {hint}", ""));
        }
        out
    }
}

/// Every check, in report order. `smoke` also makes each agent answer for real.
async fn collect(smoke: bool) -> Vec<Check> {
    let loaded = Config::load();
    let support = support_dir();
    let mut checks = Vec::new();

    checks.push(system());
    checks.push(apple_intelligence());
    checks.push(config_file(&loaded));
    checks.extend(stt_model(&loaded.config, &support));
    checks.push(microphone(eva_audio::MicrophoneSource::default_input()));
    checks.push(fn_key(&loaded.config));
    checks.push(git(&loaded.config));
    checks.push(mcp_binary());
    checks.push(login_item());
    checks.extend(agents(&loaded.config, smoke).await);
    checks.extend(worker().await);
    checks
}

/// Runs every check, prints the report, and returns the exit code: `1` if any
/// check failed outright, else `0`. With `json` the report is one JSON object
/// (what the shell's Doctor window reads) instead of text.
pub async fn run(smoke: bool, json: bool) -> i32 {
    let checks = collect(smoke).await;
    let failed = checks.iter().filter(|c| c.verdict == Verdict::Fail).count();
    let warned = checks.iter().filter(|c| c.verdict == Verdict::Warn).count();

    if json {
        let checks: Vec<_> = checks.iter().map(Check::to_json).collect();
        println!("{}", serde_json::json!({ "checks": checks, "failed": failed, "warned": warned }));
        return i32::from(failed > 0);
    }

    println!("EVA01 — diagnóstico\n");
    for check in &checks {
        println!("{}", check.render());
    }
    println!("\nRegistros: ~/Library/Logs/EVA01 · datos y modelos: {}", support_dir().display());
    println!("{}", summary(failed, warned));
    i32::from(failed > 0)
}

/// The last line of the report.
pub fn summary(failed: usize, warned: usize) -> String {
    match (failed, warned) {
        (0, 0) => "\nTodo en orden.".to_string(),
        (0, w) => format!("\nFunciona, con {w} aviso(s)."),
        (f, _) => format!("\n{f} problema(s) que impiden usar EVA01 como debe."),
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn system() -> Check {
    let version = command_output("sw_vers", &["-productVersion"]).unwrap_or_else(|| "desconocida".to_string());
    let arch = std::env::consts::ARCH;
    let major: u32 = version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if major >= 26 {
        Check::ok("Sistema", format!("macOS {version} ({arch})"))
    } else {
        Check::warn(
            "Sistema",
            format!("macOS {version} ({arch})"),
            "el formateador con Apple Intelligence necesita macOS 26 o más reciente; sin él EVA01 usa solo reglas",
        )
    }
}

fn apple_intelligence() -> Check {
    if eva_text::AppleIntelligenceFormatter::new().is_some() {
        Check::ok("Apple Intelligence", "disponible: los dictados se formatean con contexto, en el equipo")
    } else {
        Check::warn(
            "Apple Intelligence",
            "no disponible: los dictados se formatean solo con reglas",
            "Ajustes del Sistema → Apple Intelligence y Siri → actívalo (requiere Apple Silicon y macOS 26)",
        )
    }
}

fn config_file(loaded: &eva_config::Loaded) -> Check {
    if !loaded.warnings.is_empty() {
        return Check::warn(
            "Configuración",
            format!(
                "{} tiene {} problema(s): {}",
                loaded.path.display(),
                loaded.warnings.len(),
                loaded.warnings.join("; ")
            ),
            "corrígelos con `eva config edit` y reinicia EVA01",
        );
    }
    if loaded.path.exists() {
        Check::ok("Configuración", loaded.path.display().to_string())
    } else {
        Check::ok("Configuración", "sin archivo: todos los valores por defecto (`eva config edit` crea uno con ayuda)")
    }
}

/// Whether the small model is there to double-check short commands
/// (`eva_audio::second_opinion`), which the big one alone mostly mishears.
fn second_opinion(config: &Config, support: &std::path::Path) -> Check {
    let Some(small) = models::find("canary-180m-flash") else {
        return Check::ok("Órdenes cortas", "sin segunda opinión");
    };
    if !config.stt.second_opinion {
        return Check::ok("Órdenes cortas", "segunda opinión apagada en la configuración ([stt] second_opinion)");
    }
    if small.is_installed(&models::model_dir(support, small.id)) {
        Check::ok("Órdenes cortas", "canary-180m-flash vuelve a oír las frases cortas")
    } else {
        Check::warn(
            "Órdenes cortas",
            "el modelo grande entiende mal muchas órdenes cortas («Adán, abre Brave»)",
            format!(
                "eva model install {}   (~{} MB): medido, las órdenes reconocidas pasaron de 4/22 a 19/22",
                small.id,
                small.total_bytes() / 1_000_000
            ),
        )
    }
}

fn stt_model(config: &Config, support: &std::path::Path) -> Vec<Check> {
    let recommended = crate::model::recommended();
    let install_hint =
        format!("eva model install {}   (~{} MB)", recommended.id, recommended.total_bytes() / 1_000_000);
    match models::discover(config, support, &|key| std::env::var(key).ok()) {
        models::ModelChoice::Canary(dir) => {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name == recommended.id {
                vec![Check::ok("Modelo de voz", format!("Canary — {}", dir.display())), second_opinion(config, support)]
            } else {
                vec![Check::warn(
                    "Modelo de voz",
                    format!("Canary — {}", dir.display()),
                    format!(
                        "el modelo pequeño pierde la ñ («mañana» sale «ma ana») y falla más con el español real; \
                         instala el recomendado: {install_hint}"
                    ),
                )]
            }
        }
        models::ModelChoice::Whisper(path) => vec![Check::warn(
            "Modelo de voz",
            format!("Whisper — {}", path.display()),
            format!("Canary entiende mejor el español: {install_hint}"),
        )],
        models::ModelChoice::None => {
            vec![Check::fail("Modelo de voz", "no hay ninguno instalado: no se puede dictar", install_hint)
                .with_action("install_model")]
        }
    }
}

/// The default input device. It is only described, never opened: the check
/// must not ask for the microphone permission on its own.
fn microphone(input: Result<eva_audio::InputInfo, eva_audio::AudioError>) -> Check {
    match input {
        Ok(info) => Check::ok(
            "Micrófono",
            format!(
                "{} ({} Hz, {} canal(es)); el permiso lo pide macOS al primer dictado",
                info.name, info.sample_rate, info.channels
            ),
        ),
        Err(eva_audio::AudioError::NoInputDevice) => Check::fail(
            "Micrófono",
            "macOS no ve ningún micrófono",
            "conecta uno o elige la entrada en Ajustes del Sistema → Sonido → Entrada",
        ),
        Err(e) => Check::warn("Micrófono", e.to_string(), "revisa Ajustes del Sistema → Sonido → Entrada"),
    }
}

fn fn_key(config: &Config) -> Check {
    if !matches!(config.hotkey.dictation.trim().to_lowercase().as_str(), "fn" | "globe" | "🌐") {
        return Check::ok("Tecla de dictado", format!("{} (combinación)", config.hotkey.dictation));
    }
    // 0 = "No hacer nada", 1 = cambiar de fuente de entrada, 2 = emojis, 3 = dictado.
    match command_output("defaults", &["read", "com.apple.HIToolbox", "AppleFnUsageType"]).as_deref() {
        Some("0") => Check::ok("Tecla de dictado", "fn (mantenida); macOS no hace nada más con ella"),
        Some(_) => Check::warn(
            "Tecla de dictado",
            "fn (mantenida), pero macOS también reacciona a esa tecla",
            "Ajustes del Sistema → Teclado → «Pulsar la tecla 🌐 para» → «No hacer nada»",
        ),
        None => Check::ok("Tecla de dictado", "fn (mantenida); macOS no tiene una acción configurada para ella"),
    }
}

fn git(config: &Config) -> Check {
    match command_output("git", &["--version"]) {
        Some(version) => Check::ok("git", version),
        None if config.agents.worktree => Check::warn(
            "git",
            "no está instalado",
            "sin git las tareas de agente corren directamente en tu proyecto (xcode-select --install)",
        ),
        None => Check::ok("git", "no está instalado, y agents.worktree está apagado"),
    }
}

fn mcp_binary() -> Check {
    let path = session::sibling_binary("eva-mcp");
    if path.is_file() {
        Check::ok(
            "Herramientas MCP",
            format!("{} (los agentes pueden abrir apps, pedirte confirmación…)", path.display()),
        )
    } else {
        Check::warn(
            "Herramientas MCP",
            format!("no encuentro eva-mcp junto a eva ({})", path.display()),
            "los agentes correrán sin las herramientas de EVA; usa el `eva` de dentro de EVA01.app o `cargo build --workspace`",
        )
    }
}

fn login_item() -> Check {
    if crate::startup::is_enabled() {
        Check::ok("Inicio automático", "EVA01 se abre al iniciar sesión")
    } else {
        Check::ok("Inicio automático", "apagado (`eva startup enable` para abrirlo al iniciar sesión)")
    }
}

async fn agents(config: &Config, smoke: bool) -> Vec<Check> {
    let registry = eva_agents::registry_with_priority(&config.agents.priority);
    let mut checks = Vec::new();

    for (id, status) in registry.detect_all().await {
        let name = agent_check_name(id);
        match status {
            ProviderStatus::NotInstalled => checks.push(Check::warn(
                name,
                "no instalado",
                format!("instala su CLI si quieres usar «{}»", pretty(id)),
            )),
            ProviderStatus::InstalledNoSession { .. } => checks.push(Check::warn(
                name,
                "instalado, pero sin sesión iniciada",
                if id == "codex" { "codex login" } else { "claude auth login" },
            )),
            ProviderStatus::Active { version } if !smoke => {
                checks.push(Check::ok(
                    name,
                    format!("listo ({version}) — `eva doctor --smoke` prueba que de verdad responda"),
                ));
            }
            ProviderStatus::Active { version } => checks.push(smoke_test(&registry, id, &version, name).await),
        }
    }
    if checks.iter().all(|c| c.verdict != Verdict::Ok) {
        checks.push(Check::fail(
            "Agentes",
            "ninguno está listo: no se pueden despachar tareas por voz",
            "instala y entra en Codex o Claude Code (los dictados normales funcionan igual)",
        ));
    }
    checks
}

fn agent_check_name(id: &str) -> &'static str {
    match id {
        "codex" => "Agente Codex",
        "claude_code" => "Agente Claude Code",
        _ => "Agente",
    }
}

fn pretty(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude_code" => "Claude Code",
        other => other,
    }
}

/// Makes the agent answer one word, in an empty temporary folder.
async fn smoke_test(registry: &eva_agents::AgentRegistry, id: &str, version: &str, name: &'static str) -> Check {
    let Ok(provider) = registry.select(Some(id)).await else {
        return Check::warn(name, "dejó de estar disponible durante la prueba", "vuelve a correr `eva doctor --smoke`");
    };
    let Ok(dir) = tempdir() else {
        return Check::warn(name, "no pude crear una carpeta temporal para la prueba", "revisa el espacio en disco");
    };
    let task = AgentTask {
        prompt: "Responde únicamente con la palabra: listo".to_string(),
        project_dir: dir.clone(),
        session_id: Uuid::new_v4(),
        resume_session_id: None,
        mcp: None,
    };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::time::Instant::now();
    let running = match provider.execute(&task, tx).await {
        Ok(running) => running,
        Err(e) => {
            return finish_smoke(&dir, Check::fail(name, format!("no arrancó: {e}"), "revisa la instalación del CLI"))
        }
    };
    let outcome = running.wait_or_cancel(tokio::time::sleep(Duration::from_secs(120))).await;
    let secs = started.elapsed().as_secs_f32();

    let check = match outcome {
        AgentOutcome::Completed { summary } => Check::ok(
            name,
            format!("listo ({version}) — respondió en {secs:.1} s: «{}»", summary.unwrap_or_default().trim()),
        ),
        AgentOutcome::Failed { message } => {
            let (problem, fix) = explain_agent_failure(id, &message);
            Check::fail(
                name,
                format!("instalado y con sesión, pero falla al usarlo: {problem}"),
                format!("{fix} (mientras tanto EVA01 lo salta solo si falla antes de hacer algo)"),
            )
        }
        AgentOutcome::Cancelled => Check::warn(name, "no respondió en dos minutos", "revisa tu conexión"),
    };
    finish_smoke(&dir, check)
}

/// What went wrong with an agent, in one line, and what to do about it.
/// The CLIs report their failures as anything from a plain sentence to a raw
/// JSON error body, and the useful part is the message inside it.
fn explain_agent_failure(id: &str, message: &str) -> (String, String) {
    let first_line = message.lines().next().unwrap_or_default();
    let readable = serde_json::from_str::<serde_json::Value>(first_line)
        .ok()
        .and_then(|json| {
            json.pointer("/error/message")
                .or_else(|| json.pointer("/message"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| first_line.to_string());

    let lowered = readable.to_lowercase();
    let login = if id == "codex" { "codex login" } else { "claude auth login" };
    let update = if id == "codex" { "codex update" } else { "claude update" };
    let fix = if ["authenticate", "oauth", "not logged in", "log in", "expired", "unauthorized"]
        .iter()
        .any(|w| lowered.contains(w))
    {
        format!("su sesión caducó: `{login}`")
    } else if ["newer version", "upgrade", "update", "not supported"].iter().any(|w| lowered.contains(w)) {
        format!(
            "su CLI o su modelo configurado no van con esa cuenta: `{update}` o revisa el modelo en su configuración"
        )
    } else {
        format!(
            "míralo con `{}` a mano para ver el error completo",
            if id == "codex" { "codex exec hola" } else { "claude -p hola" }
        )
    };
    (readable, fix)
}

fn finish_smoke(dir: &std::path::Path, check: Check) -> Check {
    let _ = std::fs::remove_dir_all(dir);
    check
}

fn tempdir() -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("eva-doctor-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Asks a real worker for its health: the model it loaded, its formatter,
/// the gateway socket agents will use.
async fn worker() -> Vec<Check> {
    let request_id = Uuid::new_v4();
    let outcome =
        session::collect(ShellToWorker::HealthCheck { request_id }, request_id, Duration::from_secs(90), |_| {}).await;

    let Some(report) = outcome.events.into_iter().find_map(|e| match e {
        WorkerToShell::Health { report, .. } => Some(report),
        _ => None,
    }) else {
        return vec![Check::fail(
            "eva-worker",
            "no arrancó o no respondió",
            "mira ~/Library/Logs/EVA01/eva-worker.log; si el binario falta: cargo build --workspace",
        )];
    };

    let mut checks = vec![Check::ok(
        "eva-worker",
        format!("arranca y responde; base de datos {}", if report.store_ok { "en orden" } else { "CON PROBLEMAS" }),
    )];
    checks.push(match &report.stt_model_id {
        Some(id) => Check::ok("Modelo de voz cargado", id.clone()),
        None => Check::fail(
            "Modelo de voz cargado",
            "el worker no cargó ningún modelo",
            "instala uno (arriba) o mira el registro del worker por el motivo",
        ),
    });
    checks.push(match &report.gateway_socket {
        Some(socket) => Check::ok("Gateway para agentes", socket.clone()),
        None => Check::warn(
            "Gateway para agentes",
            "sin socket: los agentes correrán sin las herramientas de EVA",
            "mira el registro del worker",
        ),
    });
    checks.push(Check::ok(
        "Proyectos conocidos",
        format!("{} repositorio(s) en agents.project_roots", report.project_count),
    ));
    if !report.formatter.starts_with("apple_intelligence") {
        checks.push(Check::warn("Formateador en uso", report.formatter.clone(), "ver «Apple Intelligence» arriba"));
    }
    checks
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_microphone_check_names_the_device_or_says_there_is_none() {
        let fine = microphone(Ok(eva_audio::InputInfo {
            name: "MacBook Pro Microphone".into(),
            sample_rate: 48_000,
            channels: 1,
        }));
        assert_eq!(fine.verdict, Verdict::Ok);
        assert!(fine.detail.contains("MacBook Pro Microphone") && fine.detail.contains("48000 Hz"));

        let missing = microphone(Err(eva_audio::AudioError::NoInputDevice));
        assert_eq!(missing.verdict, Verdict::Fail);
        assert!(missing.hint.is_some_and(|h| h.contains("Sonido")));

        let odd = microphone(Err(eva_audio::AudioError::ConfigFailed("x".into())));
        assert_eq!(odd.verdict, Verdict::Warn);
    }

    #[test]
    fn a_fine_check_is_one_line_and_a_problem_adds_what_to_do() {
        let fine = Check::ok("Modelo de voz", "Canary").render();
        assert_eq!(fine, "✓ Modelo de voz          Canary");

        let bad = Check::fail("Modelo de voz", "no hay ninguno", "eva model install canary-1b-flash").render();
        assert!(bad.starts_with("✗ Modelo de voz"));
        assert!(bad.contains("\n") && bad.ends_with("→ eva model install canary-1b-flash"));
    }

    #[test]
    fn warnings_are_marked_differently_from_failures() {
        assert!(Check::warn("x", "y", "z").render().starts_with('⚠'));
        assert!(Check::fail("x", "y", "z").render().starts_with('✗'));
    }

    #[test]
    fn the_summary_says_whether_it_can_be_used() {
        assert_eq!(summary(0, 0), "\nTodo en orden.");
        assert!(summary(0, 2).contains("2 aviso"));
        assert!(summary(1, 0).contains("1 problema"));
    }

    #[test]
    fn a_config_with_problems_is_a_warning_naming_them() {
        let loaded = eva_config::Loaded {
            config: Config::default(),
            warnings: vec!["agents.priority: no conozco el agente \"x\"".into()],
            path: std::path::PathBuf::from("/tmp/config.toml"),
        };
        let check = config_file(&loaded);
        assert_eq!(check.verdict, Verdict::Warn);
        assert!(check.detail.contains("no conozco el agente"));
    }

    #[test]
    fn a_missing_config_file_is_fine() {
        let loaded = eva_config::Loaded {
            config: Config::default(),
            warnings: vec![],
            path: std::path::PathBuf::from("/definitivamente/no/existe/config.toml"),
        };
        assert_eq!(config_file(&loaded).verdict, Verdict::Ok);
    }

    #[test]
    fn no_model_at_all_is_a_failure_that_says_how_to_install_one() {
        let support = tempfile::tempdir().expect("tempdir");
        let checks = stt_model(&Config::default(), support.path());
        assert_eq!(checks[0].verdict, Verdict::Fail);
        assert!(checks[0].hint.as_deref().unwrap_or_default().contains("eva model install canary-1b-flash"));
    }

    #[test]
    fn a_non_recommended_model_works_but_is_flagged() {
        let support = tempfile::tempdir().expect("tempdir");
        let dir = models::model_dir(support.path(), "canary-180m-flash");
        std::fs::create_dir_all(&dir).expect("mkdir");
        for name in ["encoder-model.int8.onnx", "decoder-model.int8.onnx", "vocab.txt", "nemo128.onnx"] {
            std::fs::write(dir.join(name), "x").expect("write");
        }
        assert_eq!(stt_model(&Config::default(), support.path())[0].verdict, Verdict::Warn);
    }

    #[test]
    fn a_combination_hotkey_needs_no_fn_settings() {
        let mut config = Config::default();
        config.hotkey.dictation = "cmd+shift+space".to_string();
        assert_eq!(fn_key(&config).verdict, Verdict::Ok);
    }

    #[test]
    fn a_raw_json_error_body_is_reduced_to_its_message_and_an_outdated_cli_says_to_update() {
        let raw = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The 'gpt-5.6-terra' model requires a newer version of Codex. Please upgrade to the latest app or CLI and try again."}}"#;
        let (problem, fix) = explain_agent_failure("codex", raw);
        assert!(problem.starts_with("The 'gpt-5.6-terra' model requires a newer version"), "{problem}");
        assert!(fix.contains("codex update"), "{fix}");
    }

    #[test]
    fn an_expired_session_says_to_log_in_again_with_the_right_command() {
        let (_, fix) = explain_agent_failure(
            "claude_code",
            "Failed to authenticate: OAuth session expired and could not be refreshed",
        );
        assert!(fix.contains("claude auth login"), "{fix}");
        let (_, fix) = explain_agent_failure("codex", "not logged in");
        assert!(fix.contains("codex login"), "{fix}");
    }

    #[test]
    fn an_unknown_failure_points_at_running_the_agent_by_hand() {
        let (problem, fix) = explain_agent_failure("claude_code", "algo raro");
        assert_eq!(problem, "algo raro");
        assert!(fix.contains("claude -p hola"), "{fix}");
    }

    #[test]
    fn agent_names_are_stable_for_the_report() {
        assert_eq!(agent_check_name("codex"), "Agente Codex");
        assert_eq!(agent_check_name("claude_code"), "Agente Claude Code");
        assert_eq!(pretty("claude_code"), "Claude Code");
    }
}
