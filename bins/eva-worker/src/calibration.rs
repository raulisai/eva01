//! Guided calibration: the worker asks for a handful of commands, listens to
//! each with the very microphone and speech models real use goes through, and
//! learns from what came out — how this person's wake word and app names are
//! written by the recognizer (`eva_intent::calibration` works that out).
//! Nothing is pasted or executed: what is said here is only listened to.

use crate::context::WorkerContext;
use eva_intent::calibration::{lessons, Sample, CERTAIN_AFTER};
use eva_intent::AppIndex;
use eva_ipc::WorkerState;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// Apps to ask for, in this order, the first few that are installed: the ones
/// people actually open by voice.
const PREFERRED_APPS: &[&str] = &[
    "Spotify",
    "Brave Browser",
    "Google Chrome",
    "Safari",
    "Notes",
    "Terminal",
    "Visual Studio Code",
    "Slack",
    "WhatsApp",
    "Mail",
    "Calendar",
    "Messages",
    "Finder",
];
/// How many "abre <app>" commands are asked for.
const APPS_ASKED: usize = 5;
/// Pause after showing what to say, so it is read before the microphone opens.
const LEAD_IN: Duration = if cfg!(test) { Duration::ZERO } else { Duration::from_millis(1_400) };
/// How long each command is listened to.
const WINDOW: Duration = if cfg!(test) { Duration::from_millis(5) } else { Duration::from_millis(4_000) };

/// One thing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Prompt {
    /// The words, wake word included.
    said: String,
    /// The app it is about.
    expected_app: Option<String>,
}

/// What to ask for: opening a few of the apps this Mac has, closing one, a
/// search — the shapes of command that matter — each with the wake word.
fn prompts(index: &AppIndex, wake_word: &str) -> Vec<Prompt> {
    let installed = index.names();
    let apps: Vec<&str> =
        PREFERRED_APPS.iter().copied().filter(|app| installed.contains(app)).take(APPS_ASKED).collect();
    let spoken = |app: &str| {
        index.spoken_names(app).into_iter().min_by_key(|name| name.chars().count()).unwrap_or(app).to_string()
    };
    let mut prompts: Vec<Prompt> = apps
        .iter()
        .map(|app| Prompt {
            said: format!("{wake_word}, abre {}", spoken(app)),
            expected_app: Some((*app).to_string()),
        })
        .collect();
    if let Some(app) = apps.first() {
        prompts.push(Prompt {
            said: format!("{wake_word}, cierra {}", spoken(app)),
            expected_app: Some((*app).to_string()),
        });
    }
    prompts.push(Prompt { said: format!("{wake_word}, busca el clima de mañana"), expected_app: None });
    // Two more, so the wake word alone is heard enough times to see a pattern.
    prompts.push(Prompt { said: format!("{wake_word}, abre el navegador"), expected_app: None });
    prompts.push(Prompt { said: format!("{wake_word}, abre la calculadora"), expected_app: None });
    prompts
}

/// Starts the calibration in the background.
pub fn start(ctx: &Arc<WorkerContext>, request_id: Uuid) {
    if ctx.audio.is_none() {
        ctx.events.fail(
            request_id,
            "el modelo de reconocimiento de voz no está configurado (ver `eva doctor` y stt en config.toml)",
        );
        return;
    }
    if ctx.recording().is_some() {
        ctx.events.fail(request_id, "hay una grabación en curso; termínala y vuelve a calibrar");
        return;
    }
    let job_ctx = Arc::clone(ctx);
    ctx.spawn_job(async move { run(&job_ctx, request_id).await });
}

