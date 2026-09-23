//! The menu-bar item: an icon that shows the state, a menu that shows the
//! tasks. The *what* — which lines, in which order — is [`menu_spec`], pure
//! and tested; [`Tray`] only turns that into `tray-icon` objects.

use crate::icons;
use crate::model::{short, TrayIcon, TrayView};
use eva_ipc::{TaskInfo, TaskState};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

/// Menu item ids the event loop reacts to.
pub mod id {
    /// Cancel every running task.
    pub const CANCEL_TASKS: &str = "cancel_tasks";
    /// Open `config.toml` in the default editor.
    pub const OPEN_CONFIG: &str = "open_config";
    /// Open the logs folder.
    pub const OPEN_LOGS: &str = "open_logs";
    /// Quit EVA.
    pub const QUIT: &str = "quit";
}

/// One line of the menu, before it becomes a `tray-icon` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A line of text that does nothing when clicked.
    Label(String),
    /// A clickable line.
    Action {
        /// What the event loop matches on.
        id: &'static str,
        /// The text.
        text: String,
        /// Whether it can be clicked.
        enabled: bool,
    },
    /// A submenu of labels.
    Submenu {
        /// Its title.
        title: String,
        /// Its lines.
        labels: Vec<String>,
    },
    /// A divider.
    Separator,
}

/// The menu for `view`.
pub fn menu_spec(view: &TrayView) -> Vec<Item> {
    let mut items = vec![Item::Label(format!("EVA01 — {}", view.status)), Item::Separator];

    for task in &view.running {
        items.push(Item::Label(format!("▶ {} — {} ({})", provider_name(&task.provider), short(&task.prompt, 46), age(task.age_secs))));
    }
    items.push(Item::Action {
        id: id::CANCEL_TASKS,
        text: "Cancelar tareas en curso".to_string(),
        enabled: !view.running.is_empty(),
    });
    if !view.recent.is_empty() {
        items.push(Item::Submenu {
            title: "Tareas recientes".to_string(),
            labels: view.recent.iter().map(recent_line).collect(),
        });
    }

    items.extend([
        Item::Separator,
        Item::Action { id: id::OPEN_CONFIG, text: "Abrir configuración…".to_string(), enabled: true },
        Item::Action { id: id::OPEN_LOGS, text: "Abrir carpeta de registros".to_string(), enabled: true },
        Item::Separator,
        Item::Action { id: id::QUIT, text: "Salir de EVA01".to_string(), enabled: true },
    ]);
    items
}

fn recent_line(task: &TaskInfo) -> String {
    let mark = if task.state == TaskState::Succeeded { "✓" } else { "✗" };
    let summary = task.summary.as_deref().map(|s| format!(" → {}", short(s, 40))).unwrap_or_default();
    format!("{mark} {} — {}{summary}", provider_name(&task.provider), short(&task.prompt, 40))
}

fn provider_name(id: &str) -> &str {
    match id {
        "claude_code" => "Claude",
        "codex" => "Codex",
        other => other,
    }
}

/// Whole minutes, never seconds: the menu is only rebuilt when a line
/// changes, and rebuilding one the user has open would close it — a
/// seconds counter would do that every second.
fn age(secs: u64) -> String {
    match secs {
        0..=59 => "menos de 1 min".to_string(),
        60..=3_599 => format!("{} min", secs / 60),
        _ => format!("{} h", secs / 3_600),
    }
}

/// The title shown next to the icon: how many tasks are running, if any.
pub fn title_for(view: &TrayView) -> Option<String> {
    (!view.running.is_empty()).then(|| format!("▶{}", view.running.len()))
}

/// The live tray item.
pub struct Tray {
    tray: tray_icon::TrayIcon,
    shown: Option<(TrayIcon, Vec<Item>, Option<String>)>,
}

impl Tray {
    /// Creates the item in the menu bar.
    ///
    /// # Errors
    /// The message from `tray-icon` if the system refused.
    pub fn new(view: &TrayView) -> Result<Tray, String> {
        let tray = TrayIconBuilder::new()
            .with_tooltip("EVA01")
            .with_icon(icons::render(view.icon))
            .with_menu(Box::new(build_menu(&menu_spec(view))))
            .build()
            .map_err(|e| e.to_string())?;
        let mut me = Tray { tray, shown: None };
        me.update(view);
        Ok(me)
    }

    /// Brings the icon, title and menu in line with `view`, touching only
    /// what changed — rebuilding a menu the user has open would close it.
    pub fn update(&mut self, view: &TrayView) {
        let spec = menu_spec(view);
        let title = title_for(view);
        let (icon_changed, menu_changed, title_changed) = match &self.shown {
            None => (true, true, true),
            Some((icon, items, shown_title)) => (*icon != view.icon, *items != spec, *shown_title != title),
        };

        if icon_changed {
            let _ = self.tray.set_icon(Some(icons::render(view.icon)));
        }
        if menu_changed {
            self.tray.set_menu(Some(Box::new(build_menu(&spec))));
        }
        if title_changed {
            self.tray.set_title(title.as_deref());
        }
        self.shown = Some((view.icon, spec, title));
    }
}

