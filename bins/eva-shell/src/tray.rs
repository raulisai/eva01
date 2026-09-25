//! The menu-bar item: an icon that shows the state, a menu that shows the
//! tasks. The *what* — which lines, in which order — is [`menu_spec`], pure
//! and tested; [`Tray`] only turns that into `tray-icon` objects.

use crate::icons;
use crate::model::{short, TrayIcon, TrayView};
use eva_ipc::{TaskInfo, TaskState};
use tray_icon::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

/// Menu item ids the event loop reacts to.
pub mod id {
    /// Cancel every running task.
    pub const CANCEL_TASKS: &str = "cancel_tasks";
    /// Flag the last dictation as wrong.
    pub const FLAG_BAD: &str = "flag_bad";
    /// Open the panel: history, dictionary, commands, settings and Doctor in one window.
    pub const PANEL: &str = "panel";
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
    /// A submenu, with lines of its own (labels, actions, dividers).
    Submenu {
        /// Its title.
        title: String,
        /// Its lines.
        items: Vec<Item>,
    },
    /// A divider.
    Separator,
}

/// The menu for `view`: the status, one **Tareas** submenu (what is running,
/// then how the recent ones ended), and the few things done from the menu bar.
/// Everything else — history, logs, settings, Doctor — lives in the panel.
pub fn menu_spec(view: &TrayView) -> Vec<Item> {
    let mut items = vec![Item::Label(format!("EVA01 — {}", view.status)), Item::Separator];
    items.push(Item::Submenu { title: tasks_title(view), items: tasks_items(view) });
    items.extend([
        Item::Separator,
        Item::Action { id: id::FLAG_BAD, text: "Esto salió mal (último dictado)".to_string(), enabled: true },
        Item::Action { id: id::PANEL, text: "Panel".to_string(), enabled: true },
        Item::Separator,
        Item::Action { id: id::QUIT, text: "Salir de EVA01".to_string(), enabled: true },
    ]);
    items
}

fn tasks_title(view: &TrayView) -> String {
    match view.running.len() {
        0 => "Tareas".to_string(),
        n => format!("Tareas · ▶ {n} en curso"),
    }
}