async fn run(ctx: &Arc<WorkerContext>, request_id: Uuid) {
    ctx.events.state(request_id, WorkerState::Listening);
    let index = ctx.app_index.current();
    let prompts = prompts(&index, &ctx.wake_word);
    ctx.events.notice(
        request_id,
        format!("Calibración: di cada frase con naturalidad, como cuando das una orden ({} en total).", prompts.len()),
    );

    let mut samples = Vec::new();
    for (number, prompt) in prompts.iter().enumerate() {
        ctx.events.notice(request_id, format!("{}/{}  Di: «{}»", number + 1, prompts.len(), prompt.said));
        tokio::time::sleep(LEAD_IN).await;
        let heard = match listen(ctx).await {
            Ok(text) => text,
            Err(message) => {
                ctx.events.fail(request_id, message);
                return;
            }
        };
        let shown = if heard.is_empty() { "(no oí nada)" } else { heard.as_str() };
        ctx.events.notice(request_id, format!("     oí: {shown}"));
        samples.push(Sample { expected_app: prompt.expected_app.clone(), heard });
    }

    ctx.events.state(request_id, WorkerState::Thinking);
    learn(ctx, request_id, &index, &samples);
    ctx.events.state(request_id, WorkerState::Done(true));
}

/// Listens for [`WINDOW`] and returns what the speech models made of it
/// (empty for silence).
async fn listen(ctx: &Arc<WorkerContext>) -> Result<String, String> {
    let Some(audio) = &ctx.audio else { return Err("el modelo de voz ya no está disponible".to_string()) };
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buffer);
    let source = Arc::clone(&audio.source);
    let handle = tokio::task::spawn_blocking(move || {
        source.start(
            crate::recording::CAPTURE_CHUNK_SIZE,
            Box::new(move |chunk| {
                #[allow(clippy::unwrap_used)] // only poisoned if this same callback panicked, forbidden by policy
                sink.lock().unwrap().extend(chunk);
            }),
        )
    })
    .await
    .map_err(|e| format!("no se pudo abrir el micrófono: {e}"))?
    .map_err(|e| e.to_string())?;
    tokio::time::sleep(WINDOW).await;
    let _ = tokio::task::spawn_blocking(move || handle.stop()).await;

    #[allow(clippy::unwrap_used)] // as above
    let samples = std::mem::take(&mut *buffer.lock().unwrap());
    if crate::recording::is_silence(&samples) {
        return Ok(String::new());
    }
    let stt = Arc::clone(&audio.stt);
    tokio::task::spawn_blocking(move || stt.transcribe(&samples))
        .await
        .map_err(|e| format!("la transcripción falló: {e}"))?
        .map(|transcript| transcript.text)
        .map_err(|e| e.to_string())
}

