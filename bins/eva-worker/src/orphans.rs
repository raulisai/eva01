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
//!
//! And "left behind" means *by a worker that is gone*: `eva doctor` and the
//! other CLI commands start a worker of their own while the app's is
//! running, and that one must not stop the app's agents. So each entry names
//! the worker that launched it (its pid and start time too), and an agent is
//! only stopped when that worker no longer exists.

use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// How long a stopped agent gets to exit before it is killed outright.
const GRACE: Duration = Duration::from_secs(3);

/// The on-disk list of running agents: one file per task, `<task>.agent`,
/// holding the agent's pid and start time, then its worker's.
pub struct AgentLedger {
    dir: PathBuf,
    owner: Process,
}

/// A process, identified beyond its (reusable) pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pid: u32,
    started: String,
}

impl Process {
    /// This worker. Asks `ps` once, synchronously: it is called while the
    /// worker is being built.
    pub fn current() -> Process {
        let pid = std::process::id();
        let started = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default();
        Process { pid, started }
    }

    /// Whether this very process is still running.
    async fn alive(&self) -> bool {
        !self.started.is_empty() && start_time(self.pid).await.as_deref() == Some(self.started.as_str())
    }
}

/// One recorded agent and the worker that launched it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    agent: Process,
    owner: Process,
}

impl AgentLedger {
    /// A ledger kept in `dir` (created on first use), for agents launched by `owner`.
    pub fn new(dir: PathBuf, owner: Process) -> AgentLedger {
        AgentLedger { dir, owner }
    }

    /// Notes that `task` is running as process `pid`.
    pub async fn record(&self, task: Uuid, pid: u32) {
        let Some(started) = start_time(pid).await else { return };
        let owner = &self.owner;
        let written = std::fs::create_dir_all(&self.dir).and_then(|()| {
            std::fs::write(self.path(task), format!("{pid}\n{started}\n{}\n{}\n", owner.pid, owner.started))
        });
        if let Err(e) = written {
            tracing::warn!("no se pudo anotar el proceso del agente: {e}");
        }
    }

    /// Crosses `task` off: its agent is over.
    pub fn forget(&self, task: Uuid) {
        let _ = std::fs::remove_file(self.path(task));
    }

    /// Stops every agent whose worker is gone and crosses it off. Agents of
    /// a worker still running are left alone. Returns how many were stopped.
    /// The final kill, for one that ignores the polite signal, happens in
    /// the background after [`GRACE`].
    pub async fn reap(&self) -> usize {
        let Ok(files) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut stopped = 0;
        for file in files.filter_map(Result::ok).map(|f| f.path()) {
            if file.extension().is_none_or(|e| e != "agent") {
                continue;
            }
            let Some(entry) = read_entry(&file) else {
                let _ = std::fs::remove_file(&file);
                continue;
            };
            if entry.owner.alive().await {
                continue; // its worker is running and still in charge of it
            }
            if entry.agent.alive().await {
                signal_group(entry.agent.pid, libc::SIGTERM);
                tokio::spawn(kill_if_still_there(entry.agent));
                stopped += 1;
            }
            let _ = std::fs::remove_file(&file);
        }
        stopped
    }

    fn path(&self, task: Uuid) -> PathBuf {
        self.dir.join(format!("{task}.agent"))
    }
}

fn read_entry(file: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines().map(str::trim);
    let mut process = || -> Option<Process> {
        let pid = lines.next()?.parse().ok()?;
        let started = lines.next()?.to_string();
        (!started.is_empty()).then_some(Process { pid, started })
    };
    Some(Entry { agent: process()?, owner: process()? })
}

async fn kill_if_still_there(agent: Process) {
    tokio::time::sleep(GRACE).await;
    if agent.alive().await {
        signal_group(agent.pid, libc::SIGKILL);
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

    /// A stand-in for an agent (or a worker): its own process group, like the real ones.
    fn process() -> std::process::Child {
        std::process::Command::new("sleep").arg("60").process_group(0).spawn().expect("sleep starts")
    }

    async fn identity(child: &std::process::Child) -> Process {
        Process { pid: child.id(), started: start_time(child.id()).await.expect("running") }
    }

    fn stopped_by_a_signal(mut child: std::process::Child) -> bool {
        !child.wait().expect("waits").success()
    }

    #[tokio::test]
    async fn an_agent_whose_worker_died_is_stopped_by_the_next_worker() {
        let dir = tempfile::tempdir().unwrap();
        let mut dead_worker = process();
        let owner = identity(&dead_worker).await;
        let orphan = process();
        AgentLedger::new(dir.path().to_path_buf(), owner).record(Uuid::new_v4(), orphan.id()).await;
        dead_worker.kill().unwrap();
        dead_worker.wait().unwrap();

        let stopped = AgentLedger::new(dir.path().to_path_buf(), Process::current()).reap().await;

        assert_eq!(stopped, 1);
        assert!(tokio::task::spawn_blocking(move || stopped_by_a_signal(orphan)).await.unwrap());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "the list is cleared");
    }

    #[tokio::test]
    async fn the_agents_of_a_worker_that_is_still_running_are_left_alone() {
        // `eva doctor` starts a worker of its own while the app's is busy.
        let dir = tempfile::tempdir().unwrap();
        let mut app_worker = process();
        let mut its_agent = process();
        AgentLedger::new(dir.path().to_path_buf(), identity(&app_worker).await)
            .record(Uuid::new_v4(), its_agent.id())
            .await;

        let stopped = AgentLedger::new(dir.path().to_path_buf(), Process::current()).reap().await;

        assert_eq!(stopped, 0);
        assert!(its_agent.try_wait().unwrap().is_none(), "the app's task keeps running");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "and stays on the list");
        for child in [&mut its_agent, &mut app_worker] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }

    #[tokio::test]
    async fn a_pid_now_used_by_another_program_is_never_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let mut bystander = process();
        let entry = format!("{}\nMon Jan  1 00:00:00 2001\n999999\nMon Jan  1 00:00:00 2001\n", bystander.id());
        std::fs::write(dir.path().join(format!("{}.agent", Uuid::new_v4())), entry).unwrap();

        assert_eq!(AgentLedger::new(dir.path().to_path_buf(), Process::current()).reap().await, 0);
        assert!(bystander.try_wait().unwrap().is_none(), "a process that is not the recorded one is left alone");
        bystander.kill().unwrap();
        bystander.wait().unwrap();
    }

    #[tokio::test]
    async fn a_finished_task_leaves_nothing_behind_and_a_malformed_entry_is_just_removed() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = AgentLedger::new(dir.path().to_path_buf(), Process::current());
        let task = Uuid::new_v4();
        let mut finished = process();
        ledger.record(task, finished.id()).await;
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        ledger.forget(task);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        finished.kill().unwrap();
        finished.wait().unwrap();

        std::fs::write(dir.path().join(format!("{}.agent", Uuid::new_v4())), "no es una entrada").unwrap();
        assert_eq!(ledger.reap().await, 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_missing_ledger_is_nothing_to_reap() {
        let ledger = AgentLedger::new(PathBuf::from("/no/existe/agentes"), Process::current());
        assert_eq!(ledger.reap().await, 0);
    }
}