fn build_menu(items: &[Item]) -> Menu {
    let menu = Menu::new();
    for item in items {
        let _ = match item {
            Item::Label(text) => menu.append(&MenuItem::new(text, false, None)),
            Item::Action { id, text, enabled } => menu.append(&MenuItem::with_id(*id, text, *enabled, None)),
            Item::Separator => menu.append(&PredefinedMenuItem::separator()),
            Item::Submenu { title, labels } => {
                let submenu = Submenu::new(title, true);
                for label in labels {
                    let _ = submenu.append(&MenuItem::new(label, false, None));
                }
                menu.append(&submenu)
            }
        };
    }
    menu
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use uuid::Uuid;

    fn task(provider: &str, prompt: &str, state: TaskState, summary: Option<&str>, age_secs: u64) -> TaskInfo {
        TaskInfo {
            request_id: Uuid::new_v4(),
            provider: provider.into(),
            prompt: prompt.into(),
            state,
            summary: summary.map(str::to_string),
            age_secs,
        }
    }

    fn view(running: Vec<TaskInfo>, recent: Vec<TaskInfo>) -> TrayView {
        TrayView { icon: TrayIcon::Idle, status: "Listo".into(), running, recent }
    }

    fn cancel_item(items: &[Item]) -> (bool, String) {
        items
            .iter()
            .find_map(|i| match i {
                Item::Action { id, text, enabled } if *id == id::CANCEL_TASKS => Some((*enabled, text.clone())),
                _ => None,
            })
            .expect("the cancel item is always in the menu")
    }

    #[test]
    fn the_menu_starts_with_the_status_and_ends_with_quit() {
        let items = menu_spec(&view(vec![], vec![]));
        assert_eq!(items[0], Item::Label("EVA01 — Listo".into()));
        assert!(matches!(items.last(), Some(Item::Action { id: id::QUIT, .. })));
    }

    #[test]
    fn cancelling_tasks_is_only_offered_when_there_is_something_to_cancel() {
        assert!(!cancel_item(&menu_spec(&view(vec![], vec![]))).0);
        let running = vec![task("codex", "refactoriza", TaskState::Running, None, 5)];
        assert!(cancel_item(&menu_spec(&view(running, vec![]))).0);
    }

    #[test]
    fn a_running_task_shows_who_what_and_for_how_long() {
        let running = vec![task("claude_code", "arregla el login", TaskState::Running, None, 190)];
        let items = menu_spec(&view(running, vec![]));
        assert!(items.contains(&Item::Label("▶ Claude — arregla el login (3 min)".into())), "{items:?}");
    }

    #[test]
    fn recent_tasks_are_a_submenu_marked_by_outcome() {
        let recent = vec![
            task("codex", "agrega tests", TaskState::Succeeded, Some("3 archivos"), 60),
            task("codex", "borra la cache", TaskState::Failed, None, 90),
        ];
        let items = menu_spec(&view(vec![], recent));
        let Some(Item::Submenu { labels, .. }) = items.iter().find(|i| matches!(i, Item::Submenu { .. })) else {
            panic!("expected a submenu");
        };
        assert_eq!(labels[0], "✓ Codex — agrega tests → 3 archivos");
        assert_eq!(labels[1], "✗ Codex — borra la cache");
    }

    #[test]
    fn with_no_recent_tasks_there_is_no_empty_submenu() {
        assert!(!menu_spec(&view(vec![], vec![])).iter().any(|i| matches!(i, Item::Submenu { .. })));
    }

    #[test]
    fn a_very_long_prompt_is_cut_to_fit_a_menu() {
        let running = vec![task("codex", &"palabra ".repeat(40), TaskState::Running, None, 1)];
        let items = menu_spec(&view(running, vec![]));
        let Some(Item::Label(line)) = items.iter().find(|i| matches!(i, Item::Label(t) if t.starts_with('▶'))) else {
            panic!("expected the task line");
        };
        assert!(line.chars().count() < 80, "{line}");
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(5), "menos de 1 min");
        assert_eq!(age(59), "menos de 1 min");
        assert_eq!(age(60), "1 min");
        assert_eq!(age(3_599), "59 min");
        assert_eq!(age(7_300), "2 h");
    }

    #[test]
    fn the_title_counts_running_tasks_and_is_absent_with_none() {
        assert_eq!(title_for(&view(vec![], vec![])), None);
        let running = vec![task("codex", "a", TaskState::Running, None, 1), task("codex", "b", TaskState::Running, None, 2)];
        assert_eq!(title_for(&view(running, vec![])), Some("▶2".to_string()));
    }

    #[test]
    fn an_unknown_provider_id_is_shown_as_is() {
        assert_eq!(provider_name("gemini"), "gemini");
    }
}
