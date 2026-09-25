//! Push-to-talk recording: the hotkey's press starts capturing, its release
//! stops, transcribes on a blocking thread, and hands the text to the same
//! pipeline typed text goes through.

use crate::context::{Capture, RecordingSession, WorkerContext};
use eva_audio::CaptureHandle;
use eva_ipc::{WorkerState, WorkerToShell};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Chunk size for the capture callback. Push-to-talk needs no VAD
/// segmentation — the hotkey itself marks the utterance's boundaries — but
/// `AudioSource::start` needs a non-zero size for its rechunking; 1600
/// samples is 100ms at 16 kHz, a balance between callback frequency and
/// overhead.
pub(crate) const CAPTURE_CHUNK_SIZE: usize = 1_600;

/// The whole recording must reach at least this amplitude (of 1.0) somewhere
/// to count as speech. A quiet room's noise floor sits around 0.001–0.005;
/// speech peaks at 0.05 and up. Below this, transcribing invents text —
/// Whisper-family models famously "hear" a phrase like "Gracias por ver el
/// video" in silence — so a release with nothing said is dropped instead.
const SILENCE_PEAK: f32 = 0.01;

/// Shorter than this (a quarter second at 16 kHz) is a tap, not speech.
const MIN_SAMPLES: usize = 4_000;

/// The wake word is listened for while the recording is still going. What
/// the real model taught (measured, on clips of a spoken command): cut at
/// arbitrary points it is unreliable — a lead-in of quiet before the words
/// makes short clips come back empty. So the clip starts where the *speech*
/// starts (a little before), and is tried once it holds 1.0 s of it, then after
/// every further 0.4 s, up to 2.6 s of speech. Past that it is dictation, not a
/// command, and nothing more is spent on it.
const PEEK_FIRST: usize = 16_000;
const PEEK_EVERY: usize = 6_400;
const PEEK_MAX: usize = 41_600;
/// How far into the recording the start of speech is looked for (6 s).
const PEEK_SEARCH: usize = 96_000;
/// A sample this loud is speech starting (a quiet room sits around 0.005).
const ONSET_LEVEL: f32 = 0.03;
/// The clip starts this much before the first loud sample (0.15 s), so the
/// first consonant is not cut.
const ONSET_LEAD: usize = 2_400;

/// A spelling of the wake word taken for it this many times is trusted for
/// anything (the same threshold the final decision uses).
const TRUSTED_AFTER_HITS: u32 = 3;

