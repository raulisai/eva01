//! Manual smoke test: runs the real detection logic (no mocks) against
//! whatever agent CLIs are actually installed on this machine, and prints
//! what it finds. This is exactly what `eva doctor` (`docs/PLAN.md` §3.4)
//! does with the result, just without the rest of the health report yet.
//!
//! Run with: `cargo run --example detect -p eva-agents`

#[tokio::main]
async fn main() {
    let registry = eva_agents::default_registry();
    for (id, status) in registry.detect_all().await {
        println!("{id}: {status:?}");
    }
}
