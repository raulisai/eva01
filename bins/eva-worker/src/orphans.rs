//! Agents a dead worker left behind.
//!
//! Every agent runs in a process group of its own (so a cancel reaches the
//! shells and servers it starts), which also means it does not die with the
//! worker: measured, a `kill -9` to the worker left the agent running, with
//! no one to see it, cancel it or enforce its time limit — a real agent would
//! have kept editing its worktree and spending quota. So each running agent
//! is written down here, and the next worker to start stops whatever the
//! previous one left.
//!
//! A pid alone could have been reused by an unrelated program by then, so
//! each entry also keeps the process's start time, and nothing is signalled
//! unless both still match.

use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// How long a stopped agent gets to exit before it is killed outright.
const GRACE: Duration = Duration::from_secs(3);

/// The on-disk list of running agents: one file per task, `<task>.agent`,
/// holding the pid and its start time.
pub struct AgentLedger {
    dir: PathBuf,
}

/// One recorded agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    pid: u32,
    started: String,
}

impl AgentLedger {
    /// A ledger kept in `dir` (created on first use).
    pub fn new(dir: PathBuf) -> AgentLedger {
        AgentLedger { dir }
    }

    /// Notes that `task` is running as process `pid`.
    pub async fn record(&self, task: Uuid, pid: u32) {
        let Some(started) = start_time(pid).await else { return };
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|()| std::fs::write(self.path(task), format!("{pid}\n{started}\n")));
        if let Err(e) = written {
            tracing::warn!("no se pudo anotar el proceso del agente: {e}");
        }
    }

    /// Crosses `task` off: its agent is over.
    pub fn forget(&self, task: Uuid) {
        let _ = std::fs::remove_file(self.path(task));
    }

    /// Stops every agent a previous worker left running and clears the list.
    /// Returns how many were still alive. The final kill, for one that
    /// ignores the polite signal, happens in the background after [`GRACE`].
    pub async fn reap(&self) -> usize {
        let Ok(files) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut stopped = 0;
        for file in files.filter_map(Result::ok).map(|f| f.path()) {
            if file.extension().is_some_and(|e| e == "agent") {
                if let Some(entry) = read_entry(&file) {
                    if start_time(entry.pid).await.as_deref() == Some(entry.started.as_str()) {
                        signal_group(entry.pid, libc::SIGTERM);
                        tokio::spawn(kill_if_still_there(entry));
                        stopped += 1;
                    }
                }
                let _ = std::fs::remove_file(&file);
            }
        }
        stopped
    }

    fn path(&self, task: Uuid) -> PathBuf {
        self.dir.join(format!("{task}.agent"))
    }
}

fn read_entry(file: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let started = lines.next()?.trim().to_string();
    (!started.is_empty()).then_some(Entry { pid, started })
}

async fn kill_if_still_there(entry: Entry) {
    tokio::time::sleep(GRACE).await;
    if start_time(entry.pid).await.as_deref() == Some(entry.started.as_str()) {
        signal_group(entry.pid, libc::SIGKILL);
    }
}

/// When process `pid` started, as `ps` reports it — `None` if there is no
/// such process.
async fn start_time(pid: u32) -> Option<String> {
    let output =
        tokio::process::Command::new("ps").args(["-o", "lstart=", "-p", &pid.to_string()]).output().await.ok()?;
    let started = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !started.is_empty()).then_some(started)
}

fn signal_group(pid: u32, signal: libc::c_int) {
    // SAFETY: plain signals to a process (group) whose identity was just
    // checked against the ledger; failure (it already exited) is harmless.
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal);
        libc::kill(pid as libc::pid_t, signal);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    /// A stand-in for an agent: its own process group, like the real ones.
    fn agent() -> std::process::Child {
        std::process::Command::new("sleep").arg("60").process_group(0).spawn().expect("sleep starts")
    }

    #[tokio::test]
    async fn an_agent_left_behind_is_stopped_by_the_next_worker() {
        let dir = tempfile::tempdir().unwrap();
        let mut orphan = agent();
        AgentLedger::new(dir.path().to_path_buf()).record(Uuid::new_v4(), orphan.id()).await;

        // The worker died here. The next one starts:
        let stopped = AgentLedger::new(dir.path().to_path_buf()).reap().await;

        assert_eq!(stopped, 1);
        let status = tokio::task::spawn_blocking(move || orphan.wait()).await.unwrap().unwrap();
        assert!(!status.success(), "it was stopped by a signal, not left to finish");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "the list is cleared");
    }

    #[tokio::test]
    async fn a_pid_now_used_by_another_program_is_never_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let mut bystander = agent();
        std::fs::write(
            dir.path().join(format!("{}.agent", Uuid::new_v4())),
            format!("{}\nMon Jan  1 00:00:00 2001\n", bystander.id()),
        )
        .unwrap();

        assert_eq!(AgentLedger::new(dir.path().to_path_buf()).reap().await, 0);
        assert!(bystander.try_wait().unwrap().is_none(), "a process that is not the recorded one is left alone");
        bystander.kill().unwrap();
    }

    #[tokio::test]
    async fn a_finished_task_leaves_nothing_behind_and_a_stale_entry_is_just_removed() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = AgentLedger::new(dir.path().to_path_buf());
        let task = Uuid::new_v4();
        let mut finished = agent();
        ledger.record(task, finished.id()).await;
        ledger.forget(task);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        finished.kill().unwrap();
        let _ = finished.wait();

        std::fs::write(dir.path().join(format!("{}.agent", Uuid::new_v4())), "999999\nMon Jan  1 00:00:00 2001\n")
            .unwrap();
        assert_eq!(ledger.reap().await, 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_missing_ledger_is_nothing_to_reap() {
        assert_eq!(AgentLedger::new(PathBuf::from("/no/existe/agentes")).reap().await, 0);
    }
}
