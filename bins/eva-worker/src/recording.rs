//! Push-to-talk recording: the hotkey's press starts capturing, its release
//! stops, transcribes on a blocking thread, and hands the text to the same
//! pipeline typed text goes through.

use crate::context::{Capture, RecordingSession, WorkerContext};
use eva_audio::CaptureHandle;
use eva_ipc::WorkerState;
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
    ctx.events.state(request_id, WorkerState::Listening);
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
    if recording.as_ref().is_some_and(|s| s.request_id == request_id) {
        recording.take()
    } else {
        None
    }
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
