//! A throwaway git worktree per dictated task (`docs/PLAN.md` fase 6):
//! "una orden mal entendida toca una rama desechable, no tu árbol". The
//! agent runs in a fresh checkout on its own `eva/<id>` branch; if it changed
//! nothing, the worktree and branch are removed again, and if it did, both
//! stay for the user to review and merge.
//!
//! Every failure degrades to [`Workspace::InPlace`] with the reason — a
//! project that is not a git repo, a repo with no commits yet, a full disk —
//! rather than refusing to run the task: the worktree is a safety net, and
//! its absence must never be the reason a voice command does nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

/// How long any single `git` call may take before it is abandoned.
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

/// Where a task will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Workspace {
    /// A fresh worktree on its own branch.
    Worktree(Worktree),
    /// The project directory itself — with why no worktree was made.
    InPlace {
        /// A short, human-readable reason, for the log and the panel.
        reason: String,
    },
}

/// A worktree EVA created for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The directory the agent should run in — the worktree root, or the
    /// same relative subdirectory of it when the project is a subfolder of
    /// the repository.
    pub work_dir: PathBuf,
    /// The worktree's root, what `git worktree remove` needs.
    pub root: PathBuf,
    /// The repository's own root.
    pub repo_root: PathBuf,
    /// The branch created for the task, `eva/<short id>`.
    pub branch: String,
    /// The commit the branch started from, to tell later whether the agent
    /// committed anything.
    pub base_commit: String,
    /// How many files the user had changed but not committed when the task
    /// started. The worktree starts from the last commit, so the agent does
    /// not see those changes — worth telling the user, who may have meant
    /// exactly the file they were editing.
    pub unseen_changes: usize,
}

/// Prepares where the task for `task_id` will run: a new worktree under
/// `worktrees_dir` when `project_dir` is inside a git repository with at
/// least one commit, otherwise `project_dir` itself.
pub async fn prepare(project_dir: &Path, task_id: Uuid, worktrees_dir: &Path) -> Workspace {
    match try_prepare(project_dir, task_id, worktrees_dir).await {
        Ok(worktree) => Workspace::Worktree(worktree),
        Err(reason) => Workspace::InPlace { reason },
    }
}

async fn try_prepare(project_dir: &Path, task_id: Uuid, worktrees_dir: &Path) -> Result<Worktree, String> {
    let repo_root = PathBuf::from(
        git(project_dir, &["rev-parse", "--show-toplevel"])
            .await
            .map_err(|_| "no es un repositorio git".to_string())?,
    );
    let base_commit =
        git(&repo_root, &["rev-parse", "HEAD"]).await.map_err(|_| "el repositorio aún no tiene commits".to_string())?;

    let unseen_changes = git(&repo_root, &["status", "--porcelain"]).await.map(|out| out.lines().count()).unwrap_or(0);

    let short = &task_id.simple().to_string()[..8];
    let branch = format!("eva/{short}");
    let repo_name = repo_root.file_name().map_or_else(|| "repo".into(), |n| n.to_string_lossy().into_owned());
    let root = worktrees_dir.join(format!("{repo_name}-{short}"));

    std::fs::create_dir_all(worktrees_dir).map_err(|e| format!("no se pudo crear {}: {e}", worktrees_dir.display()))?;
    git(&repo_root, &["worktree", "add", "-b", &branch, &root.to_string_lossy(), "HEAD"])
        .await
        .map_err(|e| format!("git worktree add falló: {e}"))?;

    // The project may be a subfolder of the repo (a monorepo package): the
    // agent must start in the same place inside the worktree.
    let canonical_project = project_dir.canonicalize().unwrap_or_else(|_| project_dir.to_path_buf());
    let canonical_repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.clone());
    let work_dir = match canonical_project.strip_prefix(&canonical_repo) {
        Ok(relative) if !relative.as_os_str().is_empty() => root.join(relative),
        _ => root.clone(),
    };

    Ok(Worktree { work_dir, root, repo_root, branch, base_commit, unseen_changes })
}

impl Worktree {
    /// How many files differ from where the branch started, counting
    /// uncommitted changes and anything the agent committed.
    pub async fn changed_files(&self) -> usize {
        let uncommitted = git(&self.root, &["status", "--porcelain"]).await.map(|out| out.lines().count());
        let committed =
            git(&self.root, &["diff", "--name-only", &self.base_commit, "HEAD"]).await.map(|out| out.lines().count());
        uncommitted.unwrap_or(0).max(committed.unwrap_or(0))
    }

    /// Removes the worktree and its branch if the agent changed nothing, so
    /// tasks that only answered a question leave no clutter. Returns `true`
    /// if it was removed. Never removes anything with changes in it.
    pub async fn remove_if_untouched(&self) -> bool {
        if self.changed_files().await > 0 {
            return false;
        }
        let removed =
            git(&self.repo_root, &["worktree", "remove", "--force", &self.root.to_string_lossy()]).await.is_ok();
        if removed {
            let _ = git(&self.repo_root, &["branch", "-D", &self.branch]).await;
        }
        removed
    }
}

