//! Running a recognized voice command. Every action goes through the gateway
//! (`ctx.voice`), so the same policy, confirmation and audit that rule an
//! agent's tool calls rule the user's own words.

use crate::context::WorkerContext;
use eva_intent::Intent;
use eva_ipc::{WorkerState, WorkerToShell};
use eva_mcp::{DesktopService, LocalService, Outcome, ServiceError};
use eva_store::Decision;
use std::sync::Arc;
use uuid::Uuid;

/// Runs `intent`, reporting through `ctx.events`.
pub async fn run_intent(ctx: &Arc<WorkerContext>, request_id: Uuid, intent: Intent) {
    let intent_json = serde_json::to_value(&intent).unwrap_or(serde_json::Value::Null);
    ctx.events.emit(WorkerToShell::IntentRecognized { request_id, intent_json: intent_json.clone() });

    let voice = ctx.voice.scoped_to_intent(intent_json.clone());
    match intent {
        // `interpret` returns a `Command` only after the wake word matched,
        // so this cannot come out of the normal path — logged and treated
        // as a no-op rather than guessed at.
        Intent::Dictation => {
            tracing::warn!("Intent::Dictation llegó a run_intent; esto no debería pasar, se ignora");
            ctx.events.state(request_id, WorkerState::Idle);
        }
        Intent::Blocked { matched_stem, text } => {
            tracing::warn!(%matched_stem, %text, "comando bloqueado por la lista negra de verbos destructivos");
            match ctx.store.log_decision(None, &intent_json, Decision::Blocked) {
                Ok(audit_id) => {
                    let _ = ctx.store.record_audit_result(audit_id, "bloqueado: verbo destructivo detectado");
                }
                Err(e) => tracing::warn!("no se pudo escribir la auditoría del bloqueo: {e}"),
            }
            ctx.events.fail(request_id, format!("no ejecuto eso: contiene el verbo bloqueado \"{matched_stem}\""));
        }
        Intent::OpenApp { app } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.open_app(&app).await);
        }
        Intent::CloseApp { app } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.close_app(&app).await);
        }
        Intent::OpenUrl { url } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.open_url(&url).await);
        }
        Intent::WebSearch { query } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.web_search(&query).await);
        }
        Intent::SearchInSite { site, query, url, focus } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, search_in_site(&voice, &site, &query, &url, &focus).await);
        }
        Intent::ClickUi { label } => {
            ctx.events.state(request_id, WorkerState::Executing);
            report(ctx, request_id, voice.click_ui(&label).await);
        }
        Intent::Media { step } => {
            ctx.events.state(request_id, WorkerState::Executing);
            let outcome = run_step(ctx, &voice, &step).await;
            match outcome {
                Ok(()) => ctx.events.state(request_id, WorkerState::Done(true)),
                Err(message) => ctx.events.fail(request_id, message),
            }
        }
        Intent::ConfirmApp { heard, app, opening } => confirm_app(ctx, request_id, &heard, &app, opening, &voice).await,
        Intent::AppNotFound { name, opening } => app_not_found(ctx, request_id, &name, opening, &voice).await,
        Intent::Custom { phrase } => run_custom(ctx, request_id, &phrase, intent_json).await,
        Intent::ConfirmCustom { heard, phrase } => confirm_custom(ctx, request_id, &heard, &phrase, &voice).await,
        Intent::AgentTask { prompt, provider } => {
            // No rule took it: before it becomes a task for an agent, see whether
            // it is really a couple of small actions the local model can plan.
            if provider.is_none() && crate::plan::try_plan(ctx, request_id, &prompt).await {
                return;
            }
            crate::tasks::start_new(ctx, request_id, intent_json, prompt, provider).await;
        }
        Intent::ContinueAgentTask { extra_prompt } => {
            crate::tasks::continue_last(ctx, request_id, intent_json, extra_prompt).await;
        }
        Intent::EditSelection { instruction } => {
            crate::dictation::edit_selection(ctx, request_id, instruction, intent_json).await;
        }
    }
}

