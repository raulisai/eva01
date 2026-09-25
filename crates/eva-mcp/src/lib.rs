#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! EVA01's own MCP server: exposes ten desktop tools (`docs/PLAN.md`
//! fase 8) so any MCP-speaking agent — Codex, Claude Code, whatever comes
//! next — can open apps, paste text, read the selection, ask the user a
//! question and notify them, through the exact same gateway a voice command
//! goes through. This is the piece of `docs/PLAN.md` §3.1 that makes MCP
//! "the open architecture" rather than a bespoke plugin system.
//!
//! Layers, from the wire inward: [`server`] speaks MCP and holds no logic;
//! [`service`] is the async list of operations, with [`service::LocalService`]
//! ruling on each through `eva-gateway` before [`desktop`] performs it;
//! [`remote`] is how an agent's `eva-mcp` child reaches the running
//! `eva-worker` (and its overlay) instead of acting on its own.

pub mod desktop;
mod error;
pub mod projects;
pub mod remote;
pub mod server;
pub mod service;
pub mod youtube;

pub use desktop::{Desktop, SystemDesktop};
pub use error::DesktopError;
pub use projects::ConfiguredProjects;
pub use remote::{serve as serve_gateway_socket, RemoteService};
pub use server::EvaMcpServer;
pub use service::{app_store_search_url, DesktopService, LocalService, Outcome, ProjectSource, ServiceError};