/// Starts capturing, if a model is configured. The microphone is opened on a
/// blocking thread, not here: CoreAudio can take its time (a Bluetooth headset
/// switching profiles, the first-run permission prompt), and this runs on the
/// command loop, which must keep answering — the shell's heartbeat restarts
/// a worker whose loop stops answering.
pub fn start(ctx: &Arc<WorkerContext>, request_id: Uuid) {
    let Some(audio) = &ctx.audio else {
        // The real, documented "model not loaded" degradation from
        // `docs/PLAN.md` §3.3 point 5 — never a silent no-op.
        ctx.events.error(
            request_id,
            "el modelo de reconocimiento de voz no está configurado (ver `eva doctor` y stt en config.toml)",
        );
        return;
    };

    let mut recording = ctx.recording();
    if recording.is_some() {
        ctx.events.error(request_id, "ya hay una grabación en curso");
        return;
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::new(Mutex::new(Capture::Opening));
    let streaming = Arc::new(Mutex::new(None));
    *recording = Some(RecordingSession {
        request_id,
        capture: Arc::clone(&capture),
        buffer: Arc::clone(&buffer),
        streaming: Arc::clone(&streaming),
    });
    drop(recording);
    quiet_the_mac(ctx, true);
    ctx.events.state(request_id, WorkerState::Listening);
    spawn_peek(ctx, request_id, Arc::clone(&buffer));
    #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
    {
        *streaming.lock().unwrap() = crate::streaming::start(ctx, request_id, Arc::clone(&buffer));
    }

    let source = Arc::clone(&audio.source);
    let job_ctx = Arc::clone(ctx);
    ctx.spawn_job(async move {
        let opened = tokio::task::spawn_blocking(move || {
            source.start(
                CAPTURE_CHUNK_SIZE,
                Box::new(move |chunk| {
                    #[allow(clippy::unwrap_used)]
                    // only poisoned if this same callback panicked, forbidden by workspace policy
                    buffer.lock().unwrap().extend(chunk);
                }),
            )
        })
        .await;

        match opened {
            Ok(Ok(handle)) => {
                // Kept for the recording, unless it already ended meanwhile.
                let too_late = {
                    let mut state = lock(&capture);
                    if matches!(*state, Capture::Abandoned) {
                        Some(handle)
                    } else {
                        *state = Capture::Open(handle);
                        None
                    }
                };
                if let Some(handle) = too_late {
                    let _ = tokio::task::spawn_blocking(move || handle.stop()).await;
                }
            }
            Ok(Err(e)) => fail_to_open(&job_ctx, request_id, e.to_string()),
            Err(join_error) => {
                fail_to_open(&job_ctx, request_id, format!("no se pudo abrir el micrófono: {join_error}"))
            }
        }
    });
}

/// Listens to the first seconds of a recording *while it is still going*, and
/// says [`WorkerToShell::WakeWordHeard`] if they start with the wake word.
/// Purely a hint for the island: what the recording turns out to be is still
/// decided from the whole transcript, once the key is released.
fn spawn_peek(ctx: &Arc<WorkerContext>, request_id: Uuid, buffer: Arc<Mutex<Vec<f32>>>) {
    let Some(stt) = ctx.audio.as_ref().map(|a| Arc::clone(&a.stt)) else { return };
    let job_ctx = Arc::clone(ctx);
    // Not a tracked job: it lives exactly as long as the recording, which is
    // still going when everything else has settled.
    tokio::spawn(async move {
        let mut needed = PEEK_FIRST;
        loop {
            if !still_recording(&job_ctx, request_id) {
                return;
            }
            // The clip: from just before the speech started, as much as is there.
            #[allow(clippy::unwrap_used)]
            // only poisoned if the capture callback panicked, forbidden by workspace policy
            let (clip, searched) = {
                let guard = buffer.lock().unwrap();
                let head = &guard[..guard.len().min(PEEK_SEARCH)];
                let clip = head.iter().position(|s| s.abs() >= ONSET_LEVEL).and_then(|onset| {
                    let start = onset.saturating_sub(ONSET_LEAD);
                    let available = guard.len() - start;
                    (available >= needed).then(|| guard[start..start + available.min(PEEK_MAX)].to_vec())
                });
                (clip, guard.len() >= PEEK_SEARCH)
            };
            let Some(samples) = clip else {
                if searched {
                    return; // no speech in the first seconds: nothing to listen for
                }
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                continue;
            };
            let done = samples.len() >= PEEK_MAX;
            needed = samples.len() + PEEK_EVERY;

            let stt = Arc::clone(&stt);
            let seconds = samples.len() as f32 / 16_000.0;
            let transcript = match tokio::task::spawn_blocking(move || stt.transcribe(&samples)).await {
                Ok(Ok(transcript)) => transcript,
                // One failed listen is not the end of them: try again a little later.
                Ok(Err(e)) => {
                    tracing::warn!(%request_id, seconds, "no se pudo escuchar la palabra de activación: {e}");
                    continue;
                }
                Err(_) => return,
            };
            // The key may have come up while that was transcribing.
            if !still_recording(&job_ctx, request_id) {
                tracing::info!(%request_id, seconds, heard = %transcript.text, "la escucha de la palabra de activación llegó tarde (ya se soltó la tecla)");
                return;
            }
            tracing::info!(%request_id, seconds, heard = %transcript.text, "escucha de la palabra de activación");
            let heard = eva_text::filler::remove_universal_fillers(&transcript.text);
            let learned = job_ctx.store.trusted_wake_variants(TRUSTED_AFTER_HITS).unwrap_or_default();
            if eva_intent::wake::find_wake_word(&heard, &job_ctx.wake_word, &learned).is_some() {
                tracing::info!(%request_id, "palabra de activación oída mientras graba");
                job_ctx.events.emit(WorkerToShell::WakeWordHeard { request_id });
                return;
            }
            if done {
                return;
            }
        }
    });
}

/// Whether `request_id` is still the recording in progress.
pub(crate) fn still_recording(ctx: &WorkerContext, request_id: Uuid) -> bool {
    ctx.recording().as_ref().is_some_and(|s| s.request_id == request_id)
}

/// The microphone would not open: the recording is over before it began.
fn fail_to_open(ctx: &WorkerContext, request_id: Uuid, message: String) {
    if take_session(ctx, request_id).is_some() {
        ctx.events.fail(request_id, message);
    }
}

fn lock(capture: &Mutex<Capture>) -> std::sync::MutexGuard<'_, Capture> {
    #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
    capture.lock().unwrap()
}

