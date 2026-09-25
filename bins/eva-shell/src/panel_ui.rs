//! The panel window: a page served on the loopback interface and opened in
//! the default browser. It is EVA01's control room — dictation history and
//! stats, dictionary, custom commands and what ran, styles, settings,
//! calibration, and the Doctor (`eva doctor` on a click, every check with its
//! fix, the logs live).
//!
//! Why a local page and not a native window: it is lists, forms and buttons —
//! HTML does that well, needs no extra dependency, and the shell stays small.
//! The data does not come from here: `eva panel <route>` (the CLI) reads and
//! writes the database and the config, and this only relays its answer, so the
//! shell never links either. What keeps it safe to leave a server listening:
//!
//! - it binds `127.0.0.1` only, on a random port;
//! - every request must carry the random token the shell generated when it
//!   opened the page (`?t=…`) and a `Host` header naming that exact address
//!   (which is what stops a web page from reaching it through DNS rebinding);
//! - the only things a click can do are the fixed actions in [`fix`] and the
//!   routes `eva panel` knows (which refuses any other name); no request ever
//!   supplies a command, a path or an argument to run.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

const PAGE: &str = include_str!("panel.html");

/// The bundle id macOS files EVA01's permissions under.
const BUNDLE_ID: &str = "dev.eva01.app";
/// The model the recommended install fetches (`eva model install`).
const RECOMMENDED_MODEL: &str = "canary-1b-flash";
/// A full doctor run with the agent smoke test is a few seconds per agent;
/// past this something is stuck and the run is cut.
/// What one panel call may take (the slowest reads a database).
const PANEL_TIMEOUT: Duration = Duration::from_secs(40);
/// A calibration is about a minute of the user talking; past this it is stuck.
const CALIBRATION_TIMEOUT: Duration = Duration::from_secs(300);
/// Teaching mode listens until the user is done; a quarter of an hour is far
/// more than anyone spends on one command.
const TEACHING_TIMEOUT: Duration = Duration::from_secs(900);
const RUN_TIMEOUT: Duration = Duration::from_secs(150);

static SERVER: OnceLock<Option<Server>> = OnceLock::new();
static RUN_LOCK: Mutex<()> = Mutex::new(());
static INSTALLING: AtomicBool = AtomicBool::new(false);
static RESTART: AtomicBool = AtomicBool::new(false);
static CALIBRATION: Mutex<Job> = Mutex::new(Job::idle());
static TEACHING: Mutex<Job> = Mutex::new(Job::idle());

/// A long-running `eva` subcommand started from the panel — a guided
/// calibration (`eva calibrate`), or teaching mode (`eva teach`) — with its
/// output so far, which the page polls, so it can be shown as it happens.
struct Job {
    lines: Vec<String>,
    running: bool,
    exit: Option<i32>,
    pid: Option<u32>,
}

impl Job {
    const fn idle() -> Job {
        Job { lines: Vec::new(), running: false, exit: None, pid: None }
    }
}

struct Server {
    port: u16,
    token: String,
}

/// Opens the panel in the default browser at `page` (`""` for the home,
/// `"doctor"`, `"config"`…), starting the local server the first time.
pub fn open(page: &str) {
    let server = SERVER.get_or_init(start);
    let Some(server) = server else {
        tracing::warn!("no se pudo iniciar el servidor local del panel");
        return;
    };
    let url = format!("http://127.0.0.1:{}/?t={}#{page}", server.port, server.token);
    if let Err(e) = Command::new("open").arg(&url).spawn() {
        tracing::warn!("no se pudo abrir el panel: {e}");
    }
}

/// Whether the Doctor page asked for EVA01 to restart (to pick up a
/// permission it just got). The event loop checks it and exits; a helper it
/// started reopens the app.
pub fn restart_requested() -> bool {
    RESTART.load(Ordering::SeqCst)
}

fn start() -> Option<Server> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let server = Server { port, token: token.clone() };
    std::thread::Builder::new()
        .name("doctor-ui".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let token = token.clone();
                std::thread::spawn(move || handle(stream, port, &token));
            }
        })
        .ok()?;
    Some(server)
}

/// The most a request body may be (a settings change is a few KB).
const MAX_BODY: usize = 256 * 1024;

struct Request {
    method: String,
    path: String,
    query: String,
    host: String,
    range: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    // The request line and headers only: no route reads a body.
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 || stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let mut first = lines.next()?.split_whitespace();
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let host = header("host").unwrap_or_default();
    let range = header("range");
    let length: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    let (path, query) = target.split_once('?').map_or((target.clone(), String::new()), |(p, q)| (p.into(), q.into()));
    Some(Request { method, path, query, host, range, body })
}

