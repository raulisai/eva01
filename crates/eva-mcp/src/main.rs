//! The `eva-mcp` binary: runs [`eva_mcp::EvaMcpServer`] over stdio. This is
//! what an agent CLI is pointed at (`docs/PLAN.md` fase 8):
//!
//! ```text
//! claude --mcp-config '{"mcpServers":{"eva":{"command":"/path/to/eva-mcp",
//!   "args":["--gateway","/path/gateway.sock"],"env":{"EVA_GATEWAY_TOKEN":"…"}}}}'
//! ```
//!
//! With `--gateway <socket>` (what `eva-worker` injects into every agent it
//! launches) every tool call is forwarded to that running worker — which
//! rules on it, asks the user on the overlay when a policy says to, and
//! audits it. Without it, this process runs standalone: the same gateway
//! and the same policy from `config.toml`, but with nobody to ask, so
//! anything that needs a confirmation is refused rather than allowed.

use eva_config::{Config, Origin};
use eva_gateway::{DenyAll, Gateway};
use eva_mcp::{ConfiguredProjects, DesktopService, EvaMcpServer, LocalService, RemoteService, SystemDesktop};
use rmcp::transport::stdio;
use rmcp::ServiceExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // MCP's stdio transport reserves stdout for the protocol itself — logs
    // must go to stderr, never stdout, or they would corrupt the JSON-RPC
    // stream the agent CLI is reading.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let service: Arc<dyn DesktopService> = match gateway_socket_from_args() {
        Some(socket) => {
            let token = std::env::var("EVA_GATEWAY_TOKEN").unwrap_or_default();
            tracing::info!(socket = %socket.display(), "servidor MCP de EVA01, reenviando al worker");
            Arc::new(RemoteService::new(socket, token))
        }
        None => {
            tracing::info!("servidor MCP de EVA01 en modo independiente (sin nadie a quien pedirle confirmación)");
            Arc::new(standalone_service()?)
        }
    };

    let server = EvaMcpServer::new(service);
    let running = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("error al servir MCP: {e:?}");
    })?;

    running.waiting().await?;
    Ok(())
}

/// The value after `--gateway`, if given.
fn gateway_socket_from_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--gateway" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

fn standalone_service() -> Result<LocalService, Box<dyn std::error::Error>> {
    let loaded = Config::load();
    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    let config = loaded.config;

    let support = eva_config::support_dir();
    std::fs::create_dir_all(&support)?;
    let store = eva_store::Store::open(&support.join("eva.sqlite3"))?;

    let confirmer = Arc::new(DenyAll);
    let gateway = Arc::new(Gateway::new(config.gateway.clone(), store, confirmer.clone()));
    let projects = Arc::new(ConfiguredProjects::from_config(&config, std::env::current_dir()?));
    Ok(LocalService::new(
        Origin::Agent,
        gateway,
        Arc::new(SystemDesktop::with_voice(config.feedback.voice)),
        confirmer,
        projects,
        Duration::from_secs(config.gateway.confirm_timeout_secs()),
    ))
}