/// "Abre Spotifly": asks "¿quisiste decir Spotify?". A yes does it and
/// remembers how the user says it, so that name needs no question again; a
/// no does nothing and remembers nothing.
async fn confirm_app(
    ctx: &WorkerContext,
    request_id: Uuid,
    heard: &str,
    app: &str,
    opening: bool,
    voice: &impl DesktopService,
) {
    ctx.events.state(request_id, WorkerState::Executing);
    let yes =
        voice.ask_confirmation(&format!("¿Quisiste decir «{app}»?"), &format!("Oí «{heard}»")).await.unwrap_or(false);
    if !yes {
        ctx.events.fail(request_id, format!("no abro nada: «{heard}» no era «{app}»"));
        return;
    }
    learn_app(ctx, heard, app);
    run_app(ctx, request_id, app, opening, voice).await;
}

/// Remembers, for good, that this is how the user says `app`.
fn learn_app(ctx: &WorkerContext, heard: &str, app: &str) {
    let folded = eva_text::fold_diacritics(heard).to_lowercase();
    if let Err(e) = ctx.store.learn_app_alias(&folded, app) {
        tracing::warn!("no se pudo guardar que «{heard}» es «{app}»: {e}");
    }
    ctx.app_index.learn(heard, app);
    tracing::info!(%heard, %app, "aprendido: así dices esta app");
}

async fn run_app(ctx: &WorkerContext, request_id: Uuid, app: &str, opening: bool, voice: &impl DesktopService) {
    ctx.events.state(request_id, WorkerState::Executing);
    let outcome = if opening { voice.open_app(app).await } else { voice.close_app(app).await };
    report(ctx, request_id, outcome);
}

/// Whether `app` is spelled or sounds enough like `heard` that a model's pick
/// can be trusted without asking: the same first consonant and a similar
/// sound overall. Anything further is a question.
fn plausible(heard: &str, app: &str) -> bool {
    let said = eva_intent::sound::sound_of(heard);
    // The whole name, or any one word of it ("Brave" in "Brave Browser").
    std::iter::once(app).chain(app.split_whitespace()).any(|part| {
        let name = eva_intent::sound::sound_of(part);
        said.consonants.chars().next() == name.consonants.chars().next()
            && strsim::jaro_winkler(&said.full, &name.full) >= 0.7
    })
}

/// What the quick matching could not find, asked of the local model: which of
/// the installed apps was meant. `None` when it is off, silent or unsure.
async fn ask_model(ctx: &WorkerContext, name: &str) -> Option<String> {
    let settings = &ctx.config.resolver;
    if !settings.enabled {
        return None;
    }
    let resolver = eva_text::AppResolver::new(&settings.base_url, &settings.model)?;
    let apps: Vec<String> = ctx.app_index.current().names().into_iter().map(str::to_string).collect();
    let heard = name.to_string();
    tokio::task::spawn_blocking(move || resolver.resolve(&heard, &apps)).await.ok().flatten()
}

/// "Abre Photoshop" without Photoshop: says so, and — for opening — offers to
/// look for it in the App Store. The offer is a question on the overlay
/// answered with a key, like any other confirmation; only a yes opens the
/// App Store, and it opens on the search, not on anything to buy.
async fn app_not_found(ctx: &WorkerContext, request_id: Uuid, name: &str, opening: bool, voice: &impl DesktopService) {
    // What was said may be an installed app said badly ("braille" for Brave).
    // The local model, if there is one, knows the apps and can tell; without
    // it, the closest by sound. A candidate that resembles what was said is
    // trusted and remembered, so it is never asked twice; a far one is asked
    // about first.
    ctx.events.state(request_id, WorkerState::Executing);
    let by_model = ask_model(ctx, name).await;
    let candidate =
        by_model.clone().or_else(|| ctx.app_index.current().suggest(name).map(|a| a.canonical_name.clone()));
    if let Some(app) = candidate {
        tracing::info!(heard = %name, %app, model = by_model.is_some(), "app propuesta para lo que se oyó");
        if plausible(name, &app) {
            learn_app(ctx, name, &app);
            run_app(ctx, request_id, &app, opening, voice).await;
        } else {
            confirm_app(ctx, request_id, name, &app, opening, voice).await;
        }
        return;
    }
    if !opening {
        ctx.events.fail(request_id, format!("no encuentro «{name}» entre tus aplicaciones"));
        return;
    }
    ctx.events.state(request_id, WorkerState::Executing);
    let wants_store = voice
        .ask_confirmation(&format!("«{name}» no está instalada"), "¿La busco en la App Store para instalarla?")
        .await
        .unwrap_or(false);
    if !wants_store {
        ctx.events.fail(request_id, format!("«{name}» no está instalada"));
        return;
    }
    report(ctx, request_id, voice.open_url(&eva_mcp::app_store_search_url(name)).await);
}