/// Runs `git -C dir args…`, returning trimmed stdout, or stderr on failure.
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let run = Command::new("git").arg("-C").arg(dir).args(args).stdin(std::process::Stdio::null()).output();
    let output = tokio::time::timeout(GIT_TIMEOUT, run)
        .await
        .map_err(|_| format!("git {} tardó más de {GIT_TIMEOUT:?}", args.join(" ")))?
        .map_err(|e| format!("no se pudo ejecutar git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    /// A real git repository with one commit, in a temp directory.
    struct Repo {
        dir: tempfile::TempDir,
    }

    impl Repo {
        async fn new() -> Repo {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path();
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec!["config", "user.email", "eva@test"],
                vec!["config", "user.name", "EVA"],
                vec!["config", "commit.gpgsign", "false"],
            ] {
                git(path, &args).await.expect("git setup");
            }
            std::fs::create_dir_all(path.join("packages/app")).expect("mkdir");
            std::fs::write(path.join("README.md"), "hola").expect("write");
            std::fs::write(path.join("packages/app/main.rs"), "fn main() {}").expect("write");
            git(path, &["add", "-A"]).await.expect("add");
            git(path, &["commit", "-q", "-m", "inicial"]).await.expect("commit");
            Repo { dir }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    #[tokio::test]
    async fn a_repo_gets_a_worktree_on_its_own_branch_with_the_projects_files() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");
        let id = Uuid::new_v4();

        let Workspace::Worktree(wt) = prepare(repo.path(), id, worktrees.path()).await else {
            panic!("a git repo with a commit must get a worktree");
        };

        assert!(wt.branch.starts_with("eva/"));
        assert!(wt.work_dir.join("README.md").exists(), "the worktree must contain the project's files");
        assert_ne!(wt.root, repo.path(), "it must not be the user's own tree");
        let branches = git(repo.path(), &["branch", "--list", &wt.branch]).await.expect("branch");
        assert!(branches.contains(&wt.branch));
    }

    #[tokio::test]
    async fn the_users_uncommitted_changes_are_counted_because_the_agent_will_not_see_them() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");
        std::fs::write(repo.path().join("README.md"), "editado sin commit").expect("write");
        std::fs::write(repo.path().join("borrador.txt"), "nuevo sin commit").expect("write");

        let Workspace::Worktree(wt) = prepare(repo.path(), Uuid::new_v4(), worktrees.path()).await else {
            panic!("must get a worktree");
        };

        assert_eq!(wt.unseen_changes, 2);
        let readme = std::fs::read_to_string(wt.work_dir.join("README.md")).expect("read");
        assert_eq!(readme, "hola", "the worktree has the last commit, not the edit");
    }

    #[tokio::test]
    async fn a_subfolder_project_starts_in_the_same_subfolder_of_the_worktree() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");

        let Workspace::Worktree(wt) =
            prepare(&repo.path().join("packages/app"), Uuid::new_v4(), worktrees.path()).await
        else {
            panic!("must get a worktree");
        };

        assert!(wt.work_dir.ends_with("packages/app"), "got {}", wt.work_dir.display());
        assert!(wt.work_dir.join("main.rs").exists());
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_repo_runs_in_place_with_a_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktrees = tempfile::tempdir().expect("tempdir");
        let result = prepare(dir.path(), Uuid::new_v4(), worktrees.path()).await;
        assert!(matches!(result, Workspace::InPlace { reason } if reason.contains("git")));
    }

    #[tokio::test]
    async fn a_repo_without_commits_runs_in_place_instead_of_failing_the_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]).await.expect("init");
        let worktrees = tempfile::tempdir().expect("tempdir");
        let result = prepare(dir.path(), Uuid::new_v4(), worktrees.path()).await;
        assert!(matches!(result, Workspace::InPlace { reason } if reason.contains("commits")));
    }

    #[tokio::test]
    async fn an_untouched_worktree_is_removed_together_with_its_branch() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");
        let Workspace::Worktree(wt) = prepare(repo.path(), Uuid::new_v4(), worktrees.path()).await else {
            panic!("must get a worktree");
        };

        assert!(wt.remove_if_untouched().await);
        assert!(!wt.root.exists());
        assert_eq!(git(repo.path(), &["branch", "--list", &wt.branch]).await.expect("branch"), "");
    }

    #[tokio::test]
    async fn a_worktree_with_uncommitted_changes_is_kept() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");
        let Workspace::Worktree(wt) = prepare(repo.path(), Uuid::new_v4(), worktrees.path()).await else {
            panic!("must get a worktree");
        };
        std::fs::write(wt.work_dir.join("nuevo.txt"), "cambio del agente").expect("write");

        assert_eq!(wt.changed_files().await, 1);
        assert!(!wt.remove_if_untouched().await, "work the agent did must never be deleted");
        assert!(wt.root.exists());
    }

    #[tokio::test]
    async fn a_worktree_where_the_agent_committed_is_kept_too() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");
        let Workspace::Worktree(wt) = prepare(repo.path(), Uuid::new_v4(), worktrees.path()).await else {
            panic!("must get a worktree");
        };
        std::fs::write(wt.root.join("commit.txt"), "x").expect("write");
        git(&wt.root, &["add", "-A"]).await.expect("add");
        git(&wt.root, &["commit", "-q", "-m", "trabajo del agente"]).await.expect("commit");

        assert_eq!(wt.changed_files().await, 1, "a committed change still counts as work");
        assert!(!wt.remove_if_untouched().await);
    }

    #[tokio::test]
    async fn two_tasks_get_two_independent_worktrees() {
        let repo = Repo::new().await;
        let worktrees = tempfile::tempdir().expect("tempdir");
        let a = prepare(repo.path(), Uuid::new_v4(), worktrees.path()).await;
        let b = prepare(repo.path(), Uuid::new_v4(), worktrees.path()).await;
        let (Workspace::Worktree(a), Workspace::Worktree(b)) = (a, b) else { panic!("both must get worktrees") };
        assert_ne!(a.root, b.root);
        assert_ne!(a.branch, b.branch);
    }
}
