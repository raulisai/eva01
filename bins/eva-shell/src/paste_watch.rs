//! Watching a paste land. The shell holds the Accessibility permission the
//! worker does not always have, so the check that a dictation reached a text
//! field is made here: read the focused field just before the paste, read it
//! again after, and compare. A paste that changed nothing went nowhere, and
//! its text is kept for the user instead of being lost.
//!
//! The reads can take a moment (an app that answers slowly), so they run on a
//! thread of their own and report through a channel the event loop polls.

use eva_macos::{focused_text, frontmost_app, text_target_focused, watch_paste, Landing};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// The outcome for one paste: the words, and whether they landed.
pub struct Watched {
    pub text: String,
    pub landing: Landing,
    /// Why, when they did not (for the island).
    pub reason: &'static str,
}

/// Starts watching pastes and hears their outcomes.
pub struct PasteWatch {
    tx: Sender<Watched>,
    rx: Receiver<Watched>,
}

impl PasteWatch {
    pub fn new() -> PasteWatch {
        let (tx, rx) = channel();
        PasteWatch { tx, rx }
    }

    /// The outcomes that have come in since the last call.
    pub fn outcomes(&self) -> Vec<Watched> {
        self.rx.try_iter().collect()
    }

    /// Wakes the app's accessibility tree before it is needed: browsers and
    /// Electron apps only build theirs once asked, and the first read of a
    /// cold one answers nothing. Called when a recording starts, so the tree
    /// is there when the paste happens.
    pub fn prime(&self) {
        std::thread::spawn(|| {
            let pid = frontmost_app().map(|app| app.pid);
            let _ = focused_text(pid);
        });
    }

    /// A paste of `text` will happen in `in_ms` milliseconds: read the field
    /// now, and again once the paste has had time to land.
    pub fn watch(&self, text: String, in_ms: u64) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let pid = frontmost_app().map(|app| app.pid);
            let before = focused_text(pid);
            // Certainly nothing that takes text (a list, a page with no field
            // focused): no need to wait to see nothing arrive.
            let no_target = text_target_focused(pid) == Some(false);
            std::thread::sleep(Duration::from_millis(in_ms + 80));
            let landing = if no_target {
                Landing::NotLanded
            } else {
                watch_paste(pid, before.as_ref(), Duration::from_millis(700))
            };
            tracing::info!(?landing, no_target, readable = before.is_some(), "pegado comprobado desde la app");
            let reason = if no_target { "No hay dónde pegar" } else { "No se pegó: no había dónde" };
            let _ = tx.send(Watched { text, landing, reason });
        });
    }
}