/// The command the user's `phrase` means: the one that has it among its own
/// phrases, or the one a phrase learned by voice or confirmation points to.
fn command_for(ctx: &WorkerContext, phrase: &str) -> Option<eva_config::CommandConfig> {
    let commands = ctx.commands.runnable();
    if let Some(command) = commands.iter().find(|c| c.phrases().any(|say| say == phrase)) {
        return Some(command.clone());
    }
    let learned = ctx.store.list_command_phrases().unwrap_or_default();
    let meant = &learned.iter().find(|l| l.phrase == phrase)?.command;
    commands.into_iter().find(|c| c.say == *meant)
}

/// "Adán, ponme mi canal": close to one of the user's phrases without being
/// it. Asks "¿Quisiste decir «ver mi canal»?"; a yes runs it and remembers
/// this way of saying it (so it is never asked twice), a no does nothing and
/// remembers nothing — the same road an app name walks in [`confirm_app`].
async fn confirm_custom(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    heard: &str,
    phrase: &str,
    voice: &impl DesktopService,
) {
    ctx.events.state(request_id, WorkerState::Executing);
    let Some(command) = command_for(ctx, phrase) else {
        ctx.events.fail(request_id, format!("la orden «{phrase}» ya no está en la configuración"));
        return;
    };
    let yes = voice
        .ask_confirmation(&format!("¿Quisiste decir «{}»?", command.say), &format!("Oí «{heard}»"))
        .await
        .unwrap_or(false);
    if !yes {
        ctx.events.fail(request_id, format!("no hago nada: «{heard}» no era «{}»", command.say));
        return;
    }
    let phrase = eva_intent::intent::normalize_phrase(heard);
    match ctx.store.learn_command_phrase(&phrase, &command.say, "confirmed") {
        Ok(()) => tracing::info!(%phrase, command = %command.say, "aprendido: así dices esta orden"),
        Err(e) => tracing::warn!("no se pudo guardar que «{heard}» es «{}»: {e}", command.say),
    }
    let now_custom = serde_json::to_value(Intent::Custom { phrase: phrase.clone() }).unwrap_or(serde_json::Value::Null);
    run_command(ctx, request_id, &command, now_custom).await;
}

/// "Busca Naruto" with YouTube in front: types it into the site's own search
/// box. If the site is not what is in front any more (the user went elsewhere
/// in the seconds since), the search page opens instead, as it always did.
async fn search_in_site(voice: &LocalService, site: &str, query: &str, url: &str, focus: &str) -> Outcome<()> {
    let in_front = match voice.active_window().await {
        Ok(Some(window)) => {
            let folded = |text: &str| eva_text::fold_diacritics(text).to_lowercase();
            let wanted = folded(site);
            [window.name.as_deref(), window.title.as_deref()]
                .into_iter()
                .flatten()
                .any(|shown| folded(shown).contains(&wanted))
        }
        _ => false,
    };
    if in_front {
        voice.search_in_place(focus, query).await
    } else {
        voice.open_url(url).await
    }
}

/// Runs one of the user's own `[[commands]]`. Each thing it does goes through
/// the gateway, so `[gateway.voice]` rules it like any other command.
async fn run_custom(ctx: &Arc<WorkerContext>, request_id: Uuid, phrase: &str, intent_json: serde_json::Value) {
    match command_for(ctx, phrase) {
        Some(command) => run_command(ctx, request_id, &command, intent_json).await,
        None => ctx.events.fail(request_id, format!("la orden «{phrase}» ya no está en la configuración")),
    }
}

async fn run_command(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    command: &eva_config::CommandConfig,
    intent_json: serde_json::Value,
) {
    use eva_config::CommandAction;

    let phrase = command.say.as_str();
    match command.action() {
        Ok(CommandAction::Insert(text)) => crate::dictation::insert_text(ctx, request_id, text, intent_json).await,
        Ok(CommandAction::Task(prompt)) => {
            crate::tasks::start_new(ctx, request_id, intent_json, prompt.to_string(), None).await;
        }
        Ok(CommandAction::Open(targets)) => {
            ctx.events.state(request_id, WorkerState::Executing);
            let voice = ctx.voice.scoped_to_intent(intent_json);
            let mut problems = Vec::new();
            for target in targets {
                if let Err(problem) = run_step(ctx, &voice, target).await {
                    problems.push(problem);
                }
            }
            if problems.is_empty() {
                ctx.events.state(request_id, WorkerState::Done(true));
            } else {
                ctx.events.fail(request_id, problems.join("; "));
            }
        }
        Err(why) => ctx.events.fail(request_id, format!("la orden «{phrase}» no se puede ejecutar: {why}")),
    }
}

