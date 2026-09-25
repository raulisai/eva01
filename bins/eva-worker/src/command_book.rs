//! The user's own commands, as the worker holds them: the ones written in
//! `config.toml` and `commands/*.toml`, kept fresh. A command created in the
//! panel works on the very next thing said — no restart — because the book
//! looks at the files' modification times before each use and reloads when any
//! of them changed.

use eva_config::{CommandConfig, Config};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::SystemTime;

/// Every file the commands come from, with when it last changed.
type Stamp = Vec<(PathBuf, SystemTime)>;

struct Disk {
    config_path: PathBuf,
    stamp: Stamp,
}

/// The commands that can run, and where they are kept fresh from.
pub struct CommandBook {
    commands: RwLock<Vec<CommandConfig>>,
    disk: Mutex<Option<Disk>>,
}

impl CommandBook {
    /// A book holding `commands`, not reading anything from disk (tests, and
    /// the worker until [`CommandBook::watch`] is called).
    pub fn new(commands: Vec<CommandConfig>) -> CommandBook {
        CommandBook { commands: RwLock::new(commands), disk: Mutex::new(None) }
    }

    /// From now on, reloads the commands whenever the config file at
    /// `config_path` or the `commands/` folder next to it changes.
    pub fn watch(&self, config_path: &Path) {
        let stamp = stamp_of(config_path);
        *self.disk.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Disk { config_path: config_path.to_path_buf(), stamp });
    }

    /// The commands that can actually run, fresh from disk if it changed.
    pub fn runnable(&self) -> Vec<CommandConfig> {
        self.refresh();
        self.commands
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|c| c.action().is_ok())
            .cloned()
            .collect()
    }

    fn refresh(&self) {
        let mut disk = self.disk.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(disk) = disk.as_mut() else { return };
        let now = stamp_of(&disk.config_path);
        if now == disk.stamp {
            return;
        }
        let loaded = Config::load_from(&disk.config_path);
        for warning in &loaded.warnings {
            tracing::warn!("{warning}");
        }
        tracing::info!(commands = loaded.config.commands.len(), "órdenes propias recargadas: los archivos cambiaron");
        *self.commands.write().unwrap_or_else(PoisonError::into_inner) = loaded.config.commands;
        disk.stamp = now;
    }
}

/// The config file and everything in the `commands/` folder beside it, with
/// their modification times, sorted so two looks at the same disk are equal.
fn stamp_of(config_path: &Path) -> Stamp {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut stamp: Stamp = Vec::new();
    stamp.extend(modified(config_path).map(|at| (config_path.to_path_buf(), at)));
    if let Some(dir) = config_path.parent().map(|d| d.join("commands")) {
        stamp.extend(modified(&dir).map(|at| (dir.clone(), at)));
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            stamp.extend(modified(&path).map(|at| (path, at)));
        }
    }
    stamp.sort();
    stamp
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn says(book: &CommandBook) -> Vec<String> {
        book.runnable().into_iter().map(|c| c.say).collect()
    }

    #[test]
    fn a_book_that_watches_nothing_keeps_what_it_was_given() {
        let command =
            CommandConfig { say: "mi correo".into(), insert: Some("a@b.c".into()), ..CommandConfig::default() };
        let book = CommandBook::new(vec![command, CommandConfig { say: "roto".into(), ..CommandConfig::default() }]);
        assert_eq!(says(&book), ["mi correo"], "one that cannot run is not offered");
    }

    #[test]
    fn a_command_written_while_running_is_there_on_the_next_look_and_gone_when_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let book = CommandBook::new(Vec::new());
        book.watch(&config);
        assert!(says(&book).is_empty());

        let folder = dir.path().join("commands");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("ui-canal.toml"),
            "[[commands]]\nsay = \"ver mi canal\"\nopen = [\"https://youtube.com\"]",
        )
        .unwrap();
        assert_eq!(says(&book), ["ver mi canal"]);

        std::fs::remove_file(folder.join("ui-canal.toml")).unwrap();
        assert!(says(&book).is_empty());
    }
}
