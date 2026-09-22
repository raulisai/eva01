#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! EVA01's own MCP server: exposes a handful of desktop actions
//! (`docs/PLAN.md` fase 8) so any MCP-speaking agent — Codex, Claude Code,
//! whatever comes next — can open apps, paste text, and notify the user
//! through the exact same gateway a voice command would use. This is the
//! piece of `docs/PLAN.md` §3.1 that makes MCP "the open architecture"
//! rather than a bespoke plugin system.

pub mod desktop;
mod error;
pub mod server;

pub use desktop::{Desktop, SystemDesktop};
pub use error::DesktopError;
pub use server::EvaMcpServer;
