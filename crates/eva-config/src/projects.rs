//! Which project is a voice task about (`docs/PLAN.md` fase 6: "proyecto
//! activo por contexto"). EVA knows the git repositories directly inside the
//! configured project roots, and picks the one the user is looking at by
//! matching its folder name against the focused window's title — which works
//! across VS Code ("main.rs — eva01 — Visual Studio Code"), Xcode, JetBrains
//! IDEs, Terminal and iTerm ("user@mac: ~/code/eva01") and most terminals,
//! without a per-app integration for each.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A project directory EVA knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// The folder's name.
    pub name: String,
    /// Its absolute path.
    pub path: PathBuf,
}

/// The projects found under the configured roots.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectIndex {
    projects: Vec<Project>,
    /// Paths in the order they were last worked in, most recent first — from
    /// what the agents' own histories say (see [`ProjectIndex::with_history`]).
    recent: Vec<PathBuf>,
}

/// Why [`ProjectIndex::resolve_active`] chose the project it did — shown by
/// `eva doctor` and logged, so "why did it run there?" always has an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The user named it out loud: «en BarberiaSaas, agrega tests».
    Named,
    /// The focused window's title names this project.
    FromWindowTitle,
    /// The configured `agents.default_project`.
    Default,
    /// The project a task last ran in.
    MostRecent,
    /// Nothing else applied; the worker's own working directory.
    WorkingDirectory,
}

/// Whether `path` is far too broad to be where an agent works: the root of
/// the disk, a top-level folder (`/Users`, `/tmp`) or the home folder. A task
/// run there, with a write sandbox rooted at it, could touch anything — as
/// happened when a task started from the app (whose working directory is
/// `/`) with no project in view tried to write to `/app.txt`.
pub fn is_too_broad(path: &Path) -> bool {
    path.components().count() <= 2 || dirs::home_dir().is_some_and(|home| path == home)
}

impl ProjectIndex {
    /// An index over these projects (names must be unique enough to match).
    pub fn new(projects: Vec<Project>) -> ProjectIndex {
        ProjectIndex { projects, recent: Vec::new() }
    }

    /// Scans each root for git repositories: the immediate subdirectories, and
    /// — when a subdirectory is not itself a repository — the repositories
    /// one level inside it (`~/code/BarberiaSaas/barberias-saas`, a client's
    /// folder holding its repos). A root that does not exist is skipped, not
    /// an error — the default roots (`~/code`, `~/Developer`, `~/projects`)
    /// are guesses about where a particular Mac keeps things.
    pub fn scan(roots: &[PathBuf]) -> ProjectIndex {
        let mut projects = Vec::new();
        let mut add = |path: PathBuf| {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                projects.push(Project { name: name.to_string(), path });
            }
        };
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.join(".git").exists() {
                    add(path);
                } else if path.is_dir() && !hidden(&path) {
                    let Ok(inner) = std::fs::read_dir(&path) else { continue };
                    for child in inner.filter_map(Result::ok).map(|c| c.path()) {
                        if child.join(".git").exists() && !hidden(&child) {
                            add(child);
                        }
                    }
                }
            }
        }
        projects.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
        projects.dedup_by(|a, b| a.path == b.path);
        ProjectIndex { projects, recent: Vec::new() }
    }

    /// [`ProjectIndex::scan`] of `roots`, then [`ProjectIndex::with_history`],
    /// never counting a root itself (`~/code`, a folder *of* projects) as one.
    pub fn scan_with_history(roots: &[PathBuf], history: Vec<(PathBuf, Option<SystemTime>)>) -> ProjectIndex {
        let history = history.into_iter().filter(|(path, _)| !roots.contains(path)).collect();
        ProjectIndex::scan(roots).with_history(history)
    }

    /// Adds the places work actually happened — what Codex and Claude Code
    /// remember, and what EVA itself ran tasks in — as projects, and records
    /// which were used most recently. A place already in the index only gains
    /// its recency; a folder that no longer exists is dropped.
    #[must_use]
    pub fn with_history(mut self, history: Vec<(PathBuf, Option<SystemTime>)>) -> ProjectIndex {
        let mut dated: Vec<(PathBuf, Option<SystemTime>)> = Vec::new();
        for (path, when) in history {
            if !path.is_dir() || is_too_broad(&path) {
                continue;
            }
            match dated.iter_mut().find(|(p, _)| *p == path) {
                Some((_, known)) => *known = (*known).max(when),
                None => dated.push((path, when)),
            }
        }
        for (path, _) in &dated {
            if !self.projects.iter().any(|p| p.path == *path) {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    self.projects.push(Project { name: name.to_string(), path: path.clone() });
                }
            }
        }
        self.projects
            .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
        dated.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        self.recent = dated.into_iter().map(|(p, _)| p).collect();
        self
    }

    /// The projects most recently worked in first, then the rest by name.
    pub fn by_recency(&self) -> Vec<&Project> {
        let mut ordered: Vec<&Project> =
            self.recent.iter().filter_map(|path| self.projects.iter().find(|p| p.path == *path)).collect();
        for project in &self.projects {
            if !ordered.iter().any(|p| p.path == project.path) {
                ordered.push(project);
            }
        }
        ordered
    }

    /// Every known project, sorted by name.
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    /// The project whose name appears as a whole word in `window_title`.
    /// When several match, the longest name wins ("eva01-web" over "eva").
    pub fn find_in_title(&self, window_title: &str) -> Option<&Project> {
        let title = window_title.to_lowercase();
        self.projects
            .iter()
            .filter(|p| p.name.chars().count() >= 3 && contains_word(&title, &p.name.to_lowercase()))
            .max_by_key(|p| p.name.chars().count())
    }

    /// Decides where a voice task runs: the project the window title names,
    /// else `default_project`, else `most_recent`, else `fallback`.
    pub fn resolve_active(
        &self,
        window_title: Option<&str>,
        default_project: Option<&Path>,
        most_recent: Option<&Path>,
        fallback: &Path,
    ) -> (PathBuf, Resolution) {
        if let Some(project) = window_title.and_then(|title| self.find_in_title(title)) {
            return (project.path.clone(), Resolution::FromWindowTitle);
        }
        if let Some(path) = default_project.filter(|p| p.is_dir()) {
            return (path.to_path_buf(), Resolution::Default);
        }
        if let Some(path) = most_recent.filter(|p| p.is_dir() && !is_too_broad(p)) {
            return (path.to_path_buf(), Resolution::MostRecent);
        }
        (fallback.to_path_buf(), Resolution::WorkingDirectory)
    }
}

