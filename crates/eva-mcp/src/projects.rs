//! The project list an agent sees through `list_projects`, built from the
//! config: the git repositories directly inside the configured roots, with
//! the one the user is looking at marked active.

use crate::service::ProjectSource;
use eva_config::{expand_home, Config, ProjectIndex};
use eva_ipc::rpc::ProjectEntry;
use std::path::PathBuf;

/// [`ProjectSource`] over a scanned [`ProjectIndex`].
pub struct ConfiguredProjects {
    index: ProjectIndex,
    default_project: Option<PathBuf>,
    fallback: PathBuf,
}

impl ConfiguredProjects {
    /// Scans the configured roots. `fallback` is where a task runs when
    /// nothing says otherwise (the working directory).
    pub fn from_config(config: &Config, fallback: PathBuf) -> ConfiguredProjects {
        let roots: Vec<PathBuf> = config.agents.project_roots.iter().map(|r| expand_home(r)).collect();
        ConfiguredProjects {
            index: ProjectIndex::scan(&roots),
            default_project: config.agents.default_project.as_deref().map(expand_home),
            fallback,
        }
    }

    /// The scanned index, for callers that resolve the active project
    /// themselves (`eva-worker` also weighs the most recent task).
    pub fn index(&self) -> &ProjectIndex {
        &self.index
    }

    /// The configured default project, if any.
    pub fn default_project(&self) -> Option<&std::path::Path> {
        self.default_project.as_deref()
    }
}

impl ProjectSource for ConfiguredProjects {
    fn list(&self, active_window_title: Option<&str>) -> Vec<ProjectEntry> {
        let (active, _why) =
            self.index.resolve_active(active_window_title, self.default_project.as_deref(), None, &self.fallback);
        self.index
            .projects()
            .iter()
            .map(|p| ProjectEntry {
                name: p.name.clone(),
                path: p.path.to_string_lossy().into_owned(),
                active: p.path == active,
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn config_with_root(root: &std::path::Path) -> Config {
        let mut config = Config::default();
        config.agents.project_roots = vec![root.to_string_lossy().into_owned()];
        config
    }

    #[test]
    fn lists_the_repos_under_the_configured_roots_and_marks_the_one_in_the_window_title() {
        let root = tempfile::tempdir().expect("tempdir");
        for name in ["eva01", "novoastar"] {
            std::fs::create_dir_all(root.path().join(name).join(".git")).expect("mkdir");
        }
        let projects = ConfiguredProjects::from_config(&config_with_root(root.path()), PathBuf::from("/tmp"));

        let listed = projects.list(Some("main.rs — novoastar — Code"));
        assert_eq!(listed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["eva01", "novoastar"]);
        assert_eq!(listed.iter().filter(|p| p.active).map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["novoastar"]);
    }

    #[test]
    fn with_no_hint_and_no_default_nothing_is_marked_active() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("eva01").join(".git")).expect("mkdir");
        let projects = ConfiguredProjects::from_config(&config_with_root(root.path()), PathBuf::from("/tmp"));
        assert!(projects.list(None).iter().all(|p| !p.active));
    }

    #[test]
    fn the_configured_default_is_the_active_project_when_the_window_says_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("eva01").join(".git")).expect("mkdir");
        let mut config = config_with_root(root.path());
        config.agents.default_project = Some(root.path().join("eva01").to_string_lossy().into_owned());
        let projects = ConfiguredProjects::from_config(&config, PathBuf::from("/tmp"));
        assert!(projects.list(Some("Notes"))[0].active);
    }
}