/// Closes the recording's stream — or, if it is still being opened, leaves
/// word to close it the moment it is. Returns the stream to stop, if open.
fn close(session: &RecordingSession) -> Option<Box<dyn CaptureHandle>> {
    match std::mem::replace(&mut *lock(&session.capture), Capture::Abandoned) {
        Capture::Open(handle) => Some(handle),
        Capture::Opening | Capture::Abandoned => None,
    }
}

/// Takes the current session if `request_id` matches it — used by both
/// `StopRecording` and `Cancel` so a stray, mismatched id (a duplicate or
/// delayed message) can never stop someone else's recording.
fn take_session(ctx: &WorkerContext, request_id: Uuid) -> Option<RecordingSession> {
    let mut recording = ctx.recording();
    let session = if recording.as_ref().is_some_and(|s| s.request_id == request_id) { recording.take() } else { None };
    drop(recording);
    if session.is_some() {
        // The key is up (or the recording failed): the sound comes back now,
        // not after the transcription.
        quiet_the_mac(ctx, false);
    }
    session
}

/// While the dictation key is down the Mac's music is paused and its sound
/// silenced, so the microphone hears only the user; released, it all comes
/// back. The wish is recorded here, in order; the slow part (AppleScript)
/// runs on a blocking thread and only reconciles with the last wish, so it
/// does not matter which of the two jobs of a quick tap runs first.
fn quiet_the_mac(ctx: &WorkerContext, quiet: bool) {
    if !ctx.config.dictation.pause_media {
        return;
    }
    let desktop = Arc::clone(&ctx.desktop);
    desktop.want_media_quiet(quiet);
    ctx.spawn_job(async move {
        let _ = tokio::task::spawn_blocking(move || desktop.settle_media_quiet()).await;
    });
}

/// Stops and discards a recording. `false` if there was none with that id.
pub fn cancel(ctx: &Arc<WorkerContext>, request_id: Uuid) -> bool {
    match take_session(ctx, request_id) {
        Some(session) => {
            if let Some(handle) = close(&session) {
                ctx.spawn_job(async move {
                    let _ = tokio::task::spawn_blocking(move || handle.stop()).await;
                });
            }
            true
        }
        None => false,
    }
}

/// Stops the recording, then transcribes and processes it in the
/// background — the command loop is free again the moment this returns.
pub fn stop(ctx: &Arc<WorkerContext>, request_id: Uuid) {
    let Some(session) = take_session(ctx, request_id) else {
        ctx.events.error(request_id, "no había una grabación en curso con ese id");
        return;
    };
    let handle = close(&session);
    #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
    let streaming = session.streaming.lock().unwrap().take();
    ctx.events.state(request_id, WorkerState::Thinking);

    let job_ctx = Arc::clone(ctx);
    ctx.spawn_job(async move {
        // Closing the stream is a CoreAudio call too, so it goes to a
        // blocking thread; what it recorded is only read once it is closed.
        if let Some(handle) = handle {
            let _ = tokio::task::spawn_blocking(move || handle.stop()).await;
        }
        #[allow(clippy::unwrap_used)] // only poisoned if the capture callback panicked, forbidden by workspace policy
        let samples = std::mem::take(&mut *session.buffer.lock().unwrap());
        // What was transcribed while the key was down is not done again.
        let streamed = match streaming {
            Some(streaming) => streaming.finish().await,
            None => crate::streaming::Streamed::default(),
        };
        transcribe_and_process(&job_ctx, request_id, samples, streamed).await;
    });
}

