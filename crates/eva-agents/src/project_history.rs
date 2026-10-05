//! Where the user has actually been working with coding agents: the folders
//! Codex ran sessions in, and the projects Claude Code knows — read from the
//! files each CLI already keeps, no process spawned. This finds the projects
//! a folder scan misses (a repo outside `~/code`, a client's folder that
//! holds several repos) and says which were used most recently.
//!
//! Only what is unambiguous is kept. Agents also run in throwaway places —
//! temp folders, the health check's own directories, their own worktrees —
//! and those are dropped, and a worktree is reported as the repository it
//! belongs to, so one project never shows up as five.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Places outside the home folder that are never a project (temp folders).
const SYSTEM_PREFIXES: &[&str] = &["/private/", "/tmp/", "/var/"];

/// Paths that are never a project: what the agents and this app create for
/// themselves.
const NOISE: &[&str] = &[
    "/tmp/",
    "/Application Support/",
    "/Library/",
    "/Documents/Codex/",
    "/.claude/worktrees/",
    "eva-doctor",
    "/scratch",
    "/node_modules/",
];

/// Every place an agent worked in that looks like a project, each with when
/// it was last used (`None` when the source says nothing about time).
pub fn recent_projects() -> Vec<(PathBuf, Option<SystemTime>)> {
    recent_projects_at(&dirs::home_dir().unwrap_or_default())
}

/// `entries` from any other source (the folders EVA's own tasks ran in) with
/// the same rules applied: temp and app-owned folders dropped, worktrees
/// turned into their repository, and places that are gone left out.
pub fn cleaned(entries: Vec<(PathBuf, Option<SystemTime>)>) -> Vec<(PathBuf, Option<SystemTime>)> {
    let home = dirs::home_dir().unwrap_or_default();
    entries.into_iter().filter_map(|(path, when)| normalize(&path, &home).map(|p| (p, when))).collect()
}

fn recent_projects_at(home: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut found: Vec<(PathBuf, Option<SystemTime>)> = Vec::new();
    let mut add = |path: PathBuf, when: Option<SystemTime>| {
        let Some(path) = normalize(&path, home) else { return };
        match found.iter_mut().find(|(p, _)| *p == path) {
            Some((_, known)) => *known = (*known).max(when),
            None => found.push((path, when)),
        }
    };

    for (cwd, when) in codex_sessions(&home.join(".codex").join("sessions")) {
        add(cwd, when);
    }
    for path in claude_projects(&home.join(".claude.json")) {
        add(path, None);
    }
    found.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    found
}

/// `path` as the project it stands for, or `None` if it is not one: gone from
/// disk, too broad (the home folder, a top-level folder), one of the
/// [`NOISE`] places, or otherwise not worth listing. A git worktree becomes
/// its repository.
fn normalize(path: &Path, home: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy();
    if NOISE.iter().any(|n| text.contains(n))
        || (!path.starts_with(home) && SYSTEM_PREFIXES.iter().any(|p| text.starts_with(p)))
    {
        return None;
    }
    let path = repository_of(path).unwrap_or_else(|| path.to_path_buf());
    if !path.is_dir() || path.components().count() <= 2 || path == home {
        return None;
    }
    Some(path)
}

/// If `path` is a linked git worktree (its `.git` is a file saying
/// `gitdir: <repo>/.git/worktrees/<name>`), the repository it belongs to.
fn repository_of(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path.join(".git")).ok()?;
    let gitdir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    let repo = gitdir.ancestors().find(|a| a.file_name().is_some_and(|n| n == ".git"))?.parent()?.to_path_buf();
    (repo != path).then_some(repo)
}

/// The working directory of each Codex session on disk (`session_meta`, the
/// first line of every rollout file) with the file's modification time.
fn codex_sessions(sessions_dir: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut out = Vec::new();
    let mut pending = vec![sessions_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                if let Some(cwd) = first_line_cwd(&path) {
                    out.push((cwd, entry.metadata().and_then(|m| m.modified()).ok()));
                }
            }
        }
    }
    out
}

