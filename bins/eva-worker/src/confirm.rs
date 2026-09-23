//! Asking the user a yes/no question through the shell and waiting for the
//! click or hotkey that answers it (`docs/PLAN.md` §6: "confirmación solo
//! por clic o hotkey, jamás por voz"). The broker is the worker's
//! [`Confirmer`]: the gateway calls [`Confirmer::confirm`], which sends a
//! `ConfirmationRequested` event and parks until a `ConfirmationResponse`
//! command resolves it — or the timeout passes, which counts as "no".

use crate::context::Events;
use async_trait::async_trait;
use eva_gateway::Confirmer;
use eva_ipc::WorkerToShell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Parks confirmation requests until the shell answers them.
pub struct ConfirmationBroker {
    events: Events,
    pending: Mutex<HashMap<Uuid, oneshot::Sender<bool>>>,
}

impl ConfirmationBroker {
    /// A broker that asks through `events`.
    pub fn new(events: Events) -> ConfirmationBroker {
        ConfirmationBroker { events, pending: Mutex::new(HashMap::new()) }
    }

    /// Delivers the user's answer to the question `confirmation_id`.
    /// Returns `false` if nothing was waiting for it — a duplicate answer,
    /// or one that arrived after the timeout, both of which must be ignored
    /// rather than acted on.
    pub fn resolve(&self, confirmation_id: Uuid, approved: bool) -> bool {
        let waiting = self.pending().remove(&confirmation_id);
        match waiting {
            Some(sender) => sender.send(approved).is_ok(),
            None => false,
        }
    }

    /// How many questions are waiting for an answer.
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending().len()
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, oneshot::Sender<bool>>> {
        #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
        self.pending.lock().unwrap()
    }
}

#[async_trait]
impl Confirmer for ConfirmationBroker {
    async fn confirm(&self, title: &str, detail: &str, timeout: Duration) -> bool {
        let confirmation_id = Uuid::new_v4();
        let (answer_tx, answer_rx) = oneshot::channel();
        self.pending().insert(confirmation_id, answer_tx);

        self.events.emit(WorkerToShell::ConfirmationRequested {
            confirmation_id,
            title: title.to_string(),
            detail: detail.to_string(),
            timeout_secs: timeout.as_secs(),
        });

        let answer = tokio::time::timeout(timeout, answer_rx).await;
        // Whatever happened, the question is over: take it out of the table
        // (a late answer then finds nothing) and tell the shell to hide it.
        self.pending().remove(&confirmation_id);
        self.events.emit(WorkerToShell::ConfirmationClosed { confirmation_id });

        matches!(answer, Ok(Ok(true)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::sync::Arc;

    fn broker() -> (Arc<ConfirmationBroker>, tokio::sync::mpsc::UnboundedReceiver<WorkerToShell>) {
        let (events, rx) = Events::channel();
        (Arc::new(ConfirmationBroker::new(events)), rx)
    }

    async fn asked(rx: &mut tokio::sync::mpsc::UnboundedReceiver<WorkerToShell>) -> Uuid {
        match rx.recv().await {
            Some(WorkerToShell::ConfirmationRequested { confirmation_id, .. }) => confirmation_id,
            other => panic!("expected a confirmation request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_yes_from_the_shell_approves() {
        let (broker, mut rx) = broker();
        let asking = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("Abrir x", "detalle", Duration::from_secs(5)).await })
        };
        let id = asked(&mut rx).await;
        assert!(broker.resolve(id, true));
        assert!(asking.await.expect("no panic"));
    }

    #[tokio::test]
    async fn a_no_from_the_shell_refuses() {
        let (broker, mut rx) = broker();
        let asking = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("Abrir x", "", Duration::from_secs(5)).await })
        };
        let id = asked(&mut rx).await;
        assert!(broker.resolve(id, false));
        assert!(!asking.await.expect("no panic"));
    }

    #[tokio::test(start_paused = true)]
    async fn silence_past_the_timeout_counts_as_no_and_closes_the_prompt() {
        let (broker, mut rx) = broker();
        let asking = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("Abrir x", "", Duration::from_secs(30)).await })
        };
        let id = asked(&mut rx).await;

        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(!asking.await.expect("no panic"));
        assert!(
            matches!(rx.recv().await, Some(WorkerToShell::ConfirmationClosed { confirmation_id }) if confirmation_id == id)
        );
        assert_eq!(broker.pending_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_that_arrives_after_the_timeout_is_ignored() {
        let (broker, mut rx) = broker();
        let asking = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("Abrir x", "", Duration::from_secs(1)).await })
        };
        let id = asked(&mut rx).await;
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!asking.await.expect("no panic"));

        assert!(!broker.resolve(id, true), "a late yes must not resurrect a refused action");
    }

    #[test]
    fn answering_a_question_nobody_asked_is_ignored() {
        let (broker, _rx) = broker();
        assert!(!broker.resolve(Uuid::new_v4(), true));
    }

    #[tokio::test]
    async fn a_second_answer_to_the_same_question_is_ignored() {
        let (broker, mut rx) = broker();
        let asking = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("x", "", Duration::from_secs(5)).await })
        };
        let id = asked(&mut rx).await;
        assert!(broker.resolve(id, true));
        assert!(!broker.resolve(id, false), "the first answer stands");
        assert!(asking.await.expect("no panic"));
    }

    #[tokio::test]
    async fn two_questions_at_once_are_answered_independently() {
        let (broker, mut rx) = broker();
        let first = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("primera", "", Duration::from_secs(5)).await })
        };
        let second = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.confirm("segunda", "", Duration::from_secs(5)).await })
        };
        let (a, b) = (asked(&mut rx).await, asked(&mut rx).await);
        assert_eq!(broker.pending_count(), 2);
        assert!(broker.resolve(a, true));
        assert!(broker.resolve(b, false));
        let results = [first.await.expect("no panic"), second.await.expect("no panic")];
        assert!(results.contains(&true) && results.contains(&false));
    }
}
