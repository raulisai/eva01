//! The few seconds after a command that went well, in which EVA01 is still
//! "in conversation": it remembers what was just opened (YouTube, Spotify…),
//! says "¿Algo más?" on the island, and the next thing said leans on it —
//! "ahora busca Naruto", with no wake word, searches YouTube.
//!
//! Only what the last command plainly was is remembered (`eva_intent::context`),
//! and only while the window is open; it never listens on its own — the key is
//! pressed as always.

use crate::context::WorkerContext;
use eva_intent::context::Recall;
use eva_intent::{AppIndex, Intent, Interpreted};
use eva_ipc::WorkerToShell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long the island asks "¿Algo más?" after a command, and how long a
/// follow-up is understood without the wake word.
pub const FOLLOW_UP: Duration = Duration::from_secs(5);

/// The window is held a moment longer than it is shown: "Spotify abierto"
/// stays on the island first, and the five seconds count from after it.
const HELD: Duration = Duration::from_millis(6_500);

/// What was last opened and until when it can be leaned on.
#[derive(Default)]
pub struct Conversation {
    open: Mutex<Option<(Recall, Instant)>>,
}

impl Conversation {
    /// Opens the window on `recall`, as of `now`.
    fn open_at(&self, recall: Recall, now: Instant) {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        {
            *self.open.lock().unwrap() = Some((recall, now + HELD));
        }
    }

    /// What can be leaned on at `now`, if the window is still open.
    fn current_at(&self, now: Instant) -> Option<Recall> {
        #[allow(clippy::unwrap_used)] // as above
        let mut open = self.open.lock().unwrap();
        match open.as_ref() {
            Some((recall, until)) if now < *until => Some(recall.clone()),
            Some(_) => {
                *open = None;
                None
            }
            None => None,
        }
    }

    /// What can be leaned on right now.
    pub fn current(&self) -> Option<Recall> {
        self.current_at(Instant::now())
    }
}

/// [`eva_intent::interpret_in_context`] with what was just done.
pub fn interpret(ctx: &WorkerContext, text: &str, apps: &AppIndex, custom: &[&str], learned: &[String]) -> Interpreted {
    let recall = ctx.conversation.current();
    eva_intent::interpret_in_context(text, &ctx.wake_word, apps, custom, learned, recall.as_ref())
}

/// Runs `intent`; if it opened something and nothing went wrong, opens the
/// conversation on it and tells the island.
pub async fn run(ctx: &Arc<WorkerContext>, request_id: Uuid, intent: Intent) {
    // A search typed in place leaves things as they were: the same site, still in front.
    let recall = Recall::of(&intent)
        .or_else(|| matches!(intent, Intent::SearchInSite { .. }).then(|| ctx.conversation.current()).flatten());
    let problems = ctx.events.problems();
    crate::commands::run_intent(ctx, request_id, intent).await;
    if let Some(recall) = recall.filter(Recall::is_useful) {
        if ctx.events.problems() == problems {
            ctx.conversation.open_at(recall, Instant::now());
            ctx.events.emit(WorkerToShell::FollowUp { request_id, secs: FOLLOW_UP.as_secs() });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::ShellToWorker;
    use eva_mcp::desktop::mock::Call;

    fn typed(text: &str) -> ShellToWorker {
        ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: text.to_string() }
    }

    #[tokio::test]
    async fn after_opening_youtube_the_next_search_needs_no_wake_word_and_searches_there() {
        let youtube = eva_macos::RunningAppInfo {
            localized_name: Some("Brave Browser".to_string()),
            bundle_identifier: Some("com.brave.Browser".to_string()),
            pid: 1,
            window_title: Some("YouTube - Brave".to_string()),
        };
        let mut rig =
            Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::new().with_active_window(youtube)).build();

        let events = rig.run(typed("Adán, abre YouTube")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::OpenUrl("https://www.youtube.com".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::FollowUp { secs: 5, .. })), "{events:?}");

        // With YouTube in front, the search is typed into its own search box.
        rig.run(typed("ahora busca Naruto")).await;
        let calls = rig.desktop.calls();
        assert_eq!(
            calls[1..],
            [
                Call::PressCombo("/".to_string()),
                Call::PressCombo("cmd+a".to_string()),
                Call::InsertText("Naruto".to_string()),
                Call::PressCombo("return".to_string()),
            ]
        );

        // The conversation goes on: same site, still in front.
        let more = rig.run(typed("busca Boruto")).await;
        assert_eq!(rig.desktop.calls().iter().filter(|c| **c == Call::InsertText("Boruto".to_string())).count(), 1);
        assert!(more.iter().any(|e| matches!(e, WorkerToShell::FollowUp { .. })));
    }

    #[tokio::test]
    async fn if_the_user_went_elsewhere_the_search_opens_the_page_as_before() {
        let notes = eva_macos::RunningAppInfo {
            localized_name: Some("Notes".to_string()),
            bundle_identifier: Some("com.apple.Notes".to_string()),
            pid: 1,
            window_title: Some("Lista".to_string()),
        };
        let mut rig =
            Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::new().with_active_window(notes)).build();
        rig.run(typed("Adán, abre YouTube")).await;
        rig.run(typed("busca Naruto")).await;
        assert_eq!(
            rig.desktop.calls().last(),
            Some(&Call::OpenUrl("https://www.youtube.com/results?search_query=Naruto".to_string())),
            "nothing is typed into Notes"
        );
        assert!(!rig.desktop.calls().iter().any(|c| matches!(c, Call::PressCombo(_))));
    }

    #[tokio::test]
    async fn without_a_conversation_the_same_words_are_dictation() {
        let mut rig = Rig::new();
        rig.run(typed("ahora busca Naruto")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Ahora busca Naruto. ".to_string())]);
    }

    #[tokio::test]
    async fn a_command_that_failed_opens_no_conversation() {
        let mut rig = Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::failing()).build();
        let events = rig.run(typed("Adán, abre YouTube")).await;
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::FollowUp { .. })), "{events:?}");
        assert!(rig.ctx.conversation.current().is_none());
    }

    #[test]
    fn the_window_closes_by_itself() {
        let conversation = Conversation::default();
        let recall = Recall::of(&Intent::OpenUrl { url: "https://www.youtube.com".to_string() }).unwrap();
        let start = Instant::now();
        conversation.open_at(recall, start);
        assert!(conversation.current_at(start + Duration::from_secs(6)).is_some());
        assert!(conversation.current_at(start + Duration::from_secs(7)).is_none());
        assert!(conversation.current_at(start).is_none(), "and stays closed once it has closed");
    }
}
