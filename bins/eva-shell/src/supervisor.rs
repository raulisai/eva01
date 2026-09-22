//! Spawns `eva-worker` as a child process, forwards commands to its stdin
//! and events from its stdout, and restarts it with exponential backoff if
//! it dies — the concrete implementation of `docs/PLAN.md` §3.3 point 1:
//! a crash in the worker (an ONNX panic, a hung FFI call) never takes the
//! tray icon or hotkey down with it.
//!
//! Deliberately built on `std::thread`/`std::sync::mpsc`, not `tokio` —
//! `eva-shell` has no other async work, and `tao`'s event loop (which
//! everything else in this binary runs on) is synchronous anyway, so a
//! second runtime would be pure overhead for one blocking read loop.

use eva_ipc::{ShellToWorker, WorkerToShell};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Something the supervisor reports to `eva-shell`'s main (tao) thread.
#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    /// A line eva-worker sent, decoded.
    WorkerEvent(WorkerToShell),
    /// eva-worker exited and a restart is being attempted; `attempt` counts
    /// consecutive restarts since the last time it ran successfully for at
    /// least [`MIN_HEALTHY_UPTIME`].
    WorkerRestarting {
        /// How many consecutive restarts have happened, including this one.
        attempt: u32,
    },
}

/// A running-or-restarting worker, driven from a background thread.
pub struct Supervisor {
    to_worker: Sender<ShellToWorker>,
    from_worker: Receiver<SupervisorEvent>,
    /// Set the moment [`Supervisor::send`] is asked to send
    /// [`ShellToWorker::Shutdown`], and checked by the loop after the
    /// current child exits. Without this, a clean `exit(0)` the worker
    /// happened to produce on its own (not because it was asked to shut
    /// down) would be indistinguishable from a deliberate shutdown and the
    /// loop would stop restarting it — exactly the silent-death failure
    /// mode `docs/PLAN.md` §3.3 exists to prevent.
    shutdown_requested: Arc<AtomicBool>,
}

/// How long the worker must stay up before a subsequent crash resets the
/// backoff counter back to the fastest retry — otherwise a worker that
/// crashes once every ten minutes for an unrelated reason would eventually
/// be waiting a full minute between restarts for no reason.
const MIN_HEALTHY_UPTIME: Duration = Duration::from_secs(30);

/// The backoff schedule: 1s, 2s, 4s, 8s, 16s, capped at 30s.
fn backoff_for_attempt(attempt: u32) -> Duration {
    let capped_exponent = attempt.min(5);
    Duration::from_secs(1u64 << capped_exponent).min(Duration::from_secs(30))
}

impl Supervisor {
    /// Spawns the background thread that owns the worker's lifecycle.
    /// `worker_binary` is the path to the `eva-worker` executable.
    pub fn spawn(worker_binary: PathBuf) -> Self {
        let (to_worker_tx, to_worker_rx) = std::sync::mpsc::channel();
        let (from_worker_tx, from_worker_rx) = std::sync::mpsc::channel();
        let shutdown_requested = Arc::new(AtomicBool::new(false));

        let shutdown_flag = Arc::clone(&shutdown_requested);
        std::thread::spawn(move || supervisor_loop(worker_binary, to_worker_rx, from_worker_tx, shutdown_flag));

        Supervisor { to_worker: to_worker_tx, from_worker: from_worker_rx, shutdown_requested }
    }

    /// Queues a command to send to the worker. Silently dropped if the
    /// supervisor thread has ended (which only happens if it panicked —
    /// forbidden by this workspace's lint policy — so this is a defensive
    /// no-op, not an expected path).
    pub fn send(&self, command: ShellToWorker) {
        if matches!(command, ShellToWorker::Shutdown) {
            self.shutdown_requested.store(true, Ordering::SeqCst);
        }
        let _ = self.to_worker.send(command);
    }

    /// Non-blocking poll for the next event, for use inside `tao`'s
    /// `ControlFlow::Poll` loop.
    pub fn try_recv(&self) -> Option<SupervisorEvent> {
        self.from_worker.try_recv().ok()
    }
}

fn supervisor_loop(
    binary: PathBuf,
    to_worker_rx: Receiver<ShellToWorker>,
    from_worker_tx: Sender<SupervisorEvent>,
    shutdown_requested: Arc<AtomicBool>,
) {
    let current_stdin: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));

    // One writer thread lives for the supervisor's entire lifetime, across
    // every worker restart — it always writes to whatever `current_stdin`
    // currently points at (or drops the command if the worker is between
    // restarts), rather than needing to be re-spawned every time the loop
    // below restarts the child process.
    spawn_writer_thread(to_worker_rx, Arc::clone(&current_stdin));

    let mut attempt: u32 = 0;

    loop {
        let spawn_result = Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // eva-worker's own tracing goes to its log file, not here; inherited stderr is just a dev-time convenience
            .spawn();

        let mut child = match spawn_result {
            Ok(child) => child,
            Err(e) => {
                tracing::error!("no se pudo iniciar eva-worker: {e}");
                report_restart_and_wait(&from_worker_tx, &mut attempt);
                continue;
            }
        };

        let started_at = std::time::Instant::now();
        install_stdin(&current_stdin, child.stdin.take());
        let reader_handle = child.stdout.take().map(|stdout| spawn_reader_thread(stdout, from_worker_tx.clone()));

        let exit_status = child.wait();
        if let Some(handle) = reader_handle {
            let _ = handle.join();
        }
        install_stdin(&current_stdin, None);

        match exit_status {
            Ok(status) => tracing::info!("eva-worker terminó con {status}"),
            Err(e) => tracing::warn!("no se pudo esperar a eva-worker: {e}"),
        }

        if shutdown_requested.load(Ordering::SeqCst) {
            tracing::info!("apagado solicitado; no se reinicia eva-worker");
            return;
        }

        if started_at.elapsed() >= MIN_HEALTHY_UPTIME {
            attempt = 0;
        }
        report_restart_and_wait(&from_worker_tx, &mut attempt);
    }
}

