//! Teaching mode: the panel's "cuéntame cómo lo dirías". The worker listens
//! round after round, with the very microphone and speech models real use goes
//! through, and reports each round as a notice the panel reads: `escuchando`
//! when the window opens, then `oí: …` with what came out (or `silencio`).
//! Nothing is pasted, opened or run — what is said here is only listened to —
//! and it goes on until the process is ended (the panel's "Listo", or closing).

use crate::context::WorkerContext;
use eva_ipc::WorkerState;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// How long each round listens: long enough for a sentence, short enough that
/// a wrong one is quickly over.
const WINDOW: Duration = if cfg!(test) { Duration::from_millis(5) } else { Duration::from_millis(4_500) };
/// The breath between rounds, so the panel can show what was heard.
const PAUSE: Duration = if cfg!(test) { Duration::ZERO } else { Duration::from_millis(600) };
/// Rounds in a row that can hear nothing before teaching gives up: an
/// unplugged microphone must not spin forever.
const MAX_SILENT_ROUNDS: usize = 12;

/// Starts teaching mode in the background.
pub fn start(ctx: &Arc<WorkerContext>, request_id: Uuid) {
    if ctx.audio.is_none() {
        ctx.events.fail(
            request_id,
            "el modelo de reconocimiento de voz no está configurado (ver `eva doctor` y stt en config.toml)",
        );
        return;
    }
    if ctx.recording().is_some() {
        ctx.events.fail(request_id, "hay una grabación en curso; termínala y vuelve a intentarlo");
        return;
    }
    let job_ctx = Arc::clone(ctx);
    ctx.spawn_job(async move { run(&job_ctx, request_id, usize::MAX).await });
}

/// Listens for up to `rounds` rounds (all of them, in production).
async fn run(ctx: &Arc<WorkerContext>, request_id: Uuid, rounds: usize) {
    ctx.events.state(request_id, WorkerState::Listening);
    let mut silent_in_a_row = 0;
    for _ in 0..rounds {
        ctx.events.notice(request_id, "escuchando");
        match crate::calibration::listen_for(ctx, WINDOW).await {
            Ok(text) if text.trim().is_empty() => {
                silent_in_a_row += 1;
                ctx.events.notice(request_id, "silencio");
                if silent_in_a_row >= MAX_SILENT_ROUNDS {
                    ctx.events.fail(request_id, "no oigo nada: revisa el micrófono en «Doctor»");
                    return;
                }
            }
            Ok(text) => {
                silent_in_a_row = 0;
                ctx.events.notice(request_id, format!("oí: {}", text.trim()));
            }
            Err(message) => {
                ctx.events.fail(request_id, message);
                return;
            }
        }
        tokio::time::sleep(PAUSE).await;
    }
    ctx.events.state(request_id, WorkerState::Done(true));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::context::AudioContext;
    use crate::testkit::Rig;
    use eva_ipc::WorkerToShell;

    fn heard(texts: &[&str]) -> AudioContext {
        AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(vec![0.2; 16_000])),
            stt: Arc::new(eva_audio::transcribe::mock::SequenceTranscript::new(texts)),
            model_id: "mock".to_string(),
        }
    }

    fn notices(events: &[WorkerToShell]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                WorkerToShell::Notice { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn every_round_says_when_it_listens_and_what_it_heard_and_does_nothing_else() {
        let mut rig = Rig::builder().audio(heard(&["Ponme mi canal favorito.", ""])).build();
        run(&rig.ctx, Uuid::new_v4(), 2).await;
        let events = rig.drain();

        assert_eq!(notices(&events), ["escuchando", "oí: Ponme mi canal favorito.", "escuchando", "silencio"]);
        assert!(rig.desktop.calls().is_empty(), "what is said while teaching is never executed");
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(true), .. })));
    }

    #[tokio::test]
    async fn without_a_speech_model_it_says_so_instead_of_pretending_to_listen() {
        let mut rig = Rig::new();
        let events = rig.run(eva_ipc::ShellToWorker::Teach { request_id: Uuid::new_v4() }).await;
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("modelo"))),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_microphone_that_hears_nothing_for_long_gives_up() {
        let mut rig = Rig::builder().audio(heard(&[""])).build();
        run(&rig.ctx, Uuid::new_v4(), 100).await;
        let events = rig.drain();
        assert_eq!(notices(&events).iter().filter(|m| **m == "silencio").count(), MAX_SILENT_ROUNDS);
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("micrófono"))));
    }
}
