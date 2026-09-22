//! Chooses which agent runs a task, per `docs/PLAN.md` fase 6: "Prioridad
//! Codex ▸ Claude Code, forzable por voz." This is the only place that
//! priority order is encoded — everything else in this crate treats every
//! provider identically through [`AgentProvider`].

use crate::provider::{AgentProvider, ProviderStatus};
use thiserror::Error;

/// Why [`AgentRegistry::select`] could not return a provider to run a task.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DispatchError {
    /// Nothing in the registry is registered under that id at all.
    #[error("no se conoce el agente \"{0}\"")]
    UnknownProvider(String),
    /// The explicitly-requested provider ("usa Claude y…") is not currently
    /// usable.
    #[error("{0} no está listo ahora mismo")]
    RequestedProviderNotReady(String),
    /// No provider, forced or otherwise, is active.
    #[error("no hay ningún agente disponible; revisa Settings → Agentes")]
    NoActiveProvider,
}

/// An ordered set of agent providers, queried in priority order.
pub struct AgentRegistry {
    /// Ordered highest-priority first, per `docs/PLAN.md` fase 6.
    providers: Vec<Box<dyn AgentProvider>>,
}

impl AgentRegistry {
    /// Builds a registry. `providers` is used in the order given — put the
    /// higher-priority provider first.
    pub fn new(providers: Vec<Box<dyn AgentProvider>>) -> Self {
        AgentRegistry { providers }
    }

    /// Picks the provider to run a task with.
    ///
    /// If `forced_id` is `Some` (the user said "usa Claude y…"), that
    /// provider is used if and only if it is currently [`ProviderStatus::Active`].
    /// Otherwise, the first `Active` provider in priority order is used.
    ///
    /// # Errors
    /// [`DispatchError::UnknownProvider`] if `forced_id` does not match any
    /// registered provider; [`DispatchError::RequestedProviderNotReady`] if
    /// it does but is not active; [`DispatchError::NoActiveProvider`] if no
    /// provider is forced and none are active.
    pub async fn select(&self, forced_id: Option<&str>) -> Result<&dyn AgentProvider, DispatchError> {
        if let Some(forced_id) = forced_id {
            let provider = self
                .providers
                .iter()
                .find(|p| p.id() == forced_id)
                .ok_or_else(|| DispatchError::UnknownProvider(forced_id.to_string()))?;

            return if provider.detect().await.is_active() {
                Ok(provider.as_ref())
            } else {
                Err(DispatchError::RequestedProviderNotReady(forced_id.to_string()))
            };
        }

        for provider in &self.providers {
            if provider.detect().await.is_active() {
                return Ok(provider.as_ref());
            }
        }

        Err(DispatchError::NoActiveProvider)
    }

    /// Detects every registered provider's status, in priority order — the
    /// data behind the `eva doctor` health report (`docs/PLAN.md` §3.4) and
    /// the "Re-detectar" button.
    pub async fn detect_all(&self) -> Vec<(&'static str, ProviderStatus)> {
        let mut results = Vec::with_capacity(self.providers.len());
        for provider in &self.providers {
            results.push((provider.id(), provider.detect().await));
        }
        results
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::mock::MockProvider;
    use crate::provider::AgentOutcome;

    fn active(id: &'static str) -> MockProvider {
        MockProvider::new(id, ProviderStatus::Active { version: "1.0".into() }, Vec::new())
    }

    fn inactive(id: &'static str) -> MockProvider {
        MockProvider::new(id, ProviderStatus::NotInstalled, Vec::new())
    }

    #[tokio::test]
    async fn picks_the_first_active_provider_in_priority_order() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(active("claude_code"))]);
        let selected = registry.select(None).await.expect("must select codex");
        assert_eq!(selected.id(), "codex");
    }

    #[tokio::test]
    async fn falls_through_to_the_next_provider_if_the_first_is_not_active() {
        let registry = AgentRegistry::new(vec![Box::new(inactive("codex")), Box::new(active("claude_code"))]);
        let selected = registry.select(None).await.expect("must select claude_code");
        assert_eq!(selected.id(), "claude_code");
    }

    #[tokio::test]
    async fn no_active_provider_is_a_typed_error_not_a_panic() {
        let registry = AgentRegistry::new(vec![Box::new(inactive("codex")), Box::new(inactive("claude_code"))]);
        let result = registry.select(None).await;
        assert_eq!(result.err(), Some(DispatchError::NoActiveProvider));
    }

    #[tokio::test]
    async fn a_forced_provider_overrides_priority_order() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(active("claude_code"))]);
        let selected = registry.select(Some("claude_code")).await.expect("must select claude_code");
        assert_eq!(selected.id(), "claude_code");
    }

    #[tokio::test]
    async fn forcing_an_inactive_provider_is_an_error_not_a_silent_fallback() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(inactive("claude_code"))]);
        let result = registry.select(Some("claude_code")).await;
        assert_eq!(result.err(), Some(DispatchError::RequestedProviderNotReady("claude_code".to_string())));
    }

    #[tokio::test]
    async fn forcing_an_unknown_provider_id_is_a_typed_error() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex"))]);
        let result = registry.select(Some("gemini")).await;
        assert_eq!(result.err(), Some(DispatchError::UnknownProvider("gemini".to_string())));
    }

    #[tokio::test]
    async fn detect_all_reports_every_provider_in_order() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(inactive("claude_code"))]);
        let statuses = registry.detect_all().await;
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].0, "codex");
        assert!(statuses[0].1.is_active());
        assert_eq!(statuses[1].0, "claude_code");
        assert!(!statuses[1].1.is_active());
    }

    #[tokio::test]
    async fn selected_provider_can_actually_execute_a_task() {
        let registry = AgentRegistry::new(vec![Box::new(MockProvider::always_completes("codex", "listo"))]);
        let provider = registry.select(None).await.expect("must select codex");

        let task = crate::provider::AgentTask {
            prompt: "haz algo".to_string(),
            project_dir: std::env::temp_dir(),
            session_id: uuid::Uuid::new_v4(),
            resume_session_id: None,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let running = provider.execute(&task, tx).await.expect("execute must succeed");
        assert_eq!(running.wait().await, AgentOutcome::Completed { summary: Some("listo".to_string()) });
    }
}
