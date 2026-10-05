#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! Invokes coding-agent CLIs (Codex, Claude Code) and normalizes their
//! output. `eva-worker` never talks to a CLI directly — everything goes
//! through [`AgentProvider`], per `docs/PLAN.md` §3 and fase 6: "EVA no
//! orquesta agentes, los invoca bien."

pub mod capabilities;
pub mod claude_code;
pub mod codex;
pub mod dispatch;
pub mod event;
pub mod mock;
pub mod project_history;
pub mod provider;
mod stream;
pub mod worktree;

pub use capabilities::AgentCapabilities;
pub use claude_code::ClaudeCodeProvider;
pub use codex::CodexProvider;
pub use dispatch::{AgentRegistry, DispatchError};
pub use event::AgentEvent;
pub use provider::{AgentError, AgentOutcome, AgentProvider, AgentTask, McpInjection, ProviderStatus, RunningAgent};
pub use worktree::{Workspace, Worktree};

/// Builds the standard registry for production use: Codex first, then
/// Claude Code, matching the priority order in `docs/PLAN.md` fase 6.
pub fn default_registry() -> AgentRegistry {
    AgentRegistry::new(vec![Box::new(CodexProvider), Box::new(ClaudeCodeProvider)])
}

/// The standard registry, ordered by the user's `agents.priority` (ids
/// `"codex"` and `"claude_code"`). An id the user left out keeps its place
/// after the ones they named — it is still there for "usa Claude y…", just
/// never picked first — and an unknown id is ignored (the config validator
/// reports it).
pub fn registry_with_priority(priority: &[String]) -> AgentRegistry {
    let mut remaining: Vec<Box<dyn AgentProvider>> = vec![Box::new(CodexProvider), Box::new(ClaudeCodeProvider)];
    let mut ordered: Vec<Box<dyn AgentProvider>> = Vec::with_capacity(remaining.len());
    for id in priority {
        if let Some(position) = remaining.iter().position(|p| p.id() == id) {
            ordered.push(remaining.remove(position));
        }
    }
    ordered.extend(remaining);
    AgentRegistry::new(ordered)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    async fn ids(registry: &AgentRegistry) -> Vec<&'static str> {
        registry.detect_all().await.into_iter().map(|(id, _)| id).collect()
    }

    #[tokio::test]
    async fn the_default_order_is_codex_then_claude() {
        assert_eq!(ids(&default_registry()).await, vec!["codex", "claude_code"]);
    }

    #[tokio::test]
    async fn the_configured_priority_reorders_the_registry() {
        let registry = registry_with_priority(&["claude_code".to_string(), "codex".to_string()]);
        assert_eq!(ids(&registry).await, vec!["claude_code", "codex"]);
    }

    #[tokio::test]
    async fn an_agent_left_out_of_the_priority_still_follows_and_an_unknown_id_is_ignored() {
        let registry = registry_with_priority(&["gemini".to_string(), "claude_code".to_string()]);
        assert_eq!(ids(&registry).await, vec!["claude_code", "codex"]);
    }
}
