//! Chooses which agent runs a task, per `docs/PLAN.md` fase 6: "Prioridad
//! Codex ▸ Claude Code, forzable por voz." This is the only place that
//! priority order is encoded — everything else in this crate treats every
//! provider identically through [`AgentProvider`].
//!
//! Also the one place that remembers a real [`crate::AgentEvent::RateLimit`]
//! reported during a run: a provider marked here is skipped by
//! [`AgentRegistry::candidates`] until its cooldown passes, so a voice
//! command right after Codex ran out of quota goes straight to Claude Code
//! instead of trying Codex again and waiting for it to fail — and, if the
//! user forces that exhausted agent by name, the answer is immediate and
//! names when it comes back, instead of spending a whole attempt to find out.

use crate::provider::{AgentProvider, ProviderStatus};
use chrono::{DateTime, Local, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use thiserror::Error;

/// How long a provider is skipped after a rate limit that named no reset
/// time — better than retrying it on the very next command.
const DEFAULT_COOLDOWN: chrono::Duration = chrono::Duration::minutes(30);

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
    /// The explicitly-requested provider is active but is known, from a
    /// recent real run, to be out of quota right now.
    #[error("{0} no tiene cuota ahora mismo: {1}")]
    RequestedProviderRateLimited(String, String),
    /// No provider, forced or otherwise, is active.
    #[error("no hay ningún agente disponible; revisa Settings → Agentes")]
    NoActiveProvider,
    /// Every provider that is installed and logged in is presently out of
    /// quota — different from [`DispatchError::NoActiveProvider`] because
    /// there is nothing to fix in Settings here, only a wait.
    #[error("ningún agente tiene cuota ahora mismo: {0}")]
    AllProvidersRateLimited(String),
}

/// A provider known to be rate-limited, and why — from a real
/// [`crate::AgentEvent::RateLimit`] seen on an actual run, never guessed.
#[derive(Debug, Clone)]
struct Cooldown {
    until: DateTime<Utc>,
    reason: String,
}

impl Cooldown {
    /// `reason`, plus when it lifts, e.g. "límite de 5 horas alcanzado
    /// (vuelve a las 16:50)".
    fn describe(&self) -> String {
        format!("{} (vuelve a las {})", self.reason, format_reset(self.until))
    }
}

/// `until`, in this Mac's local time: `"16:50"` for later today, `"03 oct
/// 09:00"` for another day (Claude's weekly window can reset days out).
fn format_reset(until: DateTime<Utc>) -> String {
    let local = until.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%d %b %H:%M").to_string()
    }
}

/// An ordered set of agent providers, queried in priority order.
pub struct AgentRegistry {
    /// Ordered highest-priority first, per `docs/PLAN.md` fase 6.
    providers: Vec<Box<dyn AgentProvider>>,
    /// Providers a real run has reported as out of quota, keyed by id.
    cooldowns: Mutex<HashMap<&'static str, Cooldown>>,
}

impl AgentRegistry {
    /// Builds a registry. `providers` is used in the order given — put the
    /// higher-priority provider first.
    pub fn new(providers: Vec<Box<dyn AgentProvider>>) -> Self {
        AgentRegistry { providers, cooldowns: Mutex::new(HashMap::new()) }
    }

    /// Remembers that `id` reported being out of quota, until `until` (or,
    /// if the CLI gave no reset time, [`DEFAULT_COOLDOWN`] from now).
    /// Idempotent to call again with a fresher signal — the latest report
    /// replaces the last one rather than stacking.
    pub fn mark_rate_limited(&self, id: &'static str, until: Option<DateTime<Utc>>, reason: String) {
        let until = until.unwrap_or_else(|| Utc::now() + DEFAULT_COOLDOWN);
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        self.cooldowns.lock().unwrap().insert(id, Cooldown { until, reason });
    }

    /// `id`'s current cooldown, if any and still in force — a cooldown whose
    /// time has passed is forgotten right here, so it never needs a separate
    /// sweep.
    fn cooldown_of(&self, id: &str) -> Option<Cooldown> {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        let mut cooldowns = self.cooldowns.lock().unwrap();
        match cooldowns.get(id) {
            Some(cooldown) if cooldown.until > Utc::now() => Some(cooldown.clone()),
            Some(_) => {
                cooldowns.remove(id);
                None
            }
            None => None,
        }
    }

    /// Picks the provider to run a task with: the first of
    /// [`Self::candidates`].
    ///
    /// # Errors
    /// See [`Self::candidates`].
    pub async fn select(&self, forced_id: Option<&str>) -> Result<&dyn AgentProvider, DispatchError> {
        let mut candidates = self.candidates(forced_id).await?;
        Ok(candidates.remove(0))
    }

