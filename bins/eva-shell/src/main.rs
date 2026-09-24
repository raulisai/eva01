//! The process the user sees: tray icon, global hotkey, overlay panel. Per
//! `docs/PLAN.md` §3.3, this binary is deliberately small and has almost
//! nothing in it that can crash — the actual work (audio, STT, agents)
//! lives in `eva-worker`, spawned and supervised by [`supervisor`]. What it
//! shows is decided by [`model::ShellModel`], a pure state machine with a
//! watchdog; this file is the glue that feeds it key presses, worker events
//! and the clock, and draws what it says.
//!
//! Runs on `tao`'s event loop because both `tray-icon` and `global-hotkey`
//! require one on the main thread on macOS (documented in their own crate
//! docs) — `tao` is used purely for that run loop, not for any window it
//! could create.

mod hotkeys;
mod icons;
mod model;
mod supervisor;
mod tray;

use eva_config::Config;
use eva_ipc::ShellToWorker;
use eva_macos::{FnKeyEvent, FnKeyMonitor, Overlay, OverlayContent};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use hotkeys::DictationKey;
use model::{Command, KeyLabels, ShellModel};
use objc2::MainThreadMarker;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use supervisor::{Supervisor, SupervisorEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tracing_subscriber::EnvFilter;
use tray::{id as menu_id, Tray};
use tray_icon::menu::MenuEvent;
use uuid::Uuid;

/// How often the loop wakes to check keys, worker events and the clock —
/// often enough that a key press feels instant, rarely enough to cost nothing.
const TICK: Duration = Duration::from_millis(40);

fn main() {
    init_logging();
    tracing::info!("eva-shell iniciando");

    let loaded = Config::load();
    let mut startup_notes = loaded.warnings.clone();
    for warning in &startup_notes {
        tracing::warn!("{warning}");
    }
    let config = loaded.config;

    let event_loop = EventLoop::new();

    // `tray-icon` and `global-hotkey` both require constructing their
    // manager on the same thread the event loop pumps on macOS — this is
    // that thread (tao's `EventLoop::new` must run on the real main thread
    // too, which `fn main` already is).
    let Ok(hotkey_manager) = GlobalHotKeyManager::new() else {
        fatal("no se pudo inicializar el gestor de atajos globales");
    };
    let Some(mtm) = MainThreadMarker::new() else { fatal("eva-shell debe ejecutarse en el hilo principal") };
    let overlay = Overlay::new(mtm);

    let mut model = ShellModel::new(KeyLabels {
        confirm: hotkeys::label(&config.hotkey.confirm),
        cancel: hotkeys::label(&config.hotkey.cancel),
    });
    let mut tray = match Tray::new(&model.tray(Instant::now())) {
        Ok(tray) => tray,
        Err(e) => fatal(&format!("no se pudo crear el ícono de bandeja: {e}")),
    };

    let (dictation, fn_events, _fn_monitor) = install_dictation_key(&config, &hotkey_manager, &mut startup_notes);
    let mut confirmation_keys = ConfirmationKeys::new(&config, &mut startup_notes);
    let flag_key = install_flag_key(&config, &hotkey_manager, &mut startup_notes);

    if !eva_macos::is_accessibility_trusted() {
        tracing::warn!("EVA01 no tiene permiso de Accesibilidad; el pegado y la tecla fn no funcionarán");
        // Shows macOS's own permission dialog (once), instead of leaving the
        // user to discover a silently-failing paste.
        let _ = eva_macos::prompt_for_accessibility();
        startup_notes.push(
            "Sin el permiso de Accesibilidad EVA01 no puede pegar lo que dictas ni oír la tecla fn: \
             Ajustes → Privacidad y seguridad → Accesibilidad → activa EVA01 y vuelve a abrirlo."
                .to_string(),
        );
    }
    if !startup_notes.is_empty() {
        notify("EVA01", &startup_notes.join("\n"));
    }

    let supervisor = Supervisor::spawn(worker_binary_path());
    let mut shown_overlay: Option<OverlayContent> = None;
    let started = Instant::now();

    event_loop.run(move |_event, _window_target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + TICK);
        let now = Instant::now();
        let mut commands = Vec::new();

        // The dictation key, however it is bound.
        while let Ok(event) = fn_events.try_recv() {
            commands.extend(match event {
                FnKeyEvent::Pressed => model.press(now, Uuid::new_v4()),
                FnKeyEvent::Released => model.release(now),
                FnKeyEvent::Combined => model.abandon_recording(),
            });
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if dictation.as_ref().is_some_and(|key| key.id() == event.id) {
                commands.extend(match event.state {
                    HotKeyState::Pressed => model.press(now, Uuid::new_v4()),
                    HotKeyState::Released => model.release(now),
                });
            } else if event.state == HotKeyState::Pressed {
                if flag_key.is_some_and(|key| key.id() == event.id) {
                    commands.push(Command::FlagLastDictation(Uuid::new_v4()));
                } else if event.id == confirmation_keys.confirm.id() {
                    commands.extend(model.confirm_key());
                } else if event.id == confirmation_keys.cancel.id() {
                    commands.extend(model.cancel_key());
                }
            }
        }

        while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            match menu_event.id.0.as_str() {
                menu_id::QUIT => {
                    tracing::info!("Salir seleccionado desde la bandeja");
                    supervisor.send(ShellToWorker::Shutdown);
                    *control_flow = ControlFlow::Exit;
                }
                menu_id::CANCEL_TASKS => commands.push(Command::CancelAllTasks),
                menu_id::FLAG_BAD => commands.push(Command::FlagLastDictation(Uuid::new_v4())),
                menu_id::OPEN_CONFIG => open_config(),
                menu_id::OPEN_LOGS => open_path(&logs_dir()),
                _ => {}
            }
        }

        while let Some(event) = supervisor.try_recv() {
            match event {
                SupervisorEvent::WorkerRestarting { attempt } => {
                    tracing::warn!(attempt, "eva-worker se está reiniciando");
                    model.worker_restarting();
                }
                SupervisorEvent::WorkerEvent(worker_event) => {
                    log_worker_event(&worker_event);
                    commands.extend(model.worker_event(now, worker_event));
                }
            }
        }

        commands.extend(model.tick(now));
        for notification in model.take_notifications() {
            notify(&notification.title, &notification.body);
        }

        for command in commands {
            supervisor.send(to_wire(command));
        }
        confirmation_keys.sync(&hotkey_manager, model.wants_confirmation_keys(), model.wants_cancel_key());

        let content = model.overlay();
        if content != shown_overlay {
            match &content {
                Some(content) => overlay.show(content),
                None => overlay.hide(),
            }
            shown_overlay = content;
        }
        if shown_overlay.is_some() {
            overlay.animate(started.elapsed().as_secs_f64());
        }
        tray.update(&model.tray(now));
    });
}