/// Remembers what `samples` taught and says so.
fn learn(ctx: &WorkerContext, request_id: Uuid, index: &AppIndex, samples: &[Sample]) {
    let lessons = lessons(&ctx.wake_word, index, samples);
    for (heard, times) in &lessons.wake {
        let stored = if *times >= CERTAIN_AFTER {
            ctx.store.trust_wake_variant(heard, crate::dictation::TRUSTED_AFTER_HITS)
        } else {
            ctx.store.count_wake_variant(heard).map(|_| ())
        };
        match stored {
            Ok(()) if *times >= CERTAIN_AFTER => ctx.events.notice(
                request_id,
                format!(
                    "Aprendido: tu «{}» se oye «{heard}» ({times} veces); ya cuenta como palabra de activación.",
                    ctx.wake_word
                ),
            ),
            Ok(()) => ctx.events.notice(request_id, format!("Anotado: «{heard}» (una vez; con más usos se confirma).")),
            Err(e) => tracing::warn!("no se pudo guardar la variante «{heard}»: {e}"),
        }
    }
    for (heard, app) in &lessons.apps {
        match ctx.store.learn_app_alias(heard, app) {
            Ok(()) => {
                ctx.app_index.learn(heard, app);
                ctx.events.notice(request_id, format!("Aprendido: «{heard}» es {app}."));
            }
            Err(e) => tracing::warn!("no se pudo guardar que «{heard}» es «{app}»: {e}"),
        }
    }
    let learned = lessons.wake.len() + lessons.apps.len();
    let summary = match (learned, lessons.unheard) {
        (0, 0) => "Todo se oyó como debía: no hay nada que corregir.".to_string(),
        (0, unheard) => {
            format!("No aprendí nada: {unheard} frases no se entendieron. Prueba en un lugar más silencioso.")
        }
        (_, 0) => "Calibración lista.".to_string(),
        (_, unheard) => format!("Calibración lista ({unheard} frases no se entendieron y no cuentan)."),
    };
    ctx.events.notice(request_id, summary);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::context::AudioContext;
    use crate::testkit::Rig;
    use eva_intent::AppEntry;
    use eva_ipc::{ShellToWorker, WorkerToShell};

    #[test]
    fn it_asks_for_the_apps_this_mac_has_by_the_shortest_name_people_use() {
        let index =
            AppIndex::new(vec![AppEntry::new("Brave Browser").with_aliases(["brave"]), AppEntry::new("Spotify")]);
        let said: Vec<String> = prompts(&index, "Eva").into_iter().map(|p| p.said).collect();
        assert_eq!(said[0], "Eva, abre Spotify");
        assert_eq!(said[1], "Eva, abre brave");
        assert!(said.contains(&"Eva, cierra Spotify".to_string()));
        assert!(said.iter().all(|s| s.starts_with("Eva, ")));
        assert!(!said.iter().any(|s| s.contains("Photoshop")), "only what is installed");
    }

    fn heard(texts: &[&str]) -> AudioContext {
        AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(vec![0.2; 16_000])),
            stt: Arc::new(eva_audio::transcribe::mock::SequenceTranscript::new(texts)),
            model_id: "mock".to_string(),
        }
    }

    #[tokio::test]
    async fn a_calibration_learns_how_the_wake_word_and_an_app_come_out_and_uses_it_at_once() {
        let apps = crate::apps::AppCatalog::fixed(AppIndex::new(vec![AppEntry::new("Spotify")]));
        let texts = [
            "Adam, abre Spotifi.",
            "Adam, cierra Spotifi.",
            "Adam busca el clima de mañana.",
            "Adán, abre el navegador.",
            "Adán, abre la calculadora.",
        ];
        let mut rig = Rig::builder().apps(apps).audio(heard(&texts)).build();

        let events = rig.run(ShellToWorker::Calibrate { request_id: Uuid::new_v4() }).await;

        let notices: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                WorkerToShell::Notice { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert!(notices.iter().any(|m| m.contains("Di: «Adán, abre Spotify»")), "{notices:?}");
        assert!(notices.iter().any(|m| m.contains("Aprendido") && m.contains("«adam»")), "{notices:?}");
        assert!(notices.iter().any(|m| m.contains("«spotifi» es Spotify")), "{notices:?}");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));

        // Remembered for good, and in force in the running worker.
        assert_eq!(rig.ctx.store.trusted_wake_variants(3).unwrap(), vec!["adam".to_string()]);
        assert_eq!(rig.ctx.store.list_app_aliases().unwrap(), vec![("spotifi".to_string(), "Spotify".to_string())]);
        let taken = crate::dictation::interpret_text_for_test(&rig.ctx, "Adam agrega tests al login");
        assert_eq!(taken["kind"], "agent_task", "a trusted spelling is the wake word for anything");
        let opened = crate::dictation::interpret_text_for_test(&rig.ctx, "Adán, abre Spotifi");
        assert_eq!(opened["kind"], "open_app", "and Spotifi no longer needs a question");
    }

    #[tokio::test]
    async fn a_calibration_that_hears_nothing_says_so_and_learns_nothing() {
        let mut rig = Rig::builder().audio(heard(&[""])).build();
        let events = rig.run(ShellToWorker::Calibrate { request_id: Uuid::new_v4() }).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WorkerToShell::Notice { message, .. } if message.contains("No aprendí nada"))),
            "{events:?}"
        );
        assert!(rig.ctx.store.trusted_wake_variants(1).unwrap().is_empty());
    }

    #[tokio::test]
    async fn without_a_speech_model_it_says_what_is_missing() {
        let mut rig = Rig::new();
        let events = rig.run(ShellToWorker::Calibrate { request_id: Uuid::new_v4() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("modelo"))));
    }
}