fn first_line_cwd(file: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::BufReader::new(std::fs::File::open(file).ok()?).read_line(&mut line).ok()?;
    let value: serde_json::Value = serde_json::from_str(&line).ok()?;
    value.pointer("/payload/cwd")?.as_str().map(PathBuf::from)
}

/// The project paths in `~/.claude.json` (`projects`, keyed by path).
fn claude_projects(claude_json: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(claude_json) else { return Vec::new() };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    doc.get("projects")
        .and_then(|p| p.as_object())
        .map(|projects| projects.keys().map(PathBuf::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn session(home: &Path, name: &str, cwd: &Path) {
        let dir = home.join(".codex").join("sessions").join("2026").join("09");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let line = serde_json::json!({"type": "session_meta", "payload": {"cwd": cwd}}).to_string();
        std::fs::write(dir.join(format!("{name}.jsonl")), format!("{line}\n{{\"other\":1}}\n")).expect("write");
    }

    #[test]
    fn codex_and_claude_histories_become_projects_and_agree_on_one_repo() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = home.path().join("code").join("eva02");
        let client = home.path().join("code").join("BarberiaSaas");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(&client).expect("mkdir");
        session(home.path(), "a", &repo);
        session(home.path(), "b", &repo);
        session(home.path(), "c", &client);
        std::fs::write(
            home.path().join(".claude.json"),
            serde_json::json!({"projects": {repo.to_string_lossy(): {}}}).to_string(),
        )
        .expect("write");

        let mut paths: Vec<PathBuf> = recent_projects_at(home.path()).into_iter().map(|(p, _)| p).collect();
        paths.sort();
        assert_eq!(paths, vec![client, repo], "eva02 once, though three sources name it");
    }

    #[test]
    fn what_agents_create_for_themselves_and_places_that_are_gone_are_dropped() {
        let home = tempfile::tempdir().expect("tempdir");
        let real = home.path().join("code").join("novoastar");
        let doctor = home.path().join("tmp").join("eva-doctor-123");
        let chat = home.path().join("Documents").join("Codex").join("2026-09-26").join("chat");
        for dir in [&real, &doctor, &chat] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        session(home.path(), "a", &real);
        session(home.path(), "b", &doctor);
        session(home.path(), "c", &chat);
        session(home.path(), "d", &home.path().join("code").join("borrado"));
        session(home.path(), "e", home.path());

        let paths: Vec<PathBuf> = recent_projects_at(home.path()).into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec![real]);
    }

    #[test]
    fn a_linked_worktree_is_reported_as_the_repository_it_belongs_to() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = home.path().join("code").join("app");
        let worktree = home.path().join("code").join("app-feature");
        std::fs::create_dir_all(repo.join(".git").join("worktrees").join("feature")).expect("mkdir");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", repo.join(".git").join("worktrees").join("feature").display()),
        )
        .expect("write");
        session(home.path(), "a", &worktree);

        let paths: Vec<PathBuf> = recent_projects_at(home.path()).into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec![repo]);
    }

    #[test]
    fn the_apps_own_scratch_folder_is_never_a_project() {
        let home = tempfile::tempdir().expect("tempdir");
        let scratch = home.path().join("Library").join("Application Support").join("EVA01").join("scratch");
        std::fs::create_dir_all(&scratch).expect("mkdir");
        assert_eq!(normalize(&scratch, home.path()), None);
    }

    #[test]
    fn missing_or_garbled_histories_yield_nothing_and_never_panic() {
        let home = tempfile::tempdir().expect("tempdir");
        assert!(recent_projects_at(home.path()).is_empty());
        std::fs::write(home.path().join(".claude.json"), "{{{ no es json").expect("write");
        let dir = home.path().join(".codex").join("sessions");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("roto.jsonl"), "esto tampoco\n").expect("write");
        assert!(recent_projects_at(home.path()).is_empty());
    }
}