/// Sets up the dictation key: `fn` through the flags monitor, or a
/// combination through `global-hotkey`. A setting that does not parse falls
/// back to `⌘⇧Space` and says so. Returns the combination (if that is what
/// is bound), the channel the `fn` monitor sends on, and the monitor itself,
/// which must stay alive.
fn install_dictation_key(
    config: &Config,
    manager: &GlobalHotKeyManager,
    notes: &mut Vec<String>,
) -> (Option<HotKey>, mpsc::Receiver<FnKeyEvent>, Option<FnKeyMonitor>) {
    let (fn_tx, fn_rx) = mpsc::channel();
    let fallback = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space);

    let key = hotkeys::parse_dictation(&config.hotkey.dictation).unwrap_or_else(|e| {
        notes.push(format!("{e}; uso ⌘⇧Space."));
        DictationKey::Combo(fallback)
    });

    match key {
        DictationKey::Fn => match FnKeyMonitor::start(move |event| {
            let _ = fn_tx.send(event);
        }) {
            Some(monitor) => {
                tracing::info!("tecla de dictado: fn (mantenida)");
                (None, fn_rx, Some(monitor))
            }
            None => {
                notes.push("macOS no dejó escuchar la tecla fn; uso ⌘⇧Space.".to_string());
                (register_or_note(manager, fallback, "de dictado", notes), fn_rx, None)
            }
        },
        DictationKey::Combo(hotkey) => {
            tracing::info!(key = %config.hotkey.dictation, "tecla de dictado");
            (register_or_note(manager, hotkey, "de dictado", notes), fn_rx, None)
        }
    }
}

/// The "this came out wrong" key. Optional: a bad combination or a taken one
/// costs the shortcut only, since the tray item does the same thing.
fn install_flag_key(config: &Config, manager: &GlobalHotKeyManager, notes: &mut Vec<String>) -> Option<HotKey> {
    match hotkeys::parse_combo(&config.hotkey.flag_bad) {
        Ok(hotkey) => register_or_note(manager, hotkey, "para marcar un dictado", notes),
        Err(e) => {
            notes.push(format!("hotkey.flag_bad: {e}"));
            None
        }
    }
}

fn register_or_note(
    manager: &GlobalHotKeyManager,
    hotkey: HotKey,
    purpose: &str,
    notes: &mut Vec<String>,
) -> Option<HotKey> {
    match manager.register(hotkey) {
        Ok(()) => Some(hotkey),
        Err(e) => {
            notes.push(format!("no se pudo registrar la tecla {purpose}: {e}"));
            None
        }
    }
}

/// The yes/no keys, registered only while they mean something: EVA01 never
/// holds a system-wide ⌘⏎ or ⌘⎋ it does not need at that moment.
struct ConfirmationKeys {
    confirm: HotKey,
    cancel: HotKey,
    confirm_on: bool,
    cancel_on: bool,
}

