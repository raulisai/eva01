//! The process the user sees: tray icon, global hotkey, overlay panel. Per
//! `docs/PLAN.md` §3.3, this binary is deliberately small and has almost
//! nothing in it that can crash — the actual work (audio, STT, agents)
//! lives in `eva-worker`, spawned and supervised by [`supervisor`].
//!
//! Runs on `tao`'s event loop because both `tray-icon` and `global-hotkey`
//! require one on the main thread on macOS (documented in their own crate
//! docs) — `tao` is used purely for that run loop, not for any window it
//! could create.

mod supervisor;

use eva_ipc::{ShellToWorker, WorkerState as IpcWorkerState, WorkerToShell};
use eva_macos::{Overlay, OverlayState};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2::MainThreadMarker;
use supervisor::{Supervisor, SupervisorEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

fn main() {
    init_logging();
    tracing::info!("eva-shell iniciando");

    let event_loop = EventLoop::new();

    // `tray-icon` and `global-hotkey` both require constructing their
    // manager on the same thread the event loop pumps on macOS — this is
    // that thread (tao's `EventLoop::new` must run on the real main thread
    // too, which `fn main` already is).
    let _tray_icon = build_tray_icon();
    let hotkey_manager = GlobalHotKeyManager::new().expect("no se pudo inicializar el gestor de atajos globales");
    let hotkey = register_hotkey(&hotkey_manager);

    let mtm = MainThreadMarker::new().expect("eva-shell debe ejecutarse en el hilo principal");
    let overlay = Overlay::new(mtm);

    warn_if_accessibility_not_trusted();

    let supervisor = Supervisor::spawn(worker_binary_path());

    let mut recording_request_id: Option<Uuid> = None;

    event_loop.run(move |_event, _window_target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(50));

        if let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == hotkey.id() {
                match event.state {
                    HotKeyState::Pressed if recording_request_id.is_none() => {
                        let request_id = Uuid::new_v4();
                        recording_request_id = Some(request_id);
                        overlay.set_state(OverlayState::Listening);
                        supervisor.send(ShellToWorker::StartRecording { request_id });
                    }
                    HotKeyState::Released => {
                        if let Some(request_id) = recording_request_id.take() {
                            overlay.set_state(OverlayState::Thinking);
                            supervisor.send(ShellToWorker::StopRecording { request_id });
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            if menu_event.id.0 == QUIT_MENU_ID {
                tracing::info!("Salir seleccionado desde la bandeja");
                supervisor.send(ShellToWorker::Shutdown);
                *control_flow = ControlFlow::Exit;
            }
        }

        while let Some(event) = supervisor.try_recv() {
            handle_supervisor_event(&overlay, event);
        }
    });
}

/// Reflects a [`SupervisorEvent`] onto the overlay. Exhaustively matched —
/// per `docs/PLAN.md` §3.3 point 4, an unhandled state is a compile error,
/// not a silently-stuck overlay.
fn handle_supervisor_event(overlay: &Overlay, event: SupervisorEvent) {
    match event {
        SupervisorEvent::WorkerRestarting { attempt } => {
            tracing::warn!(attempt, "eva-worker se está reiniciando");
            overlay.set_state(OverlayState::Failed);
        }
        SupervisorEvent::WorkerEvent(worker_event) => handle_worker_event(overlay, worker_event),
    }
}

fn handle_worker_event(overlay: &Overlay, event: WorkerToShell) {
    match event {
        WorkerToShell::Ready => tracing::info!("eva-worker listo"),
        WorkerToShell::StateChanged { state, .. } => overlay.set_state(map_state(state)),
        WorkerToShell::Transcript { cleaned, .. } => {
            // `eva-worker` already pastes the cleaned text itself, via
            // `eva-mcp::Desktop::insert_text` (`docs/PLAN.md` fase 3) —
            // this event is purely informational here, for logging/a future
            // history view, not something `eva-shell` needs to act on.
            tracing::info!(%cleaned, "transcript listo");
        }
        WorkerToShell::IntentRecognized { intent_json, .. } => {
            tracing::info!(%intent_json, "intent reconocido");
        }
        WorkerToShell::AgentEvent { event_json, .. } => {
            tracing::info!(%event_json, "evento de agente");
        }
        WorkerToShell::Error { message, recoverable, .. } => {
            tracing::warn!(%message, recoverable, "error del worker");
            overlay.set_state(OverlayState::Failed);
        }
        WorkerToShell::Health { report, .. } => {
            tracing::info!(?report, "reporte de salud");
        }
        WorkerToShell::CustomWords { words, .. } => {
            tracing::info!(count = words.len(), "diccionario personal actualizado");
        }
        WorkerToShell::Ack { .. } => {
            tracing::info!("comando confirmado por eva-worker");
        }
    }
}

fn map_state(state: IpcWorkerState) -> OverlayState {
    match state {
        IpcWorkerState::Idle => OverlayState::Idle,
        IpcWorkerState::Listening => OverlayState::Listening,
        IpcWorkerState::Thinking => OverlayState::Thinking,
        IpcWorkerState::Executing => OverlayState::Executing,
        IpcWorkerState::Done(true) => OverlayState::Done,
        IpcWorkerState::Done(false) => OverlayState::Failed,
    }
}

const QUIT_MENU_ID: &str = "quit";

fn build_tray_icon() -> tray_icon::TrayIcon {
    let menu = Menu::new();
    let quit_item = MenuItem::with_id(QUIT_MENU_ID, "Salir", true, None);
    let _ = menu.append(&quit_item);

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("EVA01")
        .with_icon(placeholder_icon())
        .build()
        .expect("no se pudo crear el ícono de bandeja")
}

/// A small solid-color square, generated in memory — no bundled asset file
/// needed for this increment. A real icon is cosmetic polish, tracked
/// separately from the functional pieces this session focused on.
fn placeholder_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x2E, 0xA0, 0x43, 0xFF]); // opaque green
    }
    #[allow(clippy::expect_used)] // a fixed-size, hand-built buffer always matches its own dimensions
    Icon::from_rgba(rgba, SIZE, SIZE).expect("el ícono generado en memoria siempre tiene dimensiones válidas")
}