fn install_stdin(current_stdin: &Arc<Mutex<Option<ChildStdin>>>, stdin: Option<ChildStdin>) {
    #[allow(clippy::unwrap_used)] // only poisoned if the writer thread panicked, forbidden by workspace policy
    let mut guard = current_stdin.lock().unwrap();
    *guard = stdin;
}

fn spawn_writer_thread(to_worker_rx: Receiver<ShellToWorker>, current_stdin: Arc<Mutex<Option<ChildStdin>>>) {
    std::thread::spawn(move || {
        for command in to_worker_rx {
            let Ok(line) = eva_ipc::encode_line(&command) else {
                tracing::error!("no se pudo codificar un comando para eva-worker");
                continue;
            };
            #[allow(clippy::unwrap_used)] // only poisoned if this same thread panicked, forbidden by workspace policy
            let mut guard = current_stdin.lock().unwrap();
            if let Some(stdin) = guard.as_mut() {
                if stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()).is_err() {
                    tracing::warn!("no se pudo escribir en eva-worker (¿está reiniciando?)");
                }
            }
        }
    });
}

fn spawn_reader_thread(
    stdout: std::process::ChildStdout,
    from_worker_tx: Sender<SupervisorEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            match eva_ipc::decode_line::<WorkerToShell>(&line) {
                Ok(event) => {
                    if from_worker_tx.send(SupervisorEvent::WorkerEvent(event)).is_err() {
                        break; // the main thread went away; nothing more to do
                    }
                }
                Err(e) => tracing::warn!("línea no reconocida de eva-worker, se ignora: {e}"),
            }
        }
    })
}

fn report_restart_and_wait(from_worker_tx: &Sender<SupervisorEvent>, attempt: &mut u32) {
    *attempt += 1;
    let _ = from_worker_tx.send(SupervisorEvent::WorkerRestarting { attempt: *attempt });
    std::thread::sleep(backoff_for_attempt(*attempt));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        assert_eq!(backoff_for_attempt(0), Duration::from_secs(1));
        assert_eq!(backoff_for_attempt(1), Duration::from_secs(2));
        assert_eq!(backoff_for_attempt(2), Duration::from_secs(4));
        assert_eq!(backoff_for_attempt(3), Duration::from_secs(8));
        assert_eq!(backoff_for_attempt(4), Duration::from_secs(16));
        assert_eq!(backoff_for_attempt(5), Duration::from_secs(30), "hits the 30s cap");
        assert_eq!(backoff_for_attempt(50), Duration::from_secs(30), "stays capped for large attempt counts");
    }

    #[test]
    fn spawning_a_nonexistent_binary_reports_a_restart_instead_of_panicking() {
        let supervisor = Supervisor::spawn(PathBuf::from("/no/existe/eva-worker-de-prueba"));
        let mut saw_restart = false;
        for _ in 0..50 {
            if let Some(SupervisorEvent::WorkerRestarting { .. }) = supervisor.try_recv() {
                saw_restart = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_restart, "a missing binary must be reported as a restart attempt, not silently hang");
    }

    #[test]
    fn an_unprompted_clean_exit_is_still_restarted_not_mistaken_for_shutdown() {
        // `/usr/bin/true` exits(0) instantly, every time, forever — the
        // worst case for the bug this test guards against: without the
        // `shutdown_requested` flag, a clean exit the loop never asked for
        // would look identical to a deliberate shutdown and the loop would
        // stop restarting it, which is exactly the silent-death mode
        // `docs/PLAN.md` §3.3 exists to prevent.
        let supervisor = Supervisor::spawn(PathBuf::from("/usr/bin/true"));

        // Backoff grows 2s, 4s, 8s… after the first (near-instant) restart,
        // so the window to reliably observe a second one needs real margin.
        let mut restarts = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while restarts < 2 && std::time::Instant::now() < deadline {
            if let Some(SupervisorEvent::WorkerRestarting { .. }) = supervisor.try_recv() {
                restarts += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(restarts >= 2, "an unprompted clean exit must be restarted, not treated as a deliberate shutdown");
    }

    #[test]
    fn sending_shutdown_eventually_stops_the_restart_loop() {
        let supervisor = Supervisor::spawn(PathBuf::from("/usr/bin/true"));

        // Let it restart at least once un-shut-down, to prove the loop was
        // actually running before asking it to stop.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Some(SupervisorEvent::WorkerRestarting { .. }) = supervisor.try_recv() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        supervisor.send(ShellToWorker::Shutdown);

        // The loop checks `shutdown_requested` right after the *current*
        // child exits, and `/usr/bin/true` exits near-instantly, so it
        // should observe the flag and return within a couple of backoff
        // cycles at most — well under 3s even at this schedule's slowest.
        std::thread::sleep(Duration::from_secs(3));

        // Drain anything that arrived during that window, then confirm a
        // further quiet period produces nothing more — the loop returned.
        while supervisor.try_recv().is_some() {}
        std::thread::sleep(Duration::from_millis(500));
        assert!(supervisor.try_recv().is_none(), "restarts must have stopped after Shutdown was sent");
    }
}
