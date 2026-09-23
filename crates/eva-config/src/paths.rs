//! Where EVA01 keeps its files, and `~` expansion for paths in the config.

use std::path::{Path, PathBuf};

/// `~/Library/Application Support/EVA01` — the config file, the database, the
/// models and the agents' worktrees all live under here, so uninstalling is
/// deleting one folder.
pub fn support_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("EVA01")
}

/// Expands a leading `~` or `~/` to the home directory. Anything else is
/// returned as-is.
pub fn expand_home(path: &str) -> PathBuf {
    let home = dirs::home_dir();
    match (path.strip_prefix("~/"), path == "~", home) {
        (Some(rest), _, Some(home)) => home.join(rest),
        (None, true, Some(home)) => home,
        _ => Path::new(path).to_path_buf(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn tilde_slash_expands_to_the_home_directory() {
        let home = dirs::home_dir().expect("a home directory");
        assert_eq!(expand_home("~/code/eva01"), home.join("code/eva01"));
        assert_eq!(expand_home("~"), home);
    }

    #[test]
    fn other_paths_are_untouched() {
        assert_eq!(expand_home("/opt/models"), PathBuf::from("/opt/models"));
        assert_eq!(expand_home("relativo/ruta"), PathBuf::from("relativo/ruta"));
        assert_eq!(expand_home("~otro/usuario"), PathBuf::from("~otro/usuario"));
    }

    #[test]
    fn the_support_directory_is_named_after_the_app() {
        assert!(support_dir().ends_with("EVA01"));
    }
}