fn param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').filter_map(|pair| pair.split_once('=')).find(|(key, _)| *key == name).map(|(_, value)| value)
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    respond_bytes(stream, status, content_type, "", body.as_bytes());
}

fn respond_bytes(stream: &mut TcpStream, status: &str, content_type: &str, extra: &str, body: &[u8]) {
    let charset = if content_type.starts_with("audio/") { "" } else { "; charset=utf-8" };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}{charset}\r\nContent-Length: {}\r\n{extra}\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn handle(mut stream: TcpStream, port: u16, token: &str) {
    let Some(request) = read_request(&mut stream) else { return };
    let host_ok = request.host == format!("127.0.0.1:{port}") || request.host == format!("localhost:{port}");
    if !host_ok || param(&request.query, "t") != Some(token) {
        return respond(&mut stream, "403 Forbidden", "text/plain", "no");
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => respond(&mut stream, "200 OK", "text/html", PAGE),
        ("GET", "/api/run") => {
            let smoke = param(&request.query, "smoke") == Some("1");
            respond(&mut stream, "200 OK", "application/json", &run_doctor(smoke).to_string());
        }
        ("GET", "/api/logs") => respond(&mut stream, "200 OK", "application/json", &recent_logs().to_string()),
        ("POST", path) if path.starts_with("/api/panel/") => {
            let route = &path["/api/panel/".len()..];
            respond(&mut stream, "200 OK", "application/json", &panel_route(route, &request.body));
        }
        ("GET", path) if path.starts_with("/audio/") => {
            serve_audio(&mut stream, &path["/audio/".len()..], request.range.as_deref())
        }
        ("POST", "/api/calibrate/start") => {
            let started = job_start(&CALIBRATION, "calibrate", CALIBRATION_TIMEOUT);
            respond(&mut stream, "200 OK", "application/json", &started.to_string());
        }
        ("GET", "/api/calibrate/poll") => {
            let from = param(&request.query, "from").and_then(|v| v.parse().ok()).unwrap_or(0);
            respond(&mut stream, "200 OK", "application/json", &job_poll(&CALIBRATION, from).to_string());
        }
        ("POST", "/api/calibrate/stop") => {
            respond(&mut stream, "200 OK", "application/json", &job_stop(&CALIBRATION).to_string());
        }
        ("POST", "/api/teach/start") => {
            let started = job_start(&TEACHING, "teach", TEACHING_TIMEOUT);
            respond(&mut stream, "200 OK", "application/json", &started.to_string());
        }
        ("GET", "/api/teach/poll") => {
            let from = param(&request.query, "from").and_then(|v| v.parse().ok()).unwrap_or(0);
            respond(&mut stream, "200 OK", "application/json", &job_poll(&TEACHING, from).to_string());
        }
        ("POST", "/api/teach/stop") => {
            respond(&mut stream, "200 OK", "application/json", &job_stop(&TEACHING).to_string());
        }
        ("POST", "/api/fix") => {
            let result = fix(param(&request.query, "id").unwrap_or(""));
            respond(&mut stream, "200 OK", "application/json", &result.to_string());
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", "no"),
    }
}

// ---- the panel's data ----

/// Relays one `eva panel <route>` call: the request body in, the CLI's JSON
/// answer out. The route name is checked for shape here and for meaning in the
/// CLI, which refuses any it does not know.
fn panel_route(route: &str, body: &[u8]) -> String {
    let shape_ok =
        !route.is_empty() && route.len() <= 32 && route.chars().all(|c| c.is_ascii_lowercase() || c == '.' || c == '_');
    let failed = |message: &str| serde_json::json!({ "ok": false, "error": message }).to_string();
    if !shape_ok {
        return failed("ruta no válida");
    }
    let Ok(mut child) = Command::new(eva_binary())
        .args(["panel", route])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return failed("no se pudo ejecutar `eva`");
    };
    let pid = child.id();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body);
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(PANEL_TIMEOUT) {
        Ok(Ok(output)) if !output.stdout.is_empty() => String::from_utf8_lossy(&output.stdout).into_owned(),
        Ok(_) => failed("`eva` no respondió"),
        Err(_) => {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            failed("la operación tardó demasiado")
        }
    }
}

