//! The `eva-mcp` binary: runs [`eva_mcp::EvaMcpServer`] over stdio. This is
//! what `--mcp-config` points an agent CLI at (`docs/PLAN.md` fase 8):
//!
//! ```text
//! claude --mcp-config '{"eva": {"command": "/path/to/eva-mcp"}}' ...
//! codex mcp add eva -- /path/to/eva-mcp
//! ```

use eva_mcp::{EvaMcpServer, SystemDesktop};
use rmcp::transport::stdio;
use rmcp::ServiceExt;
use std::sync::Arc;
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

    tracing::info!("iniciando el servidor MCP de EVA01");

    let server = EvaMcpServer::new(Arc::new(SystemDesktop));
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("error al servir MCP: {e:?}");
    })?;

    service.waiting().await?;
    Ok(())
}
