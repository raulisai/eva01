//! A long dictation, worked on while it is still being spoken.
//!
//! Everything used to wait for the key to come up: 70 s of speech took ~6 s
//! to transcribe and ~8 s to format, all of it after the last word. Here the
//! audio recorded so far is cut at real pauses, each finished stretch is
//! transcribed in the background as soon as it exists, and the text so far is
//! formatted ahead of time (the formatter remembers its answers, see
//! `eva_text::CachingFormatter`), so when the key comes up what is left is
//! the last few seconds of speech and the last sentence.
//!
//! Short dictations and commands (under 8 s) never reach any of this.

use crate::context::WorkerContext;
use eva_audio::segment::cut_while_recording;
use eva_text::Dictionary;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// How often the recording so far is looked at for a pause.
const POLL: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_millis(250) };

/// What was done while the key was down.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Streamed {
    /// How much of the recording (in samples) is already transcribed: the
    /// key coming up only has what is after it to do.
    pub upto: usize,
    /// The text of each stretch, in order.
    pub texts: Vec<String>,
}

/// A running stream of work on one recording.
pub struct Streaming {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<Streamed>,
}

impl Streaming {
    /// Ends the stream — after the stretch it is transcribing right now, if
    /// any — and says what it got through.
    pub async fn finish(self) -> Streamed {
        self.stop.store(true, Ordering::SeqCst);
        self.task.await.unwrap_or_default()
    }
}

/// Starts working on `buffer`, the recording `request_id` is filling. `None`
/// without a speech model.
pub fn start(ctx: &Arc<WorkerContext>, request_id: Uuid, buffer: Arc<Mutex<Vec<f32>>>) -> Option<Streaming> {
    let stt = Arc::clone(&ctx.audio.as_ref()?.stt);
    let ctx = Arc::clone(ctx);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);

    // Not a tracked job: it lives as long as the recording, which is still
    // going when everything else has settled.
    let task = tokio::spawn(async move {
        let words = ctx.store.list_custom_words().unwrap_or_default();
        let bundle = ctx.active_window().await.and_then(|window| window.bundle_identifier);
        let style = crate::dictation::style_for(&ctx, bundle.as_deref());
        let mut done = Streamed::default();
        loop {
            tokio::time::sleep(POLL).await;
            // Checked after the wait, not before it: a key that came up (or a
            // cancel) while waiting must not start another stretch.
            if flag.load(Ordering::SeqCst) || !crate::recording::still_recording(&ctx, request_id) {
                break;
            }
            let piece = {
                #[allow(clippy::unwrap_used)] // only poisoned if the capture callback panicked, forbidden by policy
                let recorded = buffer.lock().unwrap();
                let waiting = &recorded[done.upto.min(recorded.len())..];
                cut_while_recording(waiting).map(|cut| waiting[..cut].to_vec())
            };
            let Some(piece) = piece else { continue };
            let length = piece.len();
            if !crate::recording::is_silence(&piece) {
                let stt = Arc::clone(&stt);
                match tokio::task::spawn_blocking(move || stt.transcribe(&piece)).await {
                    Ok(Ok(transcript)) if !transcript.text.trim().is_empty() => done.texts.push(transcript.text),
                    // A voice the model answered with nothing is not "done": it is a stretch
                    // that failed. Counting it as done would lose it for good, so the stream
                    // stops here and the key coming up transcribes everything after the last
                    // good stretch (where the second model gets its chance too).
                    Ok(Ok(_)) => {
                        tracing::warn!(
                            seconds = length as f32 / 16_000.0,
                            "un trozo con voz salió vacío; se vuelve a intentar al soltar"
                        );
                        break;
                    }
                    // What is not done here is simply done when the key comes up.
                    Ok(Err(e)) => {
                        tracing::warn!("la transcripción por trozos falló; se hará al soltar: {e}");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("la transcripción por trozos se interrumpió: {e}");
                        break;
                    }
                }
            }
            done.upto += length;
            tracing::info!(request_id = %request_id, seconds = done.upto as f32 / 16_000.0, "trozo transcrito mientras se habla");
            format_ahead(&ctx, &words, style, &done.texts);
        }
        done
    });
    Some(Streaming { stop, task })
}