/// Does one step of a command: opens an app or address the way "Adán, abre …"
/// would — same grammar, same app aliases, same URL detection, so a command
/// cannot understand a name differently from a spoken one — or searches,
/// plays a video, controls the music. Each goes through the gateway.
pub(crate) async fn run_step(ctx: &WorkerContext, voice: &eva_mcp::LocalService, line: &str) -> Result<(), String> {
    use eva_intent::Step;
    use eva_macos::MediaCommand;

    let outcome = match Step::parse(line) {
        Step::Search(query) => voice.web_search(&query).await,
        Step::Youtube(query) => voice.youtube_play(&query).await,
        Step::Play(target) => voice.media(MediaCommand::Play(target)).await,
        Step::Pause => voice.media(MediaCommand::Pause).await,
        Step::Next => voice.media(MediaCommand::Next).await,
        Step::Previous => voice.media(MediaCommand::Previous).await,
        Step::Open(target) => match eva_intent::intent::parse(&format!("abre {target}"), &ctx.app_index.current()) {
            Intent::OpenApp { app } => voice.open_app(&app).await,
            Intent::OpenUrl { url } => voice.open_url(&url).await,
            _ => return Err(format!("no encontré «{target}» entre tus apps ni parece una dirección web")),
        },
    };
    outcome.map_err(|e| match e {
        ServiceError::Refused(message) | ServiceError::Failed(message) => message,
    })
}

