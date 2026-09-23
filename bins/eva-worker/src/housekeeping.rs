//! The small commands: the personal dictionary, the wake word, the health
//! report and the task list.

use crate::context::WorkerContext;
use eva_agents::ProviderStatus;
use eva_ipc::{HealthReport, WorkerToShell};
use std::sync::Arc;
use uuid::Uuid;

fn words_event(ctx: &WorkerContext, request_id: Uuid) -> WorkerToShell {
    WorkerToShell::CustomWords { request_id, words: ctx.store.list_custom_words().unwrap_or_default() }
}

/// Adds a word to the personal dictionary and replies with the new list, so
/// a caller never has to ask twice to see its own change.
pub fn add_custom_word(ctx: &WorkerContext, request_id: Uuid, word: &str) {
    if word.trim().is_empty() {
        ctx.events.error(request_id, "la palabra está vacía");
        return;
    }
    match ctx.store.add_custom_word(word) {
        Ok(()) => ctx.events.emit(words_event(ctx, request_id)),
        Err(e) => ctx.events.error(request_id, e.to_string()),
    }
}

/// Removes a word from the personal dictionary and replies with the new list.
pub fn remove_custom_word(ctx: &WorkerContext, request_id: Uuid, word: &str) {
    match ctx.store.remove_custom_word(word) {
        Ok(()) => ctx.events.emit(words_event(ctx, request_id)),
        Err(e) => ctx.events.error(request_id, e.to_string()),
    }
}

/// Replies with the personal dictionary.
pub fn list_custom_words(ctx: &WorkerContext, request_id: Uuid) {
    ctx.events.emit(words_event(ctx, request_id));
}

/// Saves the wake word. It takes effect on the next start — the running
/// gate keeps the word it was built with, so this is `Ack` alone, with no
/// false-alarm `Error` for what is not one.
pub fn set_wake_word(ctx: &WorkerContext, request_id: Uuid, word: &str) {
    if word.trim().is_empty() {
        ctx.events.error(request_id, "la palabra de activación no puede estar vacía");
        return;
    }
    match ctx.store.set_setting("wake_word", &word.trim()) {
        Ok(()) => ctx.events.emit(WorkerToShell::Ack { request_id }),
        Err(e) => ctx.events.error(request_id, e.to_string()),
    }
}

/// Replies with what is cheap to know instantly, plus each agent's status —
/// detection spawns real processes (`codex login status`, `claude doctor`)
/// and takes real time, so it runs as a background job rather than holding
/// up the command loop.
pub fn health(ctx: &Arc<WorkerContext>, request_id: Uuid, gateway_socket: Option<String>) {
    let job_ctx = Arc::clone(ctx);
    ctx.spawn_job(async move {
        let agents = job_ctx
            .agents
            .detect_all()
            .await
            .into_iter()
            .map(|(id, status)| (id.to_string(), describe(&status)))
            .collect();

        job_ctx.events.emit(WorkerToShell::Health {
            request_id,
            report: HealthReport {
                stt_model_loaded: job_ctx.audio.is_some(),
                stt_model_id: job_ctx.audio.as_ref().map(|a| a.model_id.clone()),
                store_ok: job_ctx.store.recent_transcripts(1).is_ok(),
                agents,
                formatter: job_ctx.formatter_name.clone(),
                config_warnings: job_ctx.config_warnings.clone(),
                gateway_socket,
                project_count: job_ctx.project_index().projects().len(),
            },
        });
    });
}

fn describe(status: &ProviderStatus) -> String {
    match status {
        ProviderStatus::NotInstalled => "no instalado".to_string(),
        ProviderStatus::InstalledNoSession { .. } => "instalado, sin sesión iniciada".to_string(),
        ProviderStatus::Active { version } => format!("listo ({version})"),
    }
}

/// Replies with the running tasks followed by the recent finished ones.
pub fn list_tasks(ctx: &WorkerContext, request_id: Uuid) {
    ctx.events.emit(WorkerToShell::TaskList { request_id, tasks: crate::tasks::snapshot(ctx) });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::ShellToWorker;

    #[tokio::test]
    async fn add_custom_word_persists_it_and_returns_the_updated_list() {
        let mut rig = Rig::new();
        let request_id = Uuid::new_v4();
        let events = rig.run(ShellToWorker::AddCustomWord { request_id, word: "García".to_string() }).await;

        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::CustomWords { words, .. } if words == &vec!["García".to_string()])));
        assert_eq!(rig.ctx.store.list_custom_words().expect("must succeed"), vec!["García"]);
    }

    #[tokio::test]
    async fn remove_custom_word_takes_it_out_of_the_list() {
        let mut rig = Rig::new();
        rig.ctx.store.add_custom_word("García").expect("must succeed");
        let events =
            rig.run(ShellToWorker::RemoveCustomWord { request_id: Uuid::new_v4(), word: "García".to_string() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::CustomWords { words, .. } if words.is_empty())));
    }

    #[tokio::test]
    async fn adding_an_empty_word_is_a_clear_error_not_a_silent_no_op() {
        let mut rig = Rig::new();
        let events =
            rig.run(ShellToWorker::AddCustomWord { request_id: Uuid::new_v4(), word: "   ".to_string() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn list_custom_words_replies_with_the_dictionary() {
        let mut rig = Rig::new();
        rig.ctx.store.add_custom_word("Núñez").expect("must succeed");
        let events = rig.run(ShellToWorker::ListCustomWords { request_id: Uuid::new_v4() }).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::CustomWords { words, .. } if words == &vec!["Núñez".to_string()])));
    }

    #[tokio::test]
    async fn set_wake_word_persists_to_settings_and_acknowledges() {
        let mut rig = Rig::new();
        let request_id = Uuid::new_v4();
        let events = rig.run(ShellToWorker::SetWakeWord { request_id, word: " Eva ".to_string() }).await;

        assert_eq!(events, vec![WorkerToShell::Ack { request_id }]);
        let saved: Option<String> = rig.ctx.store.get_setting("wake_word").expect("must succeed");
        assert_eq!(saved, Some("Eva".to_string()), "surrounding whitespace is trimmed");
    }

    #[tokio::test]
    async fn an_empty_wake_word_is_rejected() {
        let mut rig = Rig::new();
        let events = rig.run(ShellToWorker::SetWakeWord { request_id: Uuid::new_v4(), word: "  ".to_string() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert_eq!(rig.ctx.store.get_setting::<String>("wake_word").expect("must succeed"), None);
    }

    #[tokio::test]
    async fn health_reports_the_model_the_agents_the_formatter_and_the_config_warnings() {
        let registry = eva_agents::AgentRegistry::new(vec![Box::new(
            eva_agents::mock::MockProvider::always_completes("codex", "listo"),
        )]);
        let mut rig = Rig::builder()
            .agents(registry)
            .config_warnings(vec!["agents.priority: no conozco el agente \"x\"".into()])
            .build();
        let request_id = Uuid::new_v4();
        let events = rig.run(ShellToWorker::HealthCheck { request_id }).await;

        let Some(WorkerToShell::Health { report, .. }) =
            events.into_iter().find(|e| matches!(e, WorkerToShell::Health { .. }))
        else {
            panic!("a health report must come back");
        };
        assert!(!report.stt_model_loaded);
        assert!(report.store_ok);
        assert_eq!(report.formatter, "reglas");
        assert_eq!(report.config_warnings.len(), 1);
        assert_eq!(report.agents, vec![("codex".to_string(), "listo (mock)".to_string())]);
    }
}