/// Serves `<id>.wav` from the harvest folder (the audio of a flagged
/// dictation), with `Range` support because Safari will not play a `<audio>`
/// otherwise. The name must be a UUID plus `.wav`: nothing else is reachable.
fn serve_audio(stream: &mut TcpStream, name: &str, range: Option<&str>) {
    let stem = name.strip_suffix(".wav").unwrap_or("");
    let is_uuid = uuid::Uuid::parse_str(stem).is_ok();
    // The audio of a flagged dictation (`harvest/`), or of one kept by training mode (`training/`).
    let base = dirs::home_dir().map(|h| h.join("Library/Application Support/EVA01"));
    let bytes = base.filter(|_| is_uuid).and_then(|base| {
        ["harvest", "training"]
            .iter()
            .find_map(|folder| std::fs::read(base.join(folder).join(format!("{stem}.wav"))).ok())
    });
    let Some(bytes) = bytes else { return respond(stream, "404 Not Found", "text/plain", "no") };

    let total = bytes.len();
    let requested = range.and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split_once('-')).map(|(from, to)| {
        let start: usize = from.parse().unwrap_or(0);
        let end: usize = to.parse().unwrap_or(total.saturating_sub(1));
        (start, end.min(total.saturating_sub(1)))
    });
    match requested {
        Some((start, end)) if start <= end && start < total => {
            let extra = format!("Content-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\n");
            respond_bytes(stream, "206 Partial Content", "audio/wav", &extra, &bytes[start..=end]);
        }
        _ => respond_bytes(stream, "200 OK", "audio/wav", "Accept-Ranges: bytes\r\n", &bytes),
    }
}

// ---- jobs the panel starts: guided calibration, teaching mode ----

fn job(state: &'static Mutex<Job>) -> std::sync::MutexGuard<'static, Job> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Starts `eva <subcommand>` — from here, so the microphone permission asked
/// for is EVA01's own — and collects its output line by line.
fn job_start(state: &'static Mutex<Job>, subcommand: &str, limit: Duration) -> serde_json::Value {
    let mut running = job(state);
    if running.running {
        return serde_json::json!({ "ok": false, "error": "ya hay una escucha en marcha" });
    }
    let spawned = Command::new(eva_binary())
        .arg(subcommand)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = spawned else {
        return serde_json::json!({ "ok": false, "error": format!("no se pudo ejecutar `eva {subcommand}`") });
    };
    *running = Job { lines: Vec::new(), running: true, exit: None, pid: Some(child.id()) };
    drop(running);

    let mut readers = Vec::new();
    for stream in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufRead::lines(std::io::BufReader::new(stream)).map_while(Result::ok) {
                job(state).lines.push(line);
            }
        }));
    }
    let pid = child.id();
    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait());
        });
        let status = rx.recv_timeout(limit);
        if status.is_err() {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        for reader in readers {
            let _ = reader.join();
        }
        let mut finished = job(state);
        finished.running = false;
        finished.pid = None;
        finished.exit = Some(match status {
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            _ => -1,
        });
    });
    serde_json::json!({ "ok": true })
}

/// The lines produced since line `from`, and whether it is still going.
fn job_poll(state: &'static Mutex<Job>, from: usize) -> serde_json::Value {
    let state = job(state);
    let lines: Vec<&String> = state.lines.iter().skip(from).collect();
    serde_json::json!({ "lines": lines, "next": state.lines.len(), "running": state.running, "exit": state.exit })
}

fn job_stop(state: &'static Mutex<Job>) -> serde_json::Value {
    let pid = job(state).pid;
    if let Some(pid) = pid {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    }
    serde_json::json!({ "ok": true })
}

// ---- the diagnosis ----

/// The sibling `eva` binary (the CLI that owns the checks).
fn eva_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eva-shell"));
    path.set_file_name("eva");
    path
}

/// What the shell itself can vouch for and the CLI cannot: macOS grants
/// Accessibility per executable, so only this process can say whether *it* has it.
fn accessibility_check() -> serde_json::Value {
    if eva_macos::is_accessibility_trusted() {
        serde_json::json!({
            "verdict": "ok", "name": "Permiso de Accesibilidad",
            "detail": "concedido: la tecla fn y el pegado funcionan", "hint": null, "actions": [],
        })
    } else {
        serde_json::json!({
            "verdict": "fail", "name": "Permiso de Accesibilidad",
            "detail": "EVA01 no lo tiene: sin él no responde a la tecla fn ni puede pegar el texto",
            "hint": "Si EVA01 ya sale activada en Ajustes, es la de una versión anterior (la firma cambia al \
                     recompilar): «Restablecer» la quita, la vuelves a activar y «Reiniciar EVA01» lo aplica.",
            "actions": ["reset_accessibility", "open_accessibility", "restart_app"],
        })
    }
}