/// Turns the outcome of a gated action into the request's final events.
fn report(ctx: &WorkerContext, request_id: Uuid, outcome: Outcome<()>) {
    match outcome {
        Ok(()) => ctx.events.state(request_id, WorkerState::Done(true)),
        Err(ServiceError::Refused(message) | ServiceError::Failed(message)) => ctx.events.fail(request_id, message),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;
    use eva_ipc::ShellToWorker;
    use eva_mcp::desktop::mock::{Call, MockDesktop};

    fn typed(text: &str) -> ShellToWorker {
        ShellToWorker::RunIntentText { request_id: Uuid::new_v4(), text: text.to_string() }
    }

    /// A rig whose config has the user's own commands, written as in `config.toml`.
    fn with_commands(toml: &str) -> crate::testkit::RigBuilder {
        let config = eva_config::Config::parse(toml).expect("a valid config");
        Rig::builder().configure(move |c| c.commands = config.commands.clone())
    }

    #[tokio::test]
    async fn a_custom_insert_pastes_exactly_the_users_text() {
        let mut rig = with_commands("[[commands]]\nsay = \"mi correo\"\ninsert = \"yo@ejemplo.com\"").build();
        let events = rig.run(typed("Adán, mi correo.")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![Call::InsertText("yo@ejemplo.com".to_string())],
            "no trailing space, no formatting"
        );
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        let audit = rig.ctx.store.recent_audit(10).unwrap();
        assert_eq!(audit[0].intent_json["intent"]["kind"], "custom", "the audit trail says it was a custom command");
        assert_eq!(audit[0].intent_json["intent"]["phrase"], "mi correo");
    }

    #[tokio::test]
    async fn a_custom_open_opens_apps_and_addresses_in_order_with_the_same_names_as_speech() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"modo enfoque\"\nopen = [\"brave\", \"https://ejemplo.com/agenda\"]")
                .build();
        let events = rig.run(typed("adan modo enfoque")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![Call::OpenApp("Brave Browser".to_string()), Call::OpenUrl("https://ejemplo.com/agenda".to_string())]
        );
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn a_custom_open_with_an_unknown_app_does_the_rest_and_says_which_one_failed() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"modo enfoque\"\nopen = [\"Aplicación Que No Existe\", \"brave\"]")
                .build();
        let events = rig.run(typed("Adán, modo enfoque")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())], "the others still open");
        assert!(
            events.iter().any(
                |e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("Aplicación Que No Existe"))
            ),
            "{events:?}"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[tokio::test]
    async fn a_search_step_searches_the_web_between_the_things_it_opens() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"mi canal\"\nopen = [\"brave\", \"buscar: lofi hip hop\", \"https://youtube.com/@mio\"]")
                .build();
        rig.run(typed("Adán, mi canal")).await;

        let calls = rig.desktop.calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(calls[0], Call::OpenApp("Brave Browser".to_string()));
        assert!(matches!(&calls[1], Call::OpenUrl(url) if url.contains("google.com/search") && url.contains("lofi")));
        assert_eq!(calls[2], Call::OpenUrl("https://youtube.com/@mio".to_string()));
    }

    #[tokio::test]
    async fn a_phrase_close_to_a_command_asks_and_a_yes_runs_it_and_remembers_the_way_it_was_said() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"ver mi canal favorito\"\nopen = [\"https://youtube.com/@mio\"]")
                .build();
        let events = rig.run_answering(typed("Adán, ponme mi canal favorito"), true).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenUrl("https://youtube.com/@mio".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        let learned = rig.ctx.store.list_command_phrases().unwrap();
        assert_eq!(learned.len(), 1);
        assert_eq!(
            (learned[0].phrase.as_str(), learned[0].command.as_str()),
            ("ponme mi canal favorito", "ver mi canal favorito")
        );

        // The next time it is exact: `run` answers no confirmation, so a
        // question would have left this run without a second call.
        rig.run(typed("adan, Ponme mi canal favorito.")).await;
        assert_eq!(rig.desktop.calls().len(), 2, "no question the second time: it just runs");
    }

    #[tokio::test]
    async fn a_no_to_the_question_does_nothing_and_remembers_nothing() {
        let mut rig =
            with_commands("[[commands]]\nsay = \"ver mi canal favorito\"\nopen = [\"https://youtube.com/@mio\"]")
                .build();
        let events = rig.run_answering(typed("Adán, ponme mi canal favorito"), false).await;

        assert!(rig.desktop.calls().is_empty());
        assert!(rig.ctx.store.list_command_phrases().unwrap().is_empty());
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn controls_of_the_music_and_a_command_with_music_steps_go_through_the_gateway() {
        let mut rig = with_commands(
            "[[commands]]\nsay = \"pon musica chill\"\nopen = [\"spotify\", \"reproducir: spotify:playlist:37i9dQZF1DXcBWIGoYBM5M\", \"siguiente:\"]",
        )
        .build();
        rig.run(typed("Adán, pausa la música")).await;
        rig.run(typed("Adán, siguiente canción")).await;
        rig.run(typed("Adán, dale play")).await;
        rig.run(typed("Adán, pon música chill")).await;

        let calls = rig.desktop.calls();
        assert_eq!(calls[0], Call::Media("pausar la música".to_string()));
        assert_eq!(calls[1], Call::Media("siguiente canción".to_string()));
        assert_eq!(calls[2], Call::Media("reanudar la música".to_string()));
        assert!(
            calls.contains(&Call::Media("reproducir spotify:playlist:37i9dQZF1DXcBWIGoYBM5M".to_string())),
            "{calls:?}"
        );
        let audit = rig.ctx.store.recent_audit(20).unwrap();
        assert!(audit.iter().any(|a| a.intent_json["action"] == "media"), "the audit says it was music");
    }

    #[tokio::test]
    async fn the_gateway_policy_rules_custom_commands_too() {
        let mut rig = with_commands("[[commands]]\nsay = \"mi correo\"\ninsert = \"yo@ejemplo.com\"")
            .configure(|c| {
                c.gateway.voice.insert(eva_config::ActionKind::InsertText, eva_config::Policy::Block);
            })
            .build();
        let events = rig.run(typed("Adán, mi correo")).await;

        assert!(rig.desktop.calls().is_empty(), "a blocked action never reaches the desktop");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn a_custom_task_goes_to_the_agent_with_exactly_the_written_prompt() {
        use crate::testkit::registry_of;
        let codex = std::sync::Arc::new(eva_agents::mock::MockProvider::always_completes("codex", "listo"));
        let mut rig =
            with_commands("[[commands]]\nsay = \"revisa los tests\"\ntask = \"corre los tests y dime cuáles fallan\"")
                .agents(registry_of(&[&codex]))
                .build();
        let events = rig.run(typed("Adán, revisa los tests")).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::TaskStarted { prompt, .. } if prompt == "corre los tests y dime cuáles fallan")), "{events:?}");
    }

    #[tokio::test]
    async fn interpreting_a_text_says_what_it_is_and_does_none_of_it() {
        let mut rig = with_commands("[[commands]]\nsay = \"mi correo\"\ninsert = \"yo@ejemplo.com\"").build();
        let interpret =
            |text: &str| ShellToWorker::InterpretText { request_id: Uuid::new_v4(), text: text.to_string() };
        let kind = |events: &[WorkerToShell]| {
            events.iter().find_map(|e| match e {
                WorkerToShell::IntentRecognized { intent_json, .. } => {
                    Some(intent_json["kind"].as_str().unwrap().to_string())
                }
                _ => None,
            })
        };

        let events = rig.run(interpret("Adán, abre brave")).await;
        assert_eq!(kind(&events).as_deref(), Some("open_app"));
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Ack { .. })), "the request ends");

        assert_eq!(kind(&rig.run(interpret("Adán, mi correo")).await).as_deref(), Some("custom"));
        assert_eq!(kind(&rig.run(interpret("hola mundo")).await).as_deref(), Some("dictation"));
        assert_eq!(kind(&rig.run(interpret("Adán, borra el proyecto")).await).as_deref(), Some("blocked"));

        assert!(rig.desktop.calls().is_empty(), "nothing was opened, pasted or asked");
        assert!(rig.ctx.store.recent_audit(10).unwrap().is_empty(), "and the gateway never heard of it");
    }

    #[tokio::test]
    async fn a_broken_custom_command_never_runs_and_the_words_fall_back_to_the_normal_rules() {
        // No action at all: skipped, so "abre brave" is the ordinary command.
        let mut rig = with_commands("[[commands]]\nsay = \"abre brave\"").build();
        rig.run(typed("Adán, abre brave")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
    }

    #[tokio::test]
    async fn a_misheard_app_name_is_confirmed_once_and_then_understood_without_asking() {
        let apps =
            crate::apps::AppCatalog::fixed(eva_intent::AppIndex::new(vec![eva_intent::AppEntry::new("Spotify")]));
        let mut rig = Rig::builder().apps(apps).build();

        let declined = rig.run_answering(typed("Adán, abre Spotifly"), false).await;
        assert!(
            declined
                .iter()
                .any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("Spotify"))),
            "{declined:?}"
        );
        assert!(rig.desktop.calls().is_empty(), "a no opens nothing");
        assert!(rig.ctx.store.list_app_aliases().unwrap().is_empty(), "and teaches nothing");

        rig.run_answering(typed("Adán, abre Spotifly"), true).await;
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Spotify".to_string())]);
        assert_eq!(rig.ctx.store.list_app_aliases().unwrap(), vec![("spotifly".to_string(), "Spotify".to_string())]);

        let again = rig.run(typed("Adán, abre Spotifly")).await;
        assert!(!again.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })), "no second question");
        assert_eq!(rig.desktop.calls().len(), 2);
    }

    #[tokio::test]
    async fn a_wake_word_heard_as_adam_works_for_commands_and_is_learned_for_tasks_after_a_few_times() {
        let mut rig = Rig::new();

        rig.run(typed("Adam, abre Brave")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);

        // A task after a look-alike word is not started on a guess…
        let events = rig.run(typed("Adam agrega tests al login")).await;
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::TaskStarted { .. })), "{events:?}");
        assert_eq!(rig.ctx.store.trusted_wake_variants(3).unwrap(), Vec::<String>::new());

        // …until the same spelling has been taken for the wake word enough times.
        rig.run(typed("Adam, abre Brave")).await;
        rig.run(typed("Adam, abre Brave")).await;
        assert_eq!(rig.ctx.store.trusted_wake_variants(3).unwrap(), vec!["adam".to_string()]);
        let interpreted = crate::dictation::interpret_text_for_test(&rig.ctx, "Adam agrega tests al login");
        assert_eq!(interpreted["kind"], "agent_task");
    }

    #[tokio::test]
    async fn a_click_names_a_label_and_the_desktop_does_the_pressing() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, haz clic en Suscribirse")).await;
        assert_eq!(rig.desktop.calls(), vec![Call::ClickUi("Suscribirse".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn a_label_nothing_answers_to_is_reported_not_silently_ignored() {
        let mut rig = Rig::builder().desktop(eva_mcp::desktop::mock::MockDesktop::failing()).build();
        let events = rig.run(typed("Adán, haz clic en Algo que no existe")).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })), "{events:?}");
    }

    #[tokio::test]
    async fn an_app_that_is_not_installed_says_so_and_offers_the_app_store_only_on_a_yes() {
        let mut rig = Rig::new();
        let declined = rig.run_answering(typed("Adán, abre Photoshop"), false).await;

        assert!(declined.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("Photoshop") && title.contains("no está instalada"))), "{declined:?}");
        assert!(declined
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("no está instalada"))));
        assert!(rig.desktop.calls().is_empty(), "nothing opens unless they say yes");

        let accepted = rig.run_answering(typed("Adán, abre Photoshop"), true).await;
        assert!(accepted
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        assert_eq!(
            rig.desktop.calls(),
            vec![Call::OpenUrl(
                "macappstore://search.itunes.apple.com/WebObjects/MZSearch.woa/wa/search?q=Photoshop&media=software"
                    .to_string()
            )]
        );
    }

    #[tokio::test]
    async fn brave_said_the_ways_the_log_shows_opens_it_or_offers_it_and_remembers_the_offer() {
        let mut rig = Rig::new();
        // Heard by the speech model as "Breve", "Brive", with a comma after the verb…
        for said in ["Adán, abre, breve", "Adán, abre, Brive", "Adán Abre Breve"] {
            rig.run(typed(said)).await;
        }
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string()); 3]);

        // …and "braille" is one consonant off: close enough to open and remember.
        let opened = rig.run(typed("Adán, abre braille")).await;
        assert!(!opened.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })), "{opened:?}");
        assert_eq!(rig.desktop.calls().len(), 4);
        assert_eq!(
            rig.ctx.store.list_app_aliases().unwrap(),
            vec![("braille".to_string(), "Brave Browser".to_string())]
        );
    }

    /// A fake local model that always answers `content`.
    fn fake_model(content: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let _ = stream.read(&mut [0u8; 8192]);
                let payload = serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    )
                    .as_bytes(),
                );
            }
        });
        url
    }

    #[tokio::test]
    async fn the_local_model_resolves_what_nothing_else_could_and_the_pick_is_remembered() {
        let url = fake_model("Brave Browser");
        let mut rig = Rig::builder()
            .configure(|c| {
                c.resolver.enabled = true;
                c.resolver.base_url = url;
            })
            .build();

        // "brayle" looks like Brave: opened at once, and learned.
        let events = rig.run(typed("Adán, abre brayle")).await;
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })), "{events:?}");
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
        assert_eq!(
            rig.ctx.store.list_app_aliases().unwrap(),
            vec![("brayle".to_string(), "Brave Browser".to_string())]
        );

        // A pick that looks nothing like what was said is asked about first, and a no teaches nothing.
        let asked = rig.run_answering(typed("Adán, abre egipto"), false).await;
        assert!(
            asked.iter().any(
                |e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("Brave Browser"))
            ),
            "{asked:?}"
        );
        assert_eq!(rig.desktop.calls().len(), 1);
        assert_eq!(rig.ctx.store.list_app_aliases().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_model_that_knows_no_such_app_leaves_the_app_store_offer() {
        let url = fake_model("NINGUNA");
        let mut rig = Rig::builder()
            .configure(|c| {
                c.resolver.enabled = true;
                c.resolver.base_url = url;
            })
            .build();
        let declined = rig.run_answering(typed("Adán, abre Photoshop"), false).await;
        assert!(declined.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("no está instalada"))), "{declined:?}");
    }

    #[tokio::test]
    async fn an_app_installed_after_startup_is_found_without_restarting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let installed = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&installed);
        let catalog = crate::apps::AppCatalog::new(move || {
            let mut apps = vec![eva_intent::AppEntry::new("Brave Browser")];
            if flag.load(Ordering::SeqCst) {
                apps.push(eva_intent::AppEntry::new("Spotify"));
            }
            eva_intent::AppIndex::new(apps)
        });
        let mut rig = Rig::builder().apps(catalog).build();

        installed.store(true, Ordering::SeqCst); // they installed it from the App Store
        rig.ctx.app_index.allow_rescan_now();
        let events = rig.run(typed("Adán, abre Spotify")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Spotify".to_string())]);
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
    }

    #[tokio::test]
    async fn closing_an_app_that_is_not_there_just_says_so() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, cierra Photoshop")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("Photoshop"))));
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
    }

    #[tokio::test]
    async fn open_app_command_calls_the_desktop_and_logs_audit() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, abre brave")).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
        let audit = rig.ctx.store.recent_audit(10).expect("must succeed");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, Decision::AutoApproved);
        assert_eq!(audit[0].intent_json["intent"]["kind"], "open_app", "the voice intent is kept in the audit trail");
    }

    #[tokio::test]
    async fn destructive_command_is_blocked_and_never_reaches_the_desktop() {
        let mut rig = Rig::new();
        let events = rig.run(typed("Adán, borra el proyecto")).await;

        assert!(rig.desktop.calls().is_empty(), "a blocked command must never call the desktop");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        let audit = rig.ctx.store.recent_audit(10).expect("must succeed");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, Decision::Blocked);
    }

    #[tokio::test]
    async fn web_search_opens_a_google_url_with_the_query_encoded() {
        let mut rig = Rig::new();
        rig.run(typed("Adán, busca clima hoy")).await;

        let calls = rig.desktop.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], Call::OpenUrl(url) if url.contains("google.com/search") && url.contains("clima")));
    }

    #[tokio::test]
    async fn spoken_addresses_open_exactly_the_url_the_gateway_ruled_on() {
        let mut rig = Rig::new();
        rig.run(typed("Adán, abre localhost tres mil.")).await;
        rig.run(typed("Adán, abre google punto com punto mx")).await;
        rig.run(typed("Adán, abre github.com.")).await;

        assert_eq!(
            rig.desktop.calls(),
            vec![
                Call::OpenUrl("http://localhost:3000".to_string()),
                Call::OpenUrl("https://google.com.mx".to_string()),
                Call::OpenUrl("https://github.com".to_string()),
            ]
        );
        let audited: Vec<String> = rig
            .ctx
            .store
            .recent_audit(10)
            .unwrap()
            .into_iter()
            .map(|a| a.intent_json["subject"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(audited.contains(&"http://localhost:3000".to_string()), "the audit names what opened: {audited:?}");
    }

    #[tokio::test]
    async fn a_desktop_failure_ends_the_request_as_failed_with_the_reason() {
        let mut rig = Rig::builder().desktop(MockDesktop::failing()).build();
        let events = rig.run(typed("Adán, abre brave")).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    #[tokio::test]
    async fn a_link_that_is_not_web_asks_first_even_by_voice_and_runs_only_on_yes() {
        let mut rig = Rig::new();
        let events = rig.run_answering(typed("Adán, abre file:///etc/hosts"), true).await;

        assert!(events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { title, .. } if title.contains("file:///etc/hosts"))), "{events:?}");
        assert_eq!(rig.desktop.calls(), vec![Call::OpenUrl("file:///etc/hosts".to_string())]);
    }

    #[tokio::test]
    async fn declining_that_confirmation_runs_nothing_and_says_why() {
        let mut rig = Rig::new();
        let events = rig.run_answering(typed("Adán, abre file:///etc/hosts"), false).await;

        assert!(rig.desktop.calls().is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("no confirmaste"))));
        assert_eq!(rig.ctx.store.recent_audit(1).expect("audit")[0].decision, Decision::UserRejected);
    }

    #[tokio::test]
    async fn a_link_that_runs_code_is_refused_without_asking() {
        let mut rig = Rig::new();
        let events = rig.run_answering(typed("Adán, abre javascript://x%0Aalert(1)"), true).await;
        assert!(rig.desktop.calls().is_empty());
        assert!(!events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
    }

    #[tokio::test]
    async fn the_configured_policy_can_make_a_voice_action_ask_first() {
        let mut rig = Rig::builder()
            .configure(|c| {
                c.gateway.voice.insert(eva_config::ActionKind::OpenApp, eva_config::Policy::Confirm);
            })
            .build();
        let events = rig.run_answering(typed("Adán, abre brave"), true).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::ConfirmationRequested { .. })));
        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
    }
}
