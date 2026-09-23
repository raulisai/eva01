//! The seam to whatever asks the human.

use async_trait::async_trait;
use std::time::Duration;

/// Asks the user a yes/no question and waits for the answer.
///
/// The production implementation shows the question on the overlay and waits
/// for a click or hotkey (`eva-worker` → `eva-shell`); it must never be
/// answerable by voice, or a transcript that says "sí" — or an agent that
/// makes the microphone hear one — would approve its own request
/// (`docs/PLAN.md` §6).
#[async_trait]
pub trait Confirmer: Send + Sync {
    /// Returns `true` only for an explicit "yes" within `timeout`. Silence,
    /// a "no", and any failure to ask are all `false`: not being able to ask
    /// is never a reason to allow.
    async fn confirm(&self, title: &str, detail: &str, timeout: Duration) -> bool;
}

/// A [`Confirmer`] for when there is nobody to ask (the standalone `eva-mcp`
/// binary, a CLI run without a terminal): every question is answered "no".
pub struct DenyAll;

#[async_trait]
impl Confirmer for DenyAll {
    async fn confirm(&self, _title: &str, _detail: &str, _timeout: Duration) -> bool {
        false
    }
}

/// Test doubles, exposed (not `#[cfg(test)]`) because `eva-worker`'s own
/// tests need them too, per `docs/ENGINEERING.md` #5.
pub mod mock {
    use super::{async_trait, Confirmer, Duration};
    use std::sync::Mutex;

    /// Answers from a script and remembers every question it was asked.
    pub struct Scripted {
        answers: Mutex<Vec<bool>>,
        questions: Mutex<Vec<(String, String)>>,
    }

    impl Scripted {
        /// Answers the questions in order; once the script runs out, "no".
        pub fn new(answers: Vec<bool>) -> Scripted {
            Scripted { answers: Mutex::new(answers), questions: Mutex::new(Vec::new()) }
        }

        /// Always "yes".
        pub fn approving() -> Scripted {
            Scripted::new(vec![true; 64])
        }

        /// Always "no".
        pub fn denying() -> Scripted {
            Scripted::new(Vec::new())
        }

        /// Every `(title, detail)` asked so far.
        pub fn questions(&self) -> Vec<(String, String)> {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            self.questions.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Confirmer for Scripted {
        async fn confirm(&self, title: &str, detail: &str, _timeout: Duration) -> bool {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            self.questions.lock().unwrap().push((title.to_string(), detail.to_string()));
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            let mut answers = self.answers.lock().unwrap();
            if answers.is_empty() {
                false
            } else {
                answers.remove(0)
            }
        }
    }
}