fn run_doctor(smoke: bool) -> serde_json::Value {
    let Ok(_guard) = RUN_LOCK.try_lock() else {
        return serde_json::json!({ "error": "ya hay un diagnóstico en marcha" });
    };
    let started = std::time::Instant::now();
    let mut args = vec!["doctor", "--json"];
    if smoke {
        args.push("--smoke");
    }
    let output = run_with_timeout(&eva_binary(), &args, RUN_TIMEOUT);
    let Some(stdout) = output else {
        return serde_json::json!({ "error": "el diagnóstico no terminó a tiempo o `eva` no se pudo ejecutar" });
    };
    let Ok(mut report) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return serde_json::json!({ "error": "el diagnóstico devolvió algo que no se pudo leer" });
    };

    let ax = accessibility_check();
    if let Some(checks) = report["checks"].as_array_mut() {
        checks.insert(1.min(checks.len()), ax.clone());
    }
    if ax["verdict"] == "fail" {
        report["failed"] = (report["failed"].as_u64().unwrap_or(0) + 1).into();
    }
    report["seconds"] = serde_json::json!((started.elapsed().as_secs_f64() * 10.0).round() / 10.0);
    report["smoke"] = smoke.into();
    report
}

/// Runs `program` and returns its stdout, or `None` if it could not start or
/// outlived `limit` (then it is killed).
fn run_with_timeout(program: &std::path::Path, args: &[&str], limit: Duration) -> Option<String> {
    let child = Command::new(program).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        // `eva doctor` exits 1 when a check failed; the report is still valid.
        Ok(Ok(output)) => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            None
        }
    }
}

// ---- the monitor ----

fn logs_dir() -> PathBuf {
    dirs::home_dir().map_or_else(std::env::temp_dir, |h| h.join("Library/Logs/EVA01"))
}

/// The last lines of the newest shell and worker logs, most recent last, with
/// the model runtime's own chatter left out.
fn recent_logs() -> serde_json::Value {
    let mut lines: Vec<(String, String, String)> = Vec::new(); // (timestamp, source, text)
    for source in ["eva-shell", "eva-worker"] {
        let Some(newest) = newest_log(source) else { continue };
        let Ok(text) = std::fs::read_to_string(&newest) else { continue };
        let interesting = text
            .lines()
            .filter(|l| !l.contains("ort::logging") && !l.contains("transcribe_rs::onnx"))
            .collect::<Vec<_>>();
        for line in interesting.iter().rev().take(40).rev() {
            let stamp = line.split_whitespace().next().unwrap_or("").to_string();
            lines.push((stamp, source.to_string(), (*line).to_string()));
        }
    }
    lines.sort();
    let tail: Vec<_> = lines
        .iter()
        .rev()
        .take(60)
        .rev()
        .map(|(_, source, text)| {
            let level = ["ERROR", "WARN", "INFO"].iter().find(|l| text.contains(&format!(" {l} "))).unwrap_or(&"INFO");
            serde_json::json!({ "source": source, "level": level, "text": text })
        })
        .collect();
    serde_json::json!({ "lines": tail })
}