async fn transcribe_and_process(
    ctx: &Arc<WorkerContext>,
    request_id: Uuid,
    samples: Vec<f32>,
    streamed: crate::streaming::Streamed,
) {
    if is_silence(&samples) {
        tracing::info!(samples = samples.len(), "grabación sin voz; no se transcribe");
        ctx.events.state(request_id, WorkerState::Idle);
        return;
    }

    // Held (in memory only) until the next recording, in case the user flags
    // what comes out as wrong — see `crate::harvest`.
    ctx.harvest.remember_audio(request_id, samples.clone());

    let Some(audio) = &ctx.audio else {
        // Can only happen if the model was unloaded between Start and Stop,
        // which nothing in this binary does today — defensive, not expected.
        ctx.events.fail(request_id, "el modelo de reconocimiento de voz ya no está disponible");
        return;
    };

    // Whisper/Canary inference is CPU-bound and takes real time; running it
    // on this async task's thread would hold up everything else on it (an
    // agent's event streaming, a cancel) for that whole duration.
    //
    // A long dictation already had its finished stretches transcribed while
    // the key was down; only what came after the last of them is left.
    let tail = samples[streamed.upto.min(samples.len())..].to_vec();
    let stt = Arc::clone(&audio.stt);
    let transcript = tokio::task::spawn_blocking(move || {
        if streamed.upto > 0 && is_silence(&tail) {
            return Ok(eva_audio::Transcript { text: String::new() });
        }
        stt.transcribe(&tail)
    })
    .await;
    match transcript {
        Ok(Ok(transcript)) => {
            let mut texts = streamed.texts;
            texts.push(transcript.text);
            let text = texts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
            crate::dictation::process_text(ctx, request_id, &text).await;
        }
        Ok(Err(e)) => ctx.events.fail(request_id, e.to_string()),
        Err(join_error) => ctx.events.fail(request_id, format!("la tarea de transcripción falló: {join_error}")),
    }
}

