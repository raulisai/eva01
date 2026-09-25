//! Planning what no rule understood. "Abre YouTube y busca música chill" is
//! not one of the verbs `eva-intent` knows, and it is not a task for an agent
//! either: it is two small actions. The local model proposes the steps
//! (`eva_text::Planner`), this module checks every one against what the user
//! actually said and what exists on this Mac, shows them, and — on a yes —
//! runs them one by one through the gateway. Anything doubtful is no plan, and
//! the request goes to an agent as it always did.

use crate::commands::run_step;
use crate::context::WorkerContext;
use eva_intent::{Intent, Step};
use eva_ipc::WorkerState;
use eva_mcp::DesktopService;
use std::sync::Arc;
use uuid::Uuid;

/// Words that make a request worth asking the model about at all. A coding
/// task ("agrega tests al login") has none, so it never waits for the model.
const ACTION_WORDS: &[&str] = &[
    "abre",
    "abrir",
    "pon",
    "poner",
    "ponme",
    "busca",
    "buscar",
    "reproduce",
    "reproducir",
    "toca",
    "tocar",
    "musica",
    "cancion",
    "video",
    "videos",
    "youtube",
    "spotify",
    "escuchar",
    "ver",
    "pausa",
    "siguiente",
    "anterior",
    "playlist",
];
/// Longer than this is a paragraph, not a request for a couple of steps.
const MAX_SAID_CHARS: usize = 200;

fn worth_asking(said: &str) -> bool {
    said.chars().count() <= MAX_SAID_CHARS
        && eva_intent::intent::normalize_phrase(said).split(' ').any(|word| ACTION_WORDS.contains(&word))
}

/// Whether the app or address `target` of a step is something the user
/// mentioned: a word of it shares its start (or nearly its spelling) with a
/// word of what was said. A model that invents "Spotify" for "agrega tests"
/// is caught here.
fn grounded(said: &str, target: &str) -> bool {
    let said = eva_intent::intent::normalize_phrase(said);
    let words = |text: &str| -> Vec<String> {
        eva_intent::intent::normalize_phrase(text)
            .split(' ')
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_string)
            .collect()
    };
    let heard = words(&said);
    words(target).iter().any(|want| {
        heard.iter().any(|word| {
            let common = want.chars().zip(word.chars()).take_while(|(a, b)| a == b).count();
            common >= 4 || strsim::jaro_winkler(want, word) >= 0.9
        })
    })
}

/// The plan's lines if every one of them stands up: it is a step that exists,
/// an app that is installed or an address that opens, and nothing the user did
/// not ask for. `None` when any does not.
fn validated(ctx: &WorkerContext, said: &str, lines: &[String]) -> Option<Vec<String>> {
    let folded_said = eva_intent::intent::normalize_phrase(said);
    let mut steps: Vec<String> = Vec::new();
    for line in lines {
        let ok = match Step::parse(line) {
            Step::Open(target) => {
                let opens = matches!(
                    eva_intent::intent::parse(&format!("abre {target}"), &ctx.app_index.current()),
                    Intent::OpenApp { .. } | Intent::OpenUrl { .. }
                );
                opens && grounded(said, &target)
            }
            // A link is only ever the user's own words, never the model's.
            Step::Play(Some(target)) => folded_said.contains(&eva_intent::intent::normalize_phrase(&target)),
            _ => true,
        };
        if !ok {
            tracing::info!(%line, "paso del plan descartado: no cuadra con lo que se dijo o con esta Mac");
            return None;
        }
        if steps.last() != Some(line) {
            steps.push(line.clone());
        }
    }
    (!steps.is_empty()).then_some(steps)
}