/// Registers the dictation hotkey. `docs/PLAN.md` §10 decision 5 calls for
/// holding `fn`, but `global-hotkey` binds named key combinations, not a
/// hold-vs-tap gesture on a single modifier key — supporting that specific
/// gesture needs a lower-level key-tap tracker (`docs/PLAN.md` mentions
/// `handy-keys` for exactly this) which is a follow-up, not built here.
/// Cmd+Shift+Space is used for this increment: a real, working
/// press/release-gated hotkey that proves the whole chain (registration →
/// event → `StartRecording`/`StopRecording`), on a combination
/// `global-hotkey` is documented to support unambiguously.
fn register_hotkey(manager: &GlobalHotKeyManager) -> HotKey {
    let hotkey = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space);
    manager.register(hotkey).expect("no se pudo registrar el atajo global");
    hotkey
}

fn worker_binary_path() -> std::path::PathBuf {
    // Both binaries are built into the same target directory, so the
    // worker sits right next to `eva-shell` — this holds for `cargo build`
    // and for a packaged app bundle laid out the same way.
    let mut path = std::env::current_exe().expect("no se pudo determinar la ruta de este ejecutable");
    path.set_file_name("eva-worker");
    path
}

/// Checks Accessibility trust and, if it is missing, shows a real system
/// notification with exact instructions — closing the gap
/// `packaging/build-app.sh` documents: `eva-macos::paste`'s synthesized
/// Cmd+V needs this permission, but posting a `CGEvent` never triggers the
/// system's own request dialog on its own, so a silent failure to paste is
/// otherwise the only symptom the user would ever see.
fn warn_if_accessibility_not_trusted() {
    if eva_macos::is_accessibility_trusted() {
        tracing::info!("permiso de Accesibilidad concedido");
        return;
    }

    tracing::warn!("EVA01 no tiene permiso de Accesibilidad; el pegado por voz no funcionará");
    let notification = notify_rust::Notification::new()
        .summary("EVA01 necesita Accesibilidad")
        .body("Sin este permiso, EVA01 no puede pegar lo que dictas. Ajustes → Privacidad y seguridad → Accesibilidad → activa EVA01.")
        .show();
    if let Err(e) = notification {
        tracing::warn!("no se pudo mostrar la notificación de Accesibilidad: {e}");
    }
}

fn init_logging() {
    let log_dir = dirs::home_dir().map(|h| h.join("Library/Logs/EVA01")).unwrap_or_else(std::env::temp_dir);
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