/// Whether a recording holds no speech: too short to be an utterance, or
/// never rising above the room's noise floor.
pub(crate) fn is_silence(samples: &[f32]) -> bool {
    samples.len() < MIN_SAMPLES || samples.iter().all(|s| s.abs() < SILENCE_PEAK)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_tap_length_recording_is_silence() {
        assert!(is_silence(&[]));
        assert!(is_silence(&vec![0.5; MIN_SAMPLES - 1]), "too short to be speech, however loud");
    }

    #[test]
    fn a_long_recording_that_never_leaves_the_noise_floor_is_silence() {
        assert!(is_silence(&vec![0.003; 16_000]));
        assert!(is_silence(&vec![0.0; 16_000]));
    }

    #[test]
    fn a_recording_with_a_speech_level_peak_is_not_silence() {
        let mut samples = vec![0.002; 16_000];
        samples[8_000] = 0.2;
        assert!(!is_silence(&samples));
    }

    #[test]
    fn negative_peaks_count_too() {
        let mut samples = vec![0.0; 16_000];
        samples[100] = -0.3;
        assert!(!is_silence(&samples));
    }

    use crate::context::AudioContext;
    use crate::testkit::Rig;
    use eva_ipc::{ShellToWorker, WorkerToShell};
    use eva_mcp::desktop::mock::Call;

    /// A microphone that "hears" `script`, and an STT that always returns `text`.
    fn audio(script: Vec<f32>, text: &str) -> AudioContext {
        AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(script)),
            stt: Arc::new(eva_audio::transcribe::mock::FixedTranscript::new(text)),
            model_id: "mock-model".to_string(),
        }
    }

    /// One second of "speech" — loud enough to clear the silence gate.
    fn speech() -> Vec<f32> {
        vec![0.2; 16_000]
    }

    #[tokio::test]
    async fn a_full_recording_is_captured_transcribed_cleaned_and_pasted() {
        let mut rig = Rig::builder().audio(audio(speech(), "hola mundo")).build();
        let request_id = Uuid::new_v4();

        let start = rig.run(ShellToWorker::StartRecording { request_id }).await;
        assert!(start.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Listening, .. })));

        let stop = rig.run(ShellToWorker::StopRecording { request_id }).await;
        assert!(stop.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Thinking, .. })));
        assert!(stop
            .iter()
            .any(|e| matches!(e, WorkerToShell::Transcript { cleaned, .. } if cleaned == "Hola mundo.")));
        assert_eq!(rig.desktop.calls(), vec![Call::InsertText("Hola mundo. ".to_string())]);
    }

    /// Two seconds of "speech": past the first listen for the wake word.
    fn long_speech() -> Vec<f32> {
        vec![0.2; 32_000]
    }

    #[tokio::test]
    async fn the_wake_word_is_noticed_while_the_user_is_still_speaking() {
        let mut rig = Rig::builder().audio(audio(long_speech(), "adán abre brave")).build();
        let request_id = Uuid::new_v4();
        let mut heard = rig.run(ShellToWorker::StartRecording { request_id }).await;
        heard.extend(rig.until(|e| matches!(e, WorkerToShell::WakeWordHeard { .. })).await);
        assert!(
            heard.iter().any(|e| matches!(e, WorkerToShell::WakeWordHeard { request_id: id } if *id == request_id)),
            "the key is still down and the island can already say it is a command: {heard:?}"
        );
    }

    #[tokio::test]
    async fn a_quiet_lead_in_before_the_words_does_not_stop_the_wake_word_being_noticed() {
        // 1.5 s of a quiet room, then speech: the clip must start at the speech.
        let mut script = vec![0.002; 24_000];
        script.extend(vec![0.2; 32_000]);
        let mut rig = Rig::builder().audio(audio(script, "adán abre brave")).build();
        let request_id = Uuid::new_v4();
        let mut heard = rig.run(ShellToWorker::StartRecording { request_id }).await;
        heard.extend(rig.until(|e| matches!(e, WorkerToShell::WakeWordHeard { .. })).await);
        assert!(heard.iter().any(|e| matches!(e, WorkerToShell::WakeWordHeard { .. })), "{heard:?}");
    }

    #[tokio::test]
    async fn plain_dictation_is_never_announced_as_a_command() {
        let mut rig = Rig::builder().audio(audio(long_speech(), "hola mundo esto es un dictado")).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!rig.drain().iter().any(|e| matches!(e, WorkerToShell::WakeWordHeard { .. })));
    }

    #[tokio::test]
    async fn a_recording_that_starts_with_the_wake_word_runs_the_command_not_a_paste() {
        // The mock STT ignores the audio and always returns the same text,
        // which starts with the wake word — so this must resolve to a
        // command, exercising the exact interpret-then-act pipeline a real
        // transcript goes through (with the leading "eh" the gate must see
        // through).
        let mut rig = Rig::builder().audio(audio(speech(), "eh adán abre brave")).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        let stop = rig.run(ShellToWorker::StopRecording { request_id }).await;

        assert_eq!(rig.desktop.calls(), vec![Call::OpenApp("Brave Browser".to_string())]);
        assert!(stop.iter().any(|e| matches!(e, WorkerToShell::IntentRecognized { .. })));
    }

    #[tokio::test]
    async fn a_release_with_nothing_said_is_dropped_instead_of_transcribed() {
        // Whisper-family models invent text from silence; the STT here would
        // happily return a phrase, and the gate must keep it from being asked.
        let mut rig = Rig::builder().audio(audio(vec![0.001; 16_000], "Gracias por ver el video")).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        let stop = rig.run(ShellToWorker::StopRecording { request_id }).await;

        assert!(rig.desktop.calls().is_empty(), "nothing must be pasted for silence");
        assert!(!stop.iter().any(|e| matches!(e, WorkerToShell::Transcript { .. })));
        assert!(stop.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Idle, .. })));
    }

    #[tokio::test]
    async fn stop_recording_with_an_unknown_request_id_is_a_clear_error() {
        let mut rig = Rig::new();
        let events = rig.run(ShellToWorker::StopRecording { request_id: Uuid::new_v4() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn starting_a_second_recording_while_one_is_active_is_rejected() {
        let mut rig = Rig::builder().audio(audio(speech(), "hola")).build();
        let first = rig.run(ShellToWorker::StartRecording { request_id: Uuid::new_v4() }).await;
        assert!(first.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Listening, .. })));

        let second = rig.run(ShellToWorker::StartRecording { request_id: Uuid::new_v4() }).await;
        assert!(second.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
    }

    #[tokio::test]
    async fn the_mac_is_quiet_only_while_the_key_is_down_and_comes_back_on_release_or_cancel() {
        for cancel in [false, true] {
            let mut rig = Rig::builder()
                .audio(audio(speech(), "hola mundo"))
                .configure(|c| c.dictation.pause_media = true)
                .build();
            let request_id = Uuid::new_v4();

            rig.run(ShellToWorker::StartRecording { request_id }).await;
            assert_eq!(rig.desktop.calls().first(), Some(&Call::WantMediaQuiet(true)), "quiet from the press");

            let end =
                if cancel { ShellToWorker::Cancel { request_id } } else { ShellToWorker::StopRecording { request_id } };
            rig.run(end).await;
            let calls = rig.desktop.calls();
            let wishes: Vec<&Call> = calls.iter().filter(|c| matches!(c, Call::WantMediaQuiet(_))).collect();
            assert_eq!(
                wishes,
                vec![&Call::WantMediaQuiet(true), &Call::WantMediaQuiet(false)],
                "cancel={cancel}: {calls:?}"
            );
            assert!(
                calls.iter().filter(|c| **c == Call::SettleMediaQuiet).count() >= 2,
                "each wish is settled: {calls:?}"
            );
        }
    }

    #[tokio::test]
    async fn with_pausing_off_the_music_and_the_volume_are_never_touched() {
        let mut rig =
            Rig::builder().audio(audio(speech(), "hola")).configure(|c| c.dictation.pause_media = false).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        rig.run(ShellToWorker::StopRecording { request_id }).await;
        assert!(!rig.desktop.calls().iter().any(|c| matches!(c, Call::WantMediaQuiet(_) | Call::SettleMediaQuiet)));
    }

    #[tokio::test]
    async fn cancel_stops_an_in_progress_recording_and_discards_it() {
        let mut rig = Rig::builder().audio(audio(speech(), "hola")).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;

        let events = rig.run(ShellToWorker::Cancel { request_id }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Idle, .. })));

        // The session must actually be gone — stopping it again is now an error.
        let stop = rig.run(ShellToWorker::StopRecording { request_id }).await;
        assert!(stop.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert!(rig.desktop.calls().is_empty());
    }

    #[tokio::test]
    async fn a_stale_id_cannot_stop_someone_elses_recording() {
        let mut rig = Rig::builder().audio(audio(speech(), "hola")).build();
        let real = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id: real }).await;

        let events = rig.run(ShellToWorker::StopRecording { request_id: Uuid::new_v4() }).await;
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));

        let stop = rig.run(ShellToWorker::StopRecording { request_id: real }).await;
        assert!(
            stop.iter().any(|e| matches!(e, WorkerToShell::Transcript { .. })),
            "the real recording is still there"
        );
    }

    #[tokio::test]
    async fn start_recording_without_a_configured_model_is_a_clear_recoverable_error() {
        let mut rig = Rig::new();
        let events = rig.run(ShellToWorker::StartRecording { request_id: Uuid::new_v4() }).await;
        assert!(
            matches!(&events[0], WorkerToShell::Error { recoverable: true, message, .. } if message.contains("modelo"))
        );
    }

    #[tokio::test]
    async fn a_failing_transcription_ends_the_request_as_failed() {
        let failing = AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(speech())),
            stt: Arc::new(eva_audio::transcribe::mock::AlwaysFails),
            model_id: "mock-model".to_string(),
        };
        let mut rig = Rig::builder().audio(failing).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        let stop = rig.run(ShellToWorker::StopRecording { request_id }).await;

        assert!(stop.iter().any(|e| matches!(e, WorkerToShell::Error { .. })));
        assert!(stop.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
    }

    /// A microphone that takes a while to open and says when it is closed.
    struct SlowToOpen {
        delay: std::time::Duration,
        closed: Arc<std::sync::atomic::AtomicBool>,
    }

    struct Stream(Arc<std::sync::atomic::AtomicBool>);

    impl eva_audio::CaptureHandle for Stream {
        fn stop(self: Box<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl eva_audio::AudioSource for SlowToOpen {
        fn start(
            &self,
            _chunk_size: usize,
            _on_chunk: Box<dyn FnMut(Vec<f32>) + Send>,
        ) -> Result<Box<dyn eva_audio::CaptureHandle>, eva_audio::AudioError> {
            std::thread::sleep(self.delay);
            Ok(Box::new(Stream(Arc::clone(&self.closed))))
        }
    }

    struct NoMicrophone;

    impl eva_audio::AudioSource for NoMicrophone {
        fn start(
            &self,
            _chunk_size: usize,
            _on_chunk: Box<dyn FnMut(Vec<f32>) + Send>,
        ) -> Result<Box<dyn eva_audio::CaptureHandle>, eva_audio::AudioError> {
            Err(eva_audio::AudioError::NoInputDevice)
        }
    }

    fn with_source(source: impl eva_audio::AudioSource + 'static) -> AudioContext {
        AudioContext {
            source: Arc::new(source),
            stt: Arc::new(eva_audio::transcribe::mock::FixedTranscript::new("hola")),
            model_id: "mock-model".to_string(),
        }
    }

    #[tokio::test]
    async fn a_slow_microphone_never_holds_up_the_command_loop() {
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let source = SlowToOpen { delay: std::time::Duration::from_millis(400), closed: Arc::clone(&closed) };
        let mut rig = Rig::builder().audio(with_source(source)).build();
        let request_id = Uuid::new_v4();

        let started = std::time::Instant::now();
        crate::handler::handle(&rig.ctx, ShellToWorker::StartRecording { request_id }, None);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "the loop was held for {:?}",
            started.elapsed()
        );

        // The key comes up before the microphone has even opened.
        crate::handler::handle(&rig.ctx, ShellToWorker::StopRecording { request_id }, None);
        rig.ctx.wait_idle().await;

        assert!(
            closed.load(std::sync::atomic::Ordering::SeqCst),
            "a stream that opened too late is closed, not left recording"
        );
        let events = rig.drain();
        assert!(events.iter().any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Idle, .. })));
        assert!(rig.desktop.calls().is_empty(), "nothing was heard, nothing is pasted");
    }

    #[tokio::test]
    async fn a_microphone_that_will_not_open_ends_the_recording_and_frees_the_next_one() {
        let mut rig = Rig::builder().audio(with_source(NoMicrophone)).build();
        let first = Uuid::new_v4();
        let events = rig.run(ShellToWorker::StartRecording { request_id: first }).await;

        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("dispositivo"))));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerToShell::StateChanged { state: WorkerState::Done(false), .. })));
        let second = rig.run(ShellToWorker::StartRecording { request_id: Uuid::new_v4() }).await;
        assert!(
            !second.iter().any(|e| matches!(e, WorkerToShell::Error { message, .. } if message.contains("en curso"))),
            "the failed recording does not block the next one"
        );
    }

    proptest::proptest! {
        #[test]
        fn is_silence_never_panics(samples in proptest::collection::vec(-1.0f32..1.0, 0..5_000)) {
            let _ = is_silence(&samples);
        }
    }
}
