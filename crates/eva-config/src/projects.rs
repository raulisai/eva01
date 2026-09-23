//! Which project is a voice task about (`docs/PLAN.md` fase 6: "proyecto
//! activo por contexto"). EVA knows the git repositories directly inside the
//! configured project roots, and picks the one the user is looking at by
//! matching its folder name against the focused window's title — which works
//! across VS Code ("main.rs — eva01 — Visual Studio Code"), Xcode, JetBrains
//! IDEs, Terminal and iTerm ("user@mac: ~/code/eva01") and most terminals,
//! without a per-app integration for each.

use std::path::{Path, PathBuf};

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
}

/// Why [`ProjectIndex::resolve_active`] chose the project it did — shown by
/// `eva doctor` and logged, so "why did it run there?" always has an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The focused window's title names this project.
    FromWindowTitle,
    /// The configured `agents.default_project`.
    Default,
    /// The project a task last ran in.
    MostRecent,
    /// Nothing else applied; the worker's own working directory.
    WorkingDirectory,
}

impl ProjectIndex {
    /// An index over these projects (names must be unique enough to match).
    pub fn new(projects: Vec<Project>) -> ProjectIndex {
        ProjectIndex { projects }
    }

    /// Scans each root for immediate subdirectories that are git
    /// repositories. A root that does not exist is skipped, not an error —
    /// the default roots (`~/code`, `~/Developer`, `~/projects`) are guesses
    /// about where a particular Mac keeps things.
    pub fn scan(roots: &[PathBuf]) -> ProjectIndex {
        let mut projects = Vec::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.join(".git").exists() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        projects.push(Project { name: name.to_string(), path });
                    }
                }
            }
        }
        projects.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
        projects.dedup_by(|a, b| a.path == b.path);
        ProjectIndex { projects }
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
        if let Some(path) = most_recent.filter(|p| p.is_dir()) {
            return (path.to_path_buf(), Resolution::MostRecent);
        }
        (fallback.to_path_buf(), Resolution::WorkingDirectory)
    }
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
            names.iter().map(|n| Project { name: (*n).to_string(), path: PathBuf::from(format!("/code/{n}")) }).collect(),
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
        let (path, why) =
            index(&[]).resolve_active(None, Some(Path::new("/ya/no/existe")), None, Path::new("/tmp"));
        assert_eq!((path, why), (PathBuf::from("/tmp"), Resolution::WorkingDirectory));
    }

    proptest::proptest! {
        #[test]
        fn find_in_title_never_panics(title in ".*", name in "[a-zA-Z0-9_.-]{0,12}") {
            let _ = index(&[&name]).find_in_title(&title);
        }
    }
}
