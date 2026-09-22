#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! Invokes coding-agent CLIs (Codex, Claude Code) and normalizes their
//! output. `eva-worker` never talks to a CLI directly — everything goes
//! through [`AgentProvider`], per `docs/PLAN.md` §3 and fase 6: "EVA no
//! orquesta agentes, los invoca bien."

pub mod claude_code;
pub mod codex;
pub mod dispatch;
pub mod event;
pub mod mock;
pub mod provider;

pub use claude_code::ClaudeCodeProvider;
pub use codex::CodexProvider;
pub use dispatch::{AgentRegistry, DispatchError};
pub use event::AgentEvent;
pub use provider::{AgentError, AgentOutcome, AgentProvider, AgentTask, ProviderStatus, RunningAgent};

/// Builds the standard registry for production use: Codex first, then
/// Claude Code, matching the priority order in `docs/PLAN.md` fase 6.
pub fn default_registry() -> AgentRegistry {
    AgentRegistry::new(vec![Box::new(CodexProvider), Box::new(ClaudeCodeProvider)])
}