impl ConfirmationKeys {
    fn new(config: &Config, notes: &mut Vec<String>) -> ConfirmationKeys {
        let mut parse = |spec: &str, name: &str, fallback: HotKey| {
            hotkeys::parse_combo(spec).unwrap_or_else(|e| {
                notes.push(format!("hotkey.{name}: {e}"));
                fallback
            })
        };
        ConfirmationKeys {
            confirm: parse(&config.hotkey.confirm, "confirm", HotKey::new(Some(Modifiers::SUPER), Code::Enter)),
            cancel: parse(&config.hotkey.cancel, "cancel", HotKey::new(Some(Modifiers::SUPER), Code::Escape)),
            confirm_on: false,
            cancel_on: false,
        }
    }

    /// Registers or releases each key to match what the model wants now.
    fn sync(&mut self, manager: &GlobalHotKeyManager, wants_confirm: bool, wants_cancel: bool) {
        Self::toggle(manager, self.confirm, &mut self.confirm_on, wants_confirm);
        Self::toggle(manager, self.cancel, &mut self.cancel_on, wants_cancel);
    }

    fn toggle(manager: &GlobalHotKeyManager, key: HotKey, is_on: &mut bool, wanted: bool) {
        if wanted == *is_on {
            return;
        }
        let result = if wanted { manager.register(key) } else { manager.unregister(key) };
        match result {
            Ok(()) => *is_on = wanted,
            Err(e) => {
                tracing::warn!(
                    "no se pudo {} el atajo de confirmación: {e}",
                    if wanted { "registrar" } else { "liberar" }
                );
                // Do not retry every tick: pretend it is in the wanted state.
                *is_on = wanted;
            }
        }
    }
}

fn to_wire(command: Command) -> ShellToWorker {
    match command {
        Command::StartRecording(request_id) => ShellToWorker::StartRecording { request_id },
        Command::StopRecording(request_id) => ShellToWorker::StopRecording { request_id },
        Command::Cancel(request_id) => ShellToWorker::Cancel { request_id },
        Command::Confirm { id, approved } => ShellToWorker::ConfirmationResponse { confirmation_id: id, approved },
        Command::CancelAllTasks => ShellToWorker::CancelAllTasks,
        Command::ListTasks(request_id) => ShellToWorker::ListTasks { request_id },
        Command::FlagLastDictation(request_id) => ShellToWorker::FlagLastDictation { request_id },
        Command::CheckHealth(request_id) => ShellToWorker::HealthCheck { request_id },
    }
}

fn log_worker_event(event: &eva_ipc::WorkerToShell) {
    use eva_ipc::WorkerToShell as W;
    match event {
        W::Ready => tracing::info!("eva-worker listo"),
        W::Transcript { cleaned, .. } => tracing::info!(%cleaned, "transcript listo"),
        W::IntentRecognized { intent_json, .. } => tracing::info!(%intent_json, "intent reconocido"),
        W::Error { message, recoverable, .. } => tracing::warn!(%message, recoverable, "error del worker"),
        W::TaskStarted { provider, prompt, .. } => tracing::info!(%provider, %prompt, "tarea de agente iniciada"),
        W::TaskFinished { success, summary, .. } => tracing::info!(success, %summary, "tarea de agente terminada"),
        W::ConfirmationRequested { title, .. } => tracing::info!(%title, "el worker pide confirmación"),
        W::Health { report, .. } => tracing::info!(?report, "reporte de salud"),
        // Everything else is bookkeeping the model already reflects.
        _ => {}
    }
}

/// The identity EVA01's notifications go out under — the packaged app's
/// bundle identifier (`packaging/Info.plist`).
/// Shows a system notification, never on the event loop's thread (see
/// `eva_macos::notification` for what used to freeze it).
fn notify(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = eva_macos::notification::show(&title, &body) {
            tracing::warn!("{e}");
        }
    });
}

fn logs_dir() -> std::path::PathBuf {
    dirs::home_dir().map_or_else(std::env::temp_dir, |h| h.join("Library/Logs/EVA01"))
}

/// Opens the config file in the default editor, creating the commented
/// template first if there is none — so "Abrir configuración…" always lands
/// on something to edit.
fn open_config() {
    let path = Config::default_path();
    if let Err(e) = Config::ensure_file(&path) {
        tracing::warn!("no se pudo crear {}: {e}", path.display());
        return;
    }
    open_path(&path);
}

fn open_path(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("open").arg(path).spawn() {
        tracing::warn!("no se pudo abrir {}: {e}", path.display());
    }
}

fn worker_binary_path() -> std::path::PathBuf {
    // Both binaries are built into the same target directory, so the
    // worker sits right next to `eva-shell` — this holds for `cargo build`
    // and for a packaged app bundle laid out the same way.
    let mut path = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("eva-shell"));
    path.set_file_name("eva-worker");
    path
}

/// Reports a startup failure the shell cannot run without, then exits. Not a
/// panic: a plain message and a non-zero status, which is what a launcher
/// can show.
fn fatal(message: &str) -> ! {
    tracing::error!("{message}");
    eprintln!("eva-shell: {message}");
    std::process::exit(1);
}

fn init_logging() {
    let log_dir = logs_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "eva-shell.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    std::mem::forget(guard); // must outlive `main`; see the identical note in eva-worker's main.rs

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();
}