/// Formats the text so far in the background, so the pieces that are already
/// complete are remembered when the whole text is formatted at the end.
fn format_ahead(ctx: &Arc<WorkerContext>, words: &[String], style: eva_text::Style, texts: &[String]) {
    let (formatter, words, text) = (Arc::clone(&ctx.formatter), words.to_vec(), texts.join(" "));
    tokio::task::spawn_blocking(move || {
        let _ = eva_text::clean_styled(&text, &Dictionary::new(words), formatter.as_ref(), style);
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::context::AudioContext;
    use crate::testkit::Rig;
    use eva_audio::{SpeechToText, TranscribeError, Transcript};
    use eva_ipc::{ShellToWorker, WorkerToShell};

    const RATE: usize = 16_000;

    /// "Speech" with breaths at the given seconds.
    fn speech(seconds: usize, breaths: &[(f32, f32)]) -> Vec<f32> {
        let mut samples = vec![0.2; seconds * RATE];
        for (from, to) in breaths {
            for s in &mut samples[(from * RATE as f32) as usize..(to * RATE as f32) as usize] {
                *s = 0.0005;
            }
        }
        samples
    }

    /// Answers "trozo 1", "trozo 2"… and remembers how long each audio it got was.
    struct Numbered(Arc<Mutex<Vec<usize>>>);
    impl SpeechToText for Numbered {
        fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError> {
            // The quick listen for the wake word (the first 1.3 s and 2.6 s) is not a stretch.
            if samples.len() <= 2 * 20_800 {
                return Ok(Transcript { text: String::new() });
            }
            let mut heard = self.0.lock().unwrap();
            heard.push(samples.len());
            Ok(Transcript { text: format!("trozo {}.", heard.len()) })
        }
    }

    fn rig(samples: Vec<f32>) -> (Rig, Arc<Mutex<Vec<usize>>>) {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let audio = AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(samples)),
            stt: Arc::new(Numbered(Arc::clone(&heard))),
            model_id: "mock".to_string(),
        };
        (Rig::builder().audio(audio).build(), heard)
    }

    #[tokio::test]
    async fn a_long_recording_is_transcribed_in_pieces_while_the_key_is_still_down() {
        // 26 s: breaths at 9 s and 18 s. Everything is "already recorded" at the start.
        let (mut rig, heard) = rig(speech(26, &[(9.0, 9.5), (18.0, 18.5)]));
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;

        // Before the key comes up, the first stretch has been transcribed.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let before_release = heard.lock().unwrap().clone();
        assert_eq!(before_release.len(), 1, "one stretch, up to the last breath: {before_release:?}");
        let seconds = before_release[0] as f32 / RATE as f32;
        assert!((18.0..=18.5).contains(&seconds), "cut at {seconds} s");

        // Releasing transcribes only what is left, and the text is all of it in order.
        let events = rig.run(ShellToWorker::StopRecording { request_id }).await;
        let after_release = heard.lock().unwrap().clone();
        assert_eq!(after_release.len(), 2, "the tail, and nothing else: {after_release:?}");
        assert!(after_release[1] < 9 * RATE, "only the last ~8 s: {after_release:?}");
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::Transcript { raw, .. } if raw == "trozo 1. trozo 2.")),
            "{events:?}"
        );
    }

    /// Answers nothing the first time it is given a long audio (a stretch lost),
    /// "resto" after; remembers how long each audio it got was.
    struct LosesTheFirst(Arc<Mutex<Vec<usize>>>);
    impl SpeechToText for LosesTheFirst {
        fn transcribe(&self, samples: &[f32]) -> Result<Transcript, TranscribeError> {
            if samples.len() <= 2 * 20_800 {
                return Ok(Transcript { text: String::new() });
            }
            let mut heard = self.0.lock().unwrap();
            heard.push(samples.len());
            Ok(Transcript { text: if heard.len() == 1 { String::new() } else { "resto.".to_string() } })
        }
    }

    #[tokio::test]
    async fn a_stretch_the_model_answers_with_nothing_is_transcribed_again_when_the_key_comes_up() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let audio = AudioContext {
            source: Arc::new(eva_audio::capture::mock::ScriptedSource::new(speech(26, &[(9.0, 9.5), (18.0, 18.5)]))),
            stt: Arc::new(LosesTheFirst(Arc::clone(&heard))),
            model_id: "mock".to_string(),
        };
        let mut rig = Rig::builder().audio(audio).build();
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(heard.lock().unwrap().len(), 1, "the first stretch was tried, and came back empty");

        let events = rig.run(ShellToWorker::StopRecording { request_id }).await;
        let lengths = heard.lock().unwrap().clone();
        assert_eq!(lengths.len(), 2, "{lengths:?}");
        assert!(lengths[1] >= 25 * RATE, "the whole recording again, not only its tail: {lengths:?}");
        assert!(
            events.iter().any(|e| matches!(e, WorkerToShell::Transcript { raw, .. } if raw == "resto.")),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_short_recording_is_transcribed_whole_when_the_key_comes_up() {
        let (mut rig, heard) = rig(speech(5, &[(2.0, 2.5)]));
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(heard.lock().unwrap().is_empty(), "nothing before the key comes up");

        rig.run(ShellToWorker::StopRecording { request_id }).await;
        assert_eq!(heard.lock().unwrap().clone(), vec![5 * RATE], "one call, the whole audio");
    }

    #[tokio::test]
    async fn a_cancelled_recording_stops_its_stream_and_nothing_is_pasted() {
        let (mut rig, heard) = rig(speech(26, &[(9.0, 9.5)]));
        let request_id = Uuid::new_v4();
        rig.run(ShellToWorker::StartRecording { request_id }).await;
        rig.run(ShellToWorker::Cancel { request_id }).await;
        let seen = heard.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(heard.lock().unwrap().len(), seen, "no more work after the cancel");
        assert!(rig.desktop.calls().is_empty());
    }
}