/// Tries to turn `said` into steps and run them. `true` when it took over the
/// request (ran the plan, or the user said no to it); `false` when there is no
/// plan and the caller should do what it did before.
pub async fn try_plan(ctx: &Arc<WorkerContext>, request_id: Uuid, said: &str) -> bool {
    let settings = &ctx.config.resolver;
    if !settings.enabled || !settings.planner || !worth_asking(said) {
        return false;
    }
    let Some(planner) = eva_text::Planner::new(&settings.base_url, &settings.model) else { return false };
    ctx.events.state(request_id, WorkerState::Thinking);

    let apps: Vec<String> = ctx.app_index.current().names().into_iter().map(str::to_string).collect();
    let asked = said.to_string();
    let proposed = tokio::task::spawn_blocking(move || planner.plan(&asked, &apps)).await.unwrap_or_default();
    let Some(steps) = validated(ctx, said, &proposed) else { return false };
    tracing::info!(%said, ?steps, "plan propuesto por el modelo local");

    let intent_json = serde_json::json!({ "kind": "plan", "said": said, "steps": steps });
    let voice = ctx.voice.scoped_to_intent(intent_json);
    ctx.events.state(request_id, WorkerState::Executing);
    if settings.confirm_plans {
        let described: Vec<String> = steps.iter().map(|line| Step::parse(line).describe()).collect();
        let yes = voice.ask_confirmation("¿Hago esto?", &described.join("  →  ")).await.unwrap_or(false);
        if !yes {
            ctx.events.fail(request_id, "no hago nada: no confirmaste el plan");
            return true;
        }
    }

    let mut problems = Vec::new();
    for line in &steps {
        if let Err(problem) = run_step(ctx, &voice, line).await {
            problems.push(problem);
        }
    }
    if problems.is_empty() {
        ctx.events.state(request_id, WorkerState::Done(true));
    } else {
        ctx.events.fail(request_id, problems.join("; "));
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::{ShellToWorker, WorkerToShell};
    use eva_mcp::desktop::mock::Call;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A model on localhost that answers every request with `content`.
    fn model_saying(content: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 16384];
                let _ = stream.read(&mut buffer);
                let payload = json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).as_bytes(),
                );
            }
        });
        url
    }

    fn rig_with_model(content: &'static str, confirm: bool) -> Rig {
        let url = model_saying(content);
        let apps =
            crate::apps::AppCatalog::fixed(eva_intent::AppIndex::new(vec![eva_intent::AppEntry::new("Spotify")]));
        Rig::builder()
            .apps(apps)
            .configure(move |c| {
                c.resolver.enabled = true;
                c.resolver.base_url = url.clone();
                c.resolver.confirm_plans = confirm;
            })
            .build()
    }

    fn typed(text: &str) -> ShellToWorker {
        ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: text.to_string() }
    }

    #[test]
    fn only_requests_that_sound_like_actions_are_worth_the_model() {
        assert!(worth_asking("abre YouTube y busca música chill"));
        assert!(worth_asking("ponme algo de música"));
        assert!(!worth_asking("agrega tests al login"));
        assert!(!worth_asking(&format!("abre {}", "x".repeat(300))));
    }

    #[test]
    fn a_target_the_user_never_mentioned_is_not_grounded() {
        assert!(grounded("abre la calculadora", "Calculator"));
        assert!(grounded("abre brave y busca algo", "Brave Browser"));
        assert!(!grounded("agrega tests al login", "Spotify"));
    }

    #[tokio::test]
    async fn a_plan_is_shown_and_on_a_yes_each_step_runs_through_the_gateway() {
        let mut rig = rig_with_model(r#"{"pasos": ["Spotify", "siguiente:"]}"#, true);
        let events = rig.run_answering(typed("Adán, abre Spotify y pon la siguiente canción"), true).await;

        let asked = events.iter().find_map(|e| match e {
            WorkerToShell::ConfirmationRequested { detail, .. } => Some(detail.clone()),
            _ => None,
        });
        assert_eq!(asked.as_deref(), Some("Abrir Spotify  →  Siguiente canción"));
        assert_eq!(
            rig.desktop.calls(),
            vec![Call::OpenApp("Spotify".to_string()), Call::Media("siguiente canción".to_string())]
        );
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        let audit = rig.ctx.store.recent_audit(10).unwrap();
        assert!(audit.iter().all(|a| a.intent_json["intent"]["kind"] == "plan"), "the audit says a plan did it");
    }

    #[tokio::test]
    async fn a_no_runs_nothing_and_no_agent_is_started() {
        let mut rig = rig_with_model(r#"{"pasos": ["youtube: música chill"]}"#, true);
        let events = rig.run_answering(typed("Adán, abre youtube y busca música chill"), false).await;
        assert!(rig.desktop.calls().is_empty());
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::TaskStarted { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("no confirmaste"))));
    }

    #[tokio::test]
    async fn with_confirmation_off_the_plan_just_runs() {
        let mut rig = rig_with_model(r#"{"pasos": ["buscar: música chill"]}"#, false);
        let events = rig.run(typed("Adán, ponme música chill")).await;
        assert!(
            matches!(&rig.desktop.calls()[0], Call::OpenUrl(url) if url.contains("google.com/search") && url.contains("chill"))
        );
        assert!(events.iter().all(|e| !matches!(e, WorkerToShell::ConfirmationRequested { .. })), "asked nothing");
    }

    #[tokio::test]
    async fn a_plan_with_an_invented_app_is_no_plan_and_the_request_goes_to_an_agent() {
        let mut rig = rig_with_model(r#"{"pasos": ["Spotify"]}"#, true);
        let events = rig.run(typed("Adán, ver el estado del login")).await;
        assert!(rig.desktop.calls().is_empty(), "nothing ran: {:?}", rig.desktop.calls());
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::Error { .. } | WorkerToShell::TaskStarted { .. })),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_model_that_says_nothing_can_be_done_leaves_the_request_to_the_agent() {
        let mut rig = rig_with_model(r#"{"pasos": []}"#, true);
        let events = rig.run(typed("Adán, ponme al día con los tests")).await;
        assert!(rig.desktop.calls().is_empty());
        assert!(
            events.iter().all(|e| !matches!(e, WorkerToShell::ConfirmationRequested { .. })),
            "no plan, no question"
        );
    }
}