/// Hidden and tooling folders (`.git`, `.cache`, `node_modules`): never where
/// a project's repositories live.
fn hidden(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') || n == "node_modules")
}

/// Whether `needle` occurs in `haystack` bounded by non-name characters on
/// both sides — so "eva" is not found inside "evaluación" but is found in
/// "~/code/eva/src" and "main.rs — eva — Code".
fn contains_word(haystack: &str, needle: &str) -> bool {
    let is_name_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-' || c == '.';
    let mut start = 0;
    while let Some(found) = haystack[start..].find(needle) {
        let begin = start + found;
        let end = begin + needle.len();
        let before_ok = haystack[..begin].chars().next_back().is_none_or(|c| !is_name_char(c));
        let after_ok = haystack[end..].chars().next().is_none_or(|c| !is_name_char(c));
        if before_ok && after_ok {
            return true;
        }
        start = begin + haystack[begin..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn index(names: &[&str]) -> ProjectIndex {
        ProjectIndex::new(
            names
                .iter()
                .map(|n| Project { name: (*n).to_string(), path: PathBuf::from(format!("/code/{n}")) })
                .collect(),
        )
    }

    fn repo(root: &Path, name: &str) {
        std::fs::create_dir_all(root.join(name).join(".git")).expect("mkdir");
    }

    #[test]
    fn scan_finds_git_repos_directly_under_a_root_and_ignores_everything_else() {
        let root = tempfile::tempdir().expect("tempdir");
        repo(root.path(), "eva01");
        repo(root.path(), "Novoastar");
        std::fs::create_dir_all(root.path().join("not-a-repo")).expect("mkdir");
        std::fs::write(root.path().join("archivo.txt"), "x").expect("write");

        let names: Vec<_> =
            ProjectIndex::scan(&[root.path().to_path_buf()]).projects().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["eva01", "Novoastar"]);
    }

    #[test]
    fn scan_also_finds_the_repos_one_level_inside_a_folder_that_is_not_a_repo() {
        let root = tempfile::tempdir().expect("tempdir");
        repo(root.path(), "eva01");
        std::fs::create_dir_all(root.path().join("BarberiaSaas")).expect("mkdir");
        repo(&root.path().join("BarberiaSaas"), "barberias-saas");
        repo(&root.path().join("BarberiaSaas"), ".hidden");
        let names: Vec<_> =
            ProjectIndex::scan(&[root.path().to_path_buf()]).projects().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["barberias-saas", "eva01"], "a hidden folder is never a project");
    }

    #[test]
    fn history_adds_projects_the_scan_missed_and_orders_by_recency() {
        use std::time::Duration;
        let root = tempfile::tempdir().expect("tempdir");
        repo(root.path(), "eva01");
        repo(root.path(), "novoastar");
        let outside = tempfile::tempdir().expect("tempdir");
        let extra = outside.path().join("cleanerSpace");
        std::fs::create_dir_all(&extra).expect("mkdir");
        let now = SystemTime::now();
        let index = ProjectIndex::scan(&[root.path().to_path_buf()]).with_history(vec![
            (root.path().join("eva01"), Some(now - Duration::from_secs(3_600))),
            (extra.clone(), Some(now)),
            (root.path().join("borrado"), Some(now)),
            (root.path().join("novoastar"), None),
        ]);
        let names: Vec<_> = index.by_recency().iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            names,
            vec!["cleanerSpace", "eva01", "novoastar"],
            "recent first, undated last, a deleted folder gone"
        );
    }

    #[test]
    fn a_missing_root_is_skipped_not_an_error() {
        assert!(ProjectIndex::scan(&[PathBuf::from("/definitivamente/no/existe")]).projects().is_empty());
    }

    #[test]
    fn the_same_repo_under_two_overlapping_roots_is_listed_once() {
        let root = tempfile::tempdir().expect("tempdir");
        repo(root.path(), "eva01");
        let index = ProjectIndex::scan(&[root.path().to_path_buf(), root.path().to_path_buf()]);
        assert_eq!(index.projects().len(), 1);
    }

    #[test]
    fn a_vs_code_title_names_the_project() {
        let index = index(&["eva01", "novoastar"]);
        let found = index.find_in_title("main.rs — eva01 — Visual Studio Code").expect("found");
        assert_eq!(found.name, "eva01");
    }

    #[test]
    fn a_terminal_title_with_a_path_names_the_project() {
        let index = index(&["eva01", "novoastar"]);
        assert_eq!(index.find_in_title("djoker@mac: ~/code/novoastar").expect("found").name, "novoastar");
    }

    #[test]
    fn a_project_name_inside_a_longer_word_does_not_match() {
        let index = index(&["eva"]);
        assert_eq!(index.find_in_title("Evaluación final — Pages"), None);
        assert_eq!(index.find_in_title("eva-web — Code"), None, "eva-web is a different name, not eva");
    }

    #[test]
    fn the_longest_matching_name_wins() {
        let index = index(&["eva", "eva01"]);
        assert_eq!(index.find_in_title("src — eva01 — Code").expect("found").name, "eva01");
    }

    #[test]
    fn matching_is_case_insensitive_and_ignores_very_short_names() {
        let index = index(&["Eva01", "ab"]);
        assert_eq!(index.find_in_title("EVA01 — Terminal").expect("found").name, "Eva01");
        assert_eq!(index.find_in_title("ab — Notes"), None, "two-letter names match too much");
    }

    #[test]
    fn resolution_prefers_the_window_then_the_default_then_the_recent_then_the_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let default = dir.path().join("default");
        let recent = dir.path().join("recent");
        std::fs::create_dir_all(&default).expect("mkdir");
        std::fs::create_dir_all(&recent).expect("mkdir");
        let fallback = Path::new("/tmp");
        let index = index(&["eva01"]);

        let (path, why) = index.resolve_active(Some("x — eva01"), Some(&default), Some(&recent), fallback);
        assert_eq!((path, why), (PathBuf::from("/code/eva01"), Resolution::FromWindowTitle));

        let (path, why) = index.resolve_active(Some("Notes"), Some(&default), Some(&recent), fallback);
        assert_eq!((path, why), (default.clone(), Resolution::Default));

        let (path, why) = index.resolve_active(None, None, Some(&recent), fallback);
        assert_eq!((path, why), (recent, Resolution::MostRecent));

        let (path, why) = index.resolve_active(None, None, None, fallback);
        assert_eq!((path, why), (PathBuf::from("/tmp"), Resolution::WorkingDirectory));
    }

    #[test]
    fn a_configured_project_that_no_longer_exists_is_skipped() {
        let (path, why) = index(&[]).resolve_active(None, Some(Path::new("/ya/no/existe")), None, Path::new("/tmp"));
        assert_eq!((path, why), (PathBuf::from("/tmp"), Resolution::WorkingDirectory));
    }

    proptest::proptest! {
        #[test]
        fn find_in_title_never_panics(title in ".*", name in "[a-zA-Z0-9_.-]{0,12}") {
            let _ = index(&[&name]).find_in_title(&title);
        }
    }

    #[test]
    fn the_disk_root_top_level_folders_and_home_are_too_broad_for_an_agent_to_work_in() {
        for broad in ["/", "/Users", "/tmp", "/Applications"] {
            assert!(is_too_broad(Path::new(broad)), "{broad}");
        }
        if let Some(home) = dirs::home_dir() {
            assert!(is_too_broad(&home));
            assert!(!is_too_broad(&home.join("code/eva01")));
        }
        assert!(!is_too_broad(Path::new("/repos/iam")));
    }

    #[test]
    fn a_most_recent_project_that_is_too_broad_is_not_reused() {
        let index = ProjectIndex::new(Vec::new());
        let (path, why) = index.resolve_active(None, None, Some(Path::new("/")), Path::new("/tmp/x/y"));
        assert_eq!((path, why), (PathBuf::from("/tmp/x/y"), Resolution::WorkingDirectory));
    }
}