    /// Every provider a task may run on, best first, so the caller can fall
    /// through to the next when one fails before doing anything — an agent
    /// that is installed and signed in but broken (an outdated CLI whose
    /// configured model it no longer accepts, say) must not make every voice
    /// command fail while a second, working agent sits unused.
    ///
    /// If `forced_id` is `Some` (the user said "usa Claude y…"), the result
    /// is that provider alone, and only if it is currently
    /// [`ProviderStatus::Active`] — an explicit choice is never silently
    /// replaced by another agent. Otherwise: every `Active` provider, in
    /// priority order.
    ///
    /// # Errors
    /// [`DispatchError::UnknownProvider`] if `forced_id` does not match any
    /// registered provider; [`DispatchError::RequestedProviderNotReady`] if
    /// it does but is not active; [`DispatchError::RequestedProviderRateLimited`]
    /// if it is active but a real run recently reported it out of quota;
    /// [`DispatchError::NoActiveProvider`] if no provider is forced and none
    /// are active; [`DispatchError::AllProvidersRateLimited`] if at least one
    /// is active but every active one is presently out of quota.
    pub async fn candidates(&self, forced_id: Option<&str>) -> Result<Vec<&dyn AgentProvider>, DispatchError> {
        if let Some(forced_id) = forced_id {
            let provider = self
                .providers
                .iter()
                .find(|p| p.id() == forced_id)
                .ok_or_else(|| DispatchError::UnknownProvider(forced_id.to_string()))?;

            if !provider.detect().await.is_active() {
                return Err(DispatchError::RequestedProviderNotReady(forced_id.to_string()));
            }
            return match self.cooldown_of(forced_id) {
                Some(cooldown) => {
                    Err(DispatchError::RequestedProviderRateLimited(forced_id.to_string(), cooldown.describe()))
                }
                None => Ok(vec![provider.as_ref()]),
            };
        }

        let mut active = Vec::new();
        let mut rate_limited = Vec::new();
        for provider in &self.providers {
            if !provider.detect().await.is_active() {
                continue;
            }
            match self.cooldown_of(provider.id()) {
                Some(cooldown) => rate_limited.push(format!("{}: {}", provider.id(), cooldown.describe())),
                None => active.push(provider.as_ref()),
            }
        }

        if !active.is_empty() {
            Ok(active)
        } else if !rate_limited.is_empty() {
            Err(DispatchError::AllProvidersRateLimited(rate_limited.join("; ")))
        } else {
            Err(DispatchError::NoActiveProvider)
        }
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
    async fn candidates_lists_every_active_provider_in_priority_order() {
        let registry = AgentRegistry::new(vec![
            Box::new(active("codex")),
            Box::new(inactive("gemini")),
            Box::new(active("claude_code")),
        ]);
        let ids: Vec<_> = registry.candidates(None).await.expect("two are active").iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["codex", "claude_code"]);
    }

    #[tokio::test]
    async fn a_forced_provider_is_the_only_candidate_and_is_never_replaced() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(active("claude_code"))]);
        let ids: Vec<_> =
            registry.candidates(Some("claude_code")).await.expect("active").iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["claude_code"]);
    }

    #[tokio::test]
    async fn candidates_with_nothing_active_is_the_no_provider_error() {
        let registry = AgentRegistry::new(vec![Box::new(inactive("codex"))]);
        assert!(matches!(registry.candidates(None).await, Err(DispatchError::NoActiveProvider)));
    }

    #[tokio::test]
    async fn a_rate_limited_provider_is_skipped_but_a_working_one_still_runs() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(active("claude_code"))]);
        registry.mark_rate_limited("codex", None, "límite de 5 horas alcanzado".to_string());
        let ids: Vec<_> =
            registry.candidates(None).await.expect("claude_code still active").iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["claude_code"]);
    }

    #[tokio::test]
    async fn every_provider_rate_limited_is_a_distinct_error_from_no_active_provider() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(active("claude_code"))]);
        let reset = Utc::now() + chrono::Duration::hours(1);
        registry.mark_rate_limited("codex", Some(reset), "límite de 5 horas alcanzado".to_string());
        registry.mark_rate_limited("claude_code", Some(reset), "límite semanal alcanzado".to_string());
        let Err(error) = registry.candidates(None).await else { panic!("both are cooling down") };
        assert!(matches!(error, DispatchError::AllProvidersRateLimited(_)));
        assert!(error.to_string().contains("codex") && error.to_string().contains("claude_code"), "{error}");
    }

    #[tokio::test]
    async fn forcing_a_rate_limited_provider_names_when_it_comes_back_instead_of_trying_it() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex"))]);
        let reset = Utc::now() + chrono::Duration::minutes(10);
        registry.mark_rate_limited("codex", Some(reset), "límite de 5 horas alcanzado".to_string());
        let Err(error) = registry.select(Some("codex")).await else { panic!("codex is cooling down") };
        assert!(matches!(error, DispatchError::RequestedProviderRateLimited(..)));
        assert!(error.to_string().contains("límite de 5 horas alcanzado"), "{error}");
    }

    #[tokio::test]
    async fn a_cooldown_that_has_passed_is_forgotten_and_the_provider_runs_again() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex"))]);
        registry.mark_rate_limited("codex", Some(Utc::now() - chrono::Duration::seconds(1)), "ya pasó".to_string());
        let ids: Vec<_> = registry.candidates(None).await.expect("cooldown expired").iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["codex"]);
    }

    #[tokio::test]
    async fn a_fresher_rate_limit_report_replaces_the_older_one() {
        let registry = AgentRegistry::new(vec![Box::new(active("codex")), Box::new(active("claude_code"))]);
        registry.mark_rate_limited("codex", Some(Utc::now() + chrono::Duration::hours(1)), "primero".to_string());
        registry.mark_rate_limited("codex", Some(Utc::now() - chrono::Duration::seconds(1)), "ya pasó".to_string());
        let ids: Vec<_> =
            registry.candidates(None).await.expect("the newer, expired report wins").iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["codex", "claude_code"]);
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
            mcp: None,
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let running = provider.execute(&task, tx).await.expect("execute must succeed");
        assert_eq!(running.wait().await, AgentOutcome::Completed { summary: Some("listo".to_string()) });
    }
}
