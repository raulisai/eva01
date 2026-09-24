//! The gateway socket: how an agent's `eva-mcp` child reaches this worker.
//! Bound in a private run directory with owner-only permissions, guarded by
//! a per-run token that only the agents this worker launches are given
//! (`docs/PLAN.md` fase 8: the MCP server is a local privilege surface, so
//! it holds none of its own).

use crate::context::WorkerContext;
use eva_agents::McpInjection;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::UnixListener;

/// A bound gateway socket, not yet serving.
pub struct Endpoint {
    listener: UnixListener,
    /// The socket's path.
    pub socket: PathBuf,
    /// The secret every request must carry.
    pub token: String,
}

/// Binds a fresh socket `gateway-<pid>.sock` in `run_dir` (created private,
/// mode 0700), first removing the sockets of workers that no longer exist —
/// a crashed worker never cleans up after itself.
///
/// # Errors
/// The directory could not be created or the socket could not be bound.
pub fn bind(preferred_run_dir: &Path) -> std::io::Result<Endpoint> {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let run_dir = &socket_dir(preferred_run_dir, uid, &std::env::temp_dir());
    if run_dir != preferred_run_dir {
        tracing::warn!(
            preferred = %preferred_run_dir.display(),
            using = %run_dir.display(),
            "la ruta habitual es demasiado larga para un socket Unix; uso una más corta"
        );
    }
    std::fs::create_dir_all(run_dir)?;
    std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700))?;
    remove_stale_sockets(run_dir);

    let socket = run_dir.join(format!("gateway-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    Ok(Endpoint { listener, socket, token: new_token() })
}

/// The longest path a Unix socket can have on macOS, counting the final NUL
/// (`sizeof(sockaddr_un.sun_path)`); `bind` fails beyond it.
const SOCKET_PATH_MAX: usize = 104;

/// `preferred` if a socket file inside it fits in [`SOCKET_PATH_MAX`], else a
/// short private directory under `tmp`. A home folder with a long name pushes
/// `~/Library/Application Support/EVA01/run/…` past the limit, and without
/// this the agents would silently lose EVA's tools.
fn socket_dir(preferred: &Path, uid: u32, tmp: &Path) -> PathBuf {
    // The longest name `bind` gives a socket: a pid is at most 7 digits.
    let longest = preferred.join("gateway-9999999.sock");
    if longest.as_os_str().len() < SOCKET_PATH_MAX {
        preferred.to_path_buf()
    } else {
        tmp.join(format!("eva01-{uid}"))
    }
}

/// 256 bits from the OS random source (two v4 UUIDs' worth), hex.
fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Deletes `gateway-<pid>.sock` files whose process is gone.
fn remove_stale_sockets(run_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(run_dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        if let Some(pid) = socket_pid(&entry.file_name().to_string_lossy()) {
            if !process_alive(pid) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Whether another worker is running right now — its gateway socket is there
/// and its process is alive. `preferred_run_dir` is the same directory given
/// to [`bind`].
pub fn other_live_workers(preferred_run_dir: &Path) -> bool {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let run_dir = socket_dir(preferred_run_dir, uid, &std::env::temp_dir());
    let own = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir(run_dir) else { return false };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| socket_pid(&entry.file_name().to_string_lossy()))
        .any(|pid| pid != own && process_alive(pid))
}

/// The pid in a `gateway-<pid>.sock` name.
fn socket_pid(name: &str) -> Option<i32> {
    name.strip_prefix("gateway-").and_then(|rest| rest.strip_suffix(".sock")).and_then(|pid| pid.parse().ok())
}

fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only reports whether the process
    // exists (`EPERM` means it does, owned by someone else).
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

impl Endpoint {
    /// How an agent's MCP server is launched to reach this endpoint.
    pub fn injection(&self, eva_mcp_binary: PathBuf) -> McpInjection {
        McpInjection {
            command: eva_mcp_binary,
            args: vec!["--gateway".to_string(), self.socket.to_string_lossy().into_owned()],
            env: vec![("EVA_GATEWAY_TOKEN".to_string(), self.token.clone())],
        }
    }

    /// Starts answering requests with the worker's agent-facing service. The
    /// socket file is removed when the worker shuts down normally.
    pub fn serve(self, ctx: &Arc<WorkerContext>) -> tokio::task::JoinHandle<()> {
        let service = Arc::clone(&ctx.agent_service);
        tokio::spawn(eva_mcp::serve_gateway_socket(self.listener, self.token, service))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_mcp::desktop::mock::{Call, MockDesktop};
    use eva_mcp::{DesktopService, RemoteService, ServiceError};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_normal_path_is_used_as_is_and_a_too_long_one_moves_to_a_short_private_directory() {
        let tmp = Path::new("/var/folders/ab/cdef/T");
        let normal = Path::new("/Users/djoker/Library/Application Support/EVA01/run");
        assert_eq!(socket_dir(normal, 501, tmp), normal);

        let long =
            Path::new("/Users/una-persona-con-un-nombre-de-usuario-muy-largo/Library/Application Support/EVA01/run");
        assert_eq!(socket_dir(long, 501, tmp), Path::new("/var/folders/ab/cdef/T/eva01-501"));
        let moved = socket_dir(long, 501, tmp).join("gateway-9999999.sock");
        assert!(moved.as_os_str().len() < SOCKET_PATH_MAX);
    }

    #[tokio::test]
    async fn a_socket_really_binds_from_a_home_whose_path_is_too_long() {
        let dir = tempfile::tempdir().expect("tempdir");
        let too_long = dir.path().join("x".repeat(90)).join("Library/Application Support/EVA01/run");
        assert!(too_long.join("gateway-1.sock").as_os_str().len() >= SOCKET_PATH_MAX);

        let endpoint = bind(&too_long).expect("falls back instead of failing");
        assert!(endpoint.socket.exists());
        assert!(endpoint.socket.as_os_str().len() < SOCKET_PATH_MAX);
        let _ = std::fs::remove_file(&endpoint.socket);
    }

    #[test]
    fn another_workers_socket_counts_only_while_its_process_lives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("mkdir");
        assert!(!other_live_workers(&run_dir));

        std::fs::write(run_dir.join(format!("gateway-{}.sock", std::process::id())), "").expect("own");
        assert!(!other_live_workers(&run_dir), "this worker's own socket is not another worker");

        let mut peer = std::process::Command::new("sleep").arg("30").spawn().expect("sleep");
        std::fs::write(run_dir.join(format!("gateway-{}.sock", peer.id())), "").expect("peer");
        assert!(other_live_workers(&run_dir));

        peer.kill().expect("kill");
        peer.wait().expect("wait");
        assert!(!other_live_workers(&run_dir), "a dead worker's leftover socket does not count");
    }

    #[tokio::test]
    async fn the_socket_and_its_directory_are_private_to_the_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let run_dir = dir.path().join("run");
        let endpoint = bind(&run_dir).expect("bind");

        let dir_mode = std::fs::metadata(&run_dir).expect("meta").permissions().mode() & 0o777;
        let socket_mode = std::fs::metadata(&endpoint.socket).expect("meta").permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(socket_mode, 0o600);
    }

    #[tokio::test]
    async fn every_endpoint_gets_its_own_long_random_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        drop(dir);
    }

    #[tokio::test]
    async fn a_socket_left_by_a_dead_worker_is_removed_and_a_live_ones_is_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dead = dir.path().join("gateway-999999.sock");
        std::fs::write(&dead, "").expect("write");
        let alive = dir.path().join(format!("gateway-{}.sock", std::process::id()));
        std::fs::write(&alive, "").expect("write");
        let unrelated = dir.path().join("notas.txt");
        std::fs::write(&unrelated, "").expect("write");

        remove_stale_sockets(dir.path());

        assert!(!dead.exists(), "a dead worker's socket is stale");
        assert!(alive.exists(), "this process is alive");
        assert!(unrelated.exists(), "nothing else in the directory is touched");
    }

    #[tokio::test]
    async fn the_injection_names_the_socket_and_hands_over_the_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = bind(dir.path()).expect("bind");
        let injection = endpoint.injection(PathBuf::from("/Apps/EVA01.app/Contents/MacOS/eva-mcp"));

        assert_eq!(injection.args[0], "--gateway");
        assert_eq!(injection.args[1], endpoint.socket.to_string_lossy());
        assert_eq!(injection.env, vec![("EVA_GATEWAY_TOKEN".to_string(), endpoint.token.clone())]);
    }

    /// The whole agent → worker path, as an agent's `eva-mcp` would use it:
    /// a real socket, the real token check, the worker's real gateway, the
    /// real confirmation broker answered the way the shell would answer it.
    #[tokio::test]
    async fn an_agent_tool_call_is_ruled_on_by_the_workers_gateway_and_the_overlay() {
        let mut rig = Rig::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = bind(dir.path()).expect("bind");
        let (socket, token) = (endpoint.socket.clone(), endpoint.token.clone());
        let client = RemoteService::new(&socket, &token);
        let _server = endpoint.serve(&rig.ctx);

        // Auto for an agent: opening a URL just runs.
        client.open_url("https://github.com").await.expect("allowed");
        assert_eq!(rig.desktop.calls(), vec![Call::OpenUrl("https://github.com".into())]);

        // Confirm for an agent: quitting an app asks the user on the overlay.
        let asking = tokio::spawn(async move { client.close_app("Spotify").await });
        let confirmation_id = loop {
            match rig.next_event().await {
                Some(eva_ipc::WorkerToShell::ConfirmationRequested { confirmation_id, title, .. }) => {
                    assert!(title.contains("Spotify"), "{title}");
                    break confirmation_id;
                }
                Some(_) => {}
                None => panic!("the worker never asked the user"),
            }
        };
        assert!(rig.ctx.broker.resolve(confirmation_id, true));
        asking.await.expect("no panic").expect("the user said yes");
        assert!(rig.desktop.calls().contains(&Call::CloseApp("Spotify".into())));

        // Everything landed in the audit trail with who asked.
        let audit = rig.ctx.store.recent_audit(10).expect("audit");
        assert!(audit.iter().all(|a| a.intent_json["origin"] == "agent"));
    }

    #[tokio::test]
    async fn a_client_without_the_token_gets_nothing_done() {
        let rig = Rig::builder().desktop(MockDesktop::new()).build();
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = bind(dir.path()).expect("bind");
        let impostor = RemoteService::new(&endpoint.socket, "adivinando");
        let _server = endpoint.serve(&rig.ctx);

        assert_eq!(impostor.open_app("Brave Browser").await, Err(ServiceError::Refused("token inválido".to_string())));
        assert!(rig.desktop.calls().is_empty());
    }
}
