//! Bringing a signed-out agent back. Claude Code's sign-in expires on its own
//! ("Failed to authenticate: OAuth session expired", seen on this Mac), and a
//! voice command must not just fail because of it: EVA01 says which agent lost
//! its session, asks once on screen whether to sign it in again, opens the
//! CLI's own sign-in in a Terminal window the user completes in the browser,
//! and waits for the agent to answer as signed in — then the task that needed
//! it goes on.
//!
//! EVA never handles the account itself: the CLI signs in, the user answers.

use crate::context::WorkerContext;
use eva_agents::ProviderStatus;
use eva_mcp::DesktopService;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a sign-in is waited for: enough to find the browser tab and
/// answer it, not so long that a forgotten one keeps EVA waiting.
const SIGN_IN_WAIT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(300) };
/// How often the agent is asked whether it is signed in yet.
const POLL: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(2) };

/// The sign-ins in progress, by provider: one at a time per agent, however
/// many commands needed it meanwhile.
#[derive(Default)]
pub struct Reconnects {
    in_progress: Mutex<HashSet<&'static str>>,
}

impl Reconnects {
    fn begin(&self, id: &'static str) -> bool {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        self.in_progress.lock().unwrap().insert(id)
    }

    fn end(&self, id: &'static str) {
        #[allow(clippy::unwrap_used)] // as above
        self.in_progress.lock().unwrap().remove(id);
    }
}

/// How a reconnect went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconnected {
    /// The agent answers as signed in again.
    Yes,
    /// The user said no, the sign-in was not completed in time, or another
    /// sign-in of the same agent is already under way.
    No,
}

/// Offers to sign `provider_id` in again, for request `request_id`. On a yes,
/// opens its sign-in and waits for it. Says how it went on the island.
pub async fn offer(ctx: &Arc<WorkerContext>, request_id: Uuid, provider_id: &str) -> Reconnected {
    let Some(provider) = ctx.agents.provider(provider_id) else { return Reconnected::No };
    let Some(command) = provider.sign_in_command() else { return Reconnected::No };
    let (id, name) = (provider.id(), provider.display_name());
    if !ctx.reconnects.begin(id) {
        return Reconnected::No;
    }
    let result = sign_in(ctx, request_id, id, name, command).await;
    ctx.reconnects.end(id);
    result
}

async fn sign_in(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    id: &'static str,
    name: &'static str,
    command: &'static [&'static str],
) -> Reconnected {
    let yes = ctx
        .voice
        .ask_confirmation(
            &format!("{name} perdió la sesión"),
            "¿Abro su inicio de sesión para reconectarlo? Se completa en el navegador.",
        )
        .await
        .unwrap_or(false);
    if !yes {
        return Reconnected::No;
    }

    let desktop = Arc::clone(&ctx.desktop);
    let title = format!("EVA01: inicia sesión en {name}. Sigue los pasos en el navegador.");
    let opened = tokio::task::spawn_blocking(move || desktop.open_sign_in(&title, command)).await;
    if !matches!(opened, Ok(Ok(()))) {
        ctx.events.error(request_id, format!("no pude abrir el inicio de sesión de {name}"));
        return Reconnected::No;
    }
    tracing::info!(provider = id, "inicio de sesión abierto; esperando a que el agente responda con sesión");
    ctx.events.notice(request_id, format!("Esperando a que inicies sesión en {name}…"));

    let deadline = Instant::now() + SIGN_IN_WAIT;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        let signed_in = match ctx.agents.provider(id) {
            Some(provider) => matches!(provider.detect().await, ProviderStatus::Active { .. }),
            None => false,
        };
        if signed_in {
            tracing::info!(provider = id, "agente reconectado");
            ctx.events.notice(request_id, format!("{name} está conectado otra vez"));
            return Reconnected::Yes;
        }
    }
    tracing::warn!(provider = id, "el inicio de sesión no se completó a tiempo");
    ctx.events.error(request_id, format!("no se completó el inicio de sesión de {name}"));
    Reconnected::No
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::{registry_of, Rig};
    use eva_agents::mock::MockProvider;
    use eva_mcp::desktop::mock::Call;

    fn signed_out(id: &'static str) -> Arc<MockProvider> {
        Arc::new(MockProvider::new(id, ProviderStatus::InstalledNoSession { version: None }, Vec::new()).with_sign_in())
    }

    #[tokio::test]
    async fn a_yes_opens_the_cli_sign_in_and_waits_until_the_agent_answers_signed_in() {
        let claude = signed_out("claude_code");
        let mut rig = Rig::builder().agents(registry_of(&[&claude])).build();
        let flips = Arc::clone(&claude);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            flips.set_status(ProviderStatus::Active { version: "mock".into() });
        });

        let ctx = Arc::clone(&rig.ctx);
        let answer = tokio::spawn(async move { offer(&ctx, Uuid::new_v4(), "claude_code").await });
        rig.answer_next_confirmation(true).await;

        assert_eq!(answer.await.unwrap(), Reconnected::Yes);
        assert_eq!(rig.desktop.calls(), vec![Call::OpenSignIn("mock login".to_string())]);
    }

    #[tokio::test]
    async fn a_no_opens_nothing() {
        let claude = signed_out("claude_code");
        let mut rig = Rig::builder().agents(registry_of(&[&claude])).build();
        let ctx = Arc::clone(&rig.ctx);
        let answer = tokio::spawn(async move { offer(&ctx, Uuid::new_v4(), "claude_code").await });
        rig.answer_next_confirmation(false).await;

        assert_eq!(answer.await.unwrap(), Reconnected::No);
        assert!(rig.desktop.calls().is_empty());
    }

    #[tokio::test]
    async fn a_sign_in_never_completed_gives_up_and_says_so() {
        let claude = signed_out("claude_code");
        let mut rig = Rig::builder().agents(registry_of(&[&claude])).build();
        let ctx = Arc::clone(&rig.ctx);
        let answer = tokio::spawn(async move { offer(&ctx, Uuid::new_v4(), "claude_code").await });
        rig.answer_next_confirmation(true).await;

        assert_eq!(answer.await.unwrap(), Reconnected::No);
        let events = rig.until(|e| matches!(e, eva_ipc::WorkerToShell::Error { .. })).await;
        assert!(
            events.iter().any(
                |e| matches!(e, eva_ipc::WorkerToShell::Error { message, .. } if message.contains("no se completó"))
            ),
            "{events:?}"
        );
    }
}