fn newest_log(prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(logs_dir())
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

// ---- the fixes ----

const ACCESSIBILITY_PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
const MICROPHONE_PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

/// Runs one of the fixed fixes the page offers. `id` is matched against a
/// closed list; nothing from the request reaches a command line.
fn fix(id: &str) -> serde_json::Value {
    let done = |message: &str| serde_json::json!({ "ok": true, "message": message });
    let failed = |message: &str| serde_json::json!({ "ok": false, "message": message });
    match id {
        "open_accessibility" => open_target(ACCESSIBILITY_PANE, "Ajustes abierto en Accesibilidad: activa EVA01."),
        "open_microphone" => open_target(MICROPHONE_PANE, "Ajustes abierto en Micrófono: activa EVA01."),
        "reset_accessibility" => {
            let reset = Command::new("tccutil").args(["reset", "Accessibility", BUNDLE_ID]).status();
            if !reset.is_ok_and(|s| s.success()) {
                return failed("macOS no dejó restablecer el permiso");
            }
            let _ = Command::new("open").arg(ACCESSIBILITY_PANE).spawn();
            done("Permiso restablecido. Actívalo en Ajustes (se abrió) y pulsa «Reiniciar EVA01».")
        }
        "restart_app" => restart_app(),
        "open_logs" => open_target(&logs_dir().to_string_lossy(), "Carpeta de registros abierta."),
        "open_config" => open_support("config.toml", "Abierto config.toml."),
        "open_commands" => open_support("commands", "Carpeta de órdenes abierta."),
        "open_harvest" => open_support("harvest", "Carpeta de audios marcados abierta."),
        "open_training" => open_support("training", "Carpeta de entrenamiento abierta."),
        "enable_startup" => {
            let ok = Command::new(eva_binary()).args(["startup", "enable"]).status().is_ok_and(|s| s.success());
            if ok {
                done("EVA01 se abrirá al iniciar sesión.")
            } else {
                failed("no se pudo activar el inicio automático")
            }
        }
        "install_model" => install_model(),
        _ => failed("acción desconocida"),
    }
}

/// Opens something inside EVA01's support folder (creating a missing folder,
/// so the button never lands on "does not exist").
fn open_support(name: &str, message: &str) -> serde_json::Value {
    let Some(base) = dirs::home_dir().map(|h| h.join("Library/Application Support/EVA01")) else {
        return serde_json::json!({ "ok": false, "message": "no se encontró la carpeta de datos" });
    };
    let path = base.join(name);
    if !path.exists() && name != "config.toml" {
        let _ = std::fs::create_dir_all(&path);
    }
    if !path.exists() {
        return serde_json::json!({ "ok": false, "message": "todavía no existe; guarda un cambio en Configuración primero" });
    }
    open_target(&path.to_string_lossy(), message)
}

fn open_target(target: &str, message: &str) -> serde_json::Value {
    match Command::new("open").arg(target).spawn() {
        Ok(_) => serde_json::json!({ "ok": true, "message": message }),
        Err(e) => serde_json::json!({ "ok": false, "message": format!("no se pudo abrir: {e}") }),
    }
}

/// Downloads the recommended voice model in the background (~940 MB).
fn install_model() -> serde_json::Value {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return serde_json::json!({ "ok": true, "message": "La descarga ya está en marcha." });
    }
    std::thread::spawn(|| {
        let ok = Command::new(eva_binary())
            .args(["model", "install", RECOMMENDED_MODEL])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        tracing::info!(ok, "instalación del modelo de voz desde Doctor terminada");
        INSTALLING.store(false, Ordering::SeqCst);
    });
    serde_json::json!({
        "ok": true,
        "message": "Descargando el modelo (~940 MB) en segundo plano. Cuando termine, ejecuta el diagnóstico otra vez.",
    })
}

/// Quits EVA01 and, a moment later, opens it again. The reopen is a detached
/// shell, so it outlives this process.
fn restart_app() -> serde_json::Value {
    let Some(bundle) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().nth(3).map(PathBuf::from)) else {
        return serde_json::json!({ "ok": false, "message": "no se encontró la app" });
    };
    if bundle.extension().is_none_or(|e| e != "app") {
        return serde_json::json!({ "ok": false, "message": "solo se puede reiniciar desde EVA01.app instalada" });
    }
    let spawned = Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 2; /usr/bin/open \"$1\"")
        .arg("eva-restart")
        .arg(&bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if spawned.is_err() {
        return serde_json::json!({ "ok": false, "message": "no se pudo programar la reapertura" });
    }
    RESTART.store(true, Ordering::SeqCst);
    serde_json::json!({ "ok": true, "message": "Reiniciando EVA01…" })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_read_by_exact_name() {
        assert_eq!(param("t=abc&smoke=1", "smoke"), Some("1"));
        assert_eq!(param("t=abc&smoke=1", "t"), Some("abc"));
        assert_eq!(param("xt=abc", "t"), None);
        assert_eq!(param("", "t"), None);
    }

    #[test]
    fn a_panel_route_must_have_the_shape_of_a_route_name() {
        for route in ["", "../../bin/sh", "History", "a b", "history;ls", &"x".repeat(40)] {
            assert!(panel_route(route, b"{}").contains("ruta no v"), "{route:?}");
        }
    }

    #[test]
    fn an_unknown_fix_does_nothing() {
        let result = fix("rm -rf /");
        assert_eq!(result["ok"], false);
        assert_eq!(fix("")["ok"], false);
    }

    /// One request against a real listener: the token and the host header are
    /// what let a request in.
    #[test]
    fn requests_without_the_token_or_with_a_foreign_host_are_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || handle(stream, port, "secret"));
            }
        });
        let get = |target: &str, host: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let own_host = format!("127.0.0.1:{port}");
        assert!(get("/?t=secret", &own_host).starts_with("HTTP/1.1 200"));
        assert!(get("/?t=wrong", &own_host).starts_with("HTTP/1.1 403"));
        assert!(get("/", &own_host).starts_with("HTTP/1.1 403"));
        assert!(get("/?t=secret", "evil.example.com").starts_with("HTTP/1.1 403"), "DNS rebinding");
        assert!(get("/nope?t=secret", &own_host).starts_with("HTTP/1.1 404"));
    }
}