/// Inside the submenu: what is running (with a way to cancel it), a divider,
/// then the recent ones — ✓ if they went well, ✗ if not.
fn tasks_items(view: &TrayView) -> Vec<Item> {
    let mut items = vec![Item::Label("En curso".to_string())];
    if view.running.is_empty() {
        items.push(Item::Label("   Ninguna".to_string()));
    }
    for task in &view.running {
        items.push(Item::Label(format!(
            "▶ {} — {} ({})",
            provider_name(&task.provider),
            short(&task.prompt, 46),
            age(task.age_secs)
        )));
    }
    if !view.running.is_empty() {
        items.push(Item::Action { id: id::CANCEL_TASKS, text: "Cancelar tareas en curso".to_string(), enabled: true });
    }
    items.push(Item::Separator);
    items.push(Item::Label("Recientes".to_string()));
    if view.recent.is_empty() {
        items.push(Item::Label("   Aún no hay tareas".to_string()));
    }
    items.extend(view.recent.iter().map(|task| Item::Label(recent_line(task))));
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

fn make_item(item: &Item) -> Box<dyn IsMenuItem> {
    match item {
        Item::Label(text) => Box::new(MenuItem::new(text, false, None)),
        Item::Action { id, text, enabled } => Box::new(MenuItem::with_id(*id, text, *enabled, None)),
        Item::Separator => Box::new(PredefinedMenuItem::separator()),
        Item::Submenu { title, items } => {
            let submenu = Submenu::new(title, true);
            for child in items {
                let _ = submenu.append(make_item(child).as_ref());
            }
            Box::new(submenu)
        }
    }
}

fn build_menu(items: &[Item]) -> Menu {
    let menu = Menu::new();
    for item in items {
        let _ = menu.append(make_item(item).as_ref());
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

    /// The lines inside the Tareas submenu.
    fn tasks(items: &[Item]) -> &[Item] {
        items
            .iter()
            .find_map(|i| match i {
                Item::Submenu { items, .. } => Some(items.as_slice()),
                _ => None,
            })
            .expect("the menu always has the Tareas submenu")
    }

    fn cancel_item(items: &[Item]) -> Option<bool> {
        tasks(items).iter().find_map(|i| match i {
            Item::Action { id, enabled, .. } if *id == id::CANCEL_TASKS => Some(*enabled),
            _ => None,
        })
    }

    fn labels(items: &[Item]) -> Vec<&str> {
        tasks(items)
            .iter()
            .filter_map(|i| match i {
                Item::Label(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_menu_always_offers_to_flag_the_last_dictation() {
        let items = menu_spec(&view(vec![], vec![]));
        assert!(items.iter().any(|i| matches!(i, Item::Action { id: id::FLAG_BAD, enabled: true, .. })));
    }

    #[test]
    fn the_menu_starts_with_the_status_and_ends_with_quit() {
        let items = menu_spec(&view(vec![], vec![]));
        assert_eq!(items[0], Item::Label("EVA01 — Listo".into()));
        assert!(matches!(items.last(), Some(Item::Action { id: id::QUIT, .. })));
    }

    #[test]
    fn the_menu_is_status_tasks_and_a_few_actions_with_no_logs_entry() {
        let items = menu_spec(&view(vec![], vec![]));
        assert_eq!(items.iter().filter(|i| matches!(i, Item::Submenu { .. })).count(), 1);
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                Item::Action { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["Esto salió mal (último dictado)", "Panel", "Salir de EVA01"]);
    }

    #[test]
    fn cancelling_tasks_is_only_offered_when_there_is_something_to_cancel() {
        assert_eq!(cancel_item(&menu_spec(&view(vec![], vec![]))), None);
        let running = vec![task("codex", "refactoriza", TaskState::Running, None, 5)];
        assert_eq!(cancel_item(&menu_spec(&view(running, vec![]))), Some(true));
    }

    #[test]
    fn an_idle_menu_says_so_instead_of_showing_empty_sections() {
        let items = menu_spec(&view(vec![], vec![]));
        assert_eq!(labels(&items), ["En curso", "   Ninguna", "Recientes", "   Aún no hay tareas"]);
        assert!(matches!(&items[2], Item::Submenu { title, .. } if title == "Tareas"));
    }

    #[test]
    fn a_running_task_shows_who_what_and_for_how_long_and_the_title_counts_it() {
        let running = vec![task("claude_code", "arregla el login", TaskState::Running, None, 190)];
        let items = menu_spec(&view(running, vec![]));
        assert!(labels(&items).contains(&"▶ Claude — arregla el login (3 min)"), "{items:?}");
        assert!(matches!(&items[2], Item::Submenu { title, .. } if title == "Tareas · ▶ 1 en curso"));
    }

    #[test]
    fn running_tasks_come_first_and_recent_ones_are_marked_by_outcome() {
        let running = vec![task("codex", "refactoriza", TaskState::Running, None, 5)];
        let recent = vec![
            task("codex", "agrega tests", TaskState::Succeeded, Some("3 archivos"), 60),
            task("codex", "borra la cache", TaskState::Failed, None, 90),
        ];
        let items = menu_spec(&view(running, recent));
        assert_eq!(
            labels(&items),
            [
                "En curso",
                "▶ Codex — refactoriza (menos de 1 min)",
                "Recientes",
                "✓ Codex — agrega tests → 3 archivos",
                "✗ Codex — borra la cache",
            ]
        );
    }

    #[test]
    fn a_very_long_prompt_is_cut_to_fit_a_menu() {
        let running = vec![task("codex", &"palabra ".repeat(40), TaskState::Running, None, 1)];
        let items = menu_spec(&view(running, vec![]));
        let line = labels(&items).into_iter().find(|l| l.starts_with('▶')).expect("the task line");
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
        let running =
            vec![task("codex", "a", TaskState::Running, None, 1), task("codex", "b", TaskState::Running, None, 2)];
        assert_eq!(title_for(&view(running, vec![])), Some("▶2".to_string()));
    }

    #[test]
    fn an_unknown_provider_id_is_shown_as_is() {
        assert_eq!(provider_name("gemini"), "gemini");
    }
}
