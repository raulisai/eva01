//! Microphone capture: opens the default input device and delivers mono,
//! resampled f32 audio in fixed-size chunks, ready for [`crate::vad`].
//!
//! Per `docs/ENGINEERING.md` #5, real capture sits behind [`AudioSource`] so
//! [`Rechunker`] — the actual logic worth testing carefully — is tested
//! without a real microphone or `cpal` device at all.

use crate::resample::{downmix_to_mono, StreamResampler};
use std::collections::VecDeque;
use thiserror::Error;

/// Errors starting or running audio capture.
#[derive(Debug, Error)]
pub enum AudioError {
    /// No input device is available on this system.
    #[error("no se encontró ningún dispositivo de entrada de audio")]
    NoInputDevice,
    /// The device's default input configuration could not be read.
    #[error("no se pudo leer la configuración del micrófono: {0}")]
    ConfigFailed(String),
    /// The input stream could not be built or started.
    #[error("no se pudo iniciar la captura de audio: {0}")]
    StreamFailed(String),
}

/// Accumulates variable-sized pushes of samples into fixed-size chunks.
/// `cpal` delivers whatever buffer size the OS gives it, which rarely lines
/// up with the VAD's required chunk size — this is the seam between the two.
pub struct Rechunker {
    buffer: VecDeque<f32>,
    chunk_size: usize,
}

impl Rechunker {
    /// Builds a rechunker that yields chunks of exactly `chunk_size` samples.
    pub fn new(chunk_size: usize) -> Self {
        Rechunker { buffer: VecDeque::new(), chunk_size }
    }

    /// Pushes new samples and returns every complete chunk they produced, in
    /// order. Leftover samples that do not fill a whole chunk are kept for
    /// the next call.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.buffer.extend(samples);
        let mut chunks = Vec::new();
        while self.buffer.len() >= self.chunk_size {
            let chunk: Vec<f32> = self.buffer.drain(..self.chunk_size).collect();
            chunks.push(chunk);
        }
        chunks
    }
}

/// A handle to a running capture stream; dropping or calling [`Self::stop`]
/// stops it.
pub trait CaptureHandle: Send {
    /// Stops the capture stream.
    fn stop(self: Box<Self>);
}

/// Something that can capture audio and deliver fixed-size, 16 kHz mono
/// chunks to a callback.
pub trait AudioSource: Send + Sync {
    /// Starts capturing. `chunk_size` is the size (in 16 kHz-mono samples)
    /// of each `Vec<f32>` passed to `on_chunk`. Returns a handle to stop it.
    ///
    /// # Errors
    /// Returns [`AudioError`] if no input device is available or the stream
    /// could not be started.
    fn start(
        &self,
        chunk_size: usize,
        on_chunk: Box<dyn FnMut(Vec<f32>) + Send>,
    ) -> Result<Box<dyn CaptureHandle>, AudioError>;
}

/// What every microphone callback goes through: mix down to mono, bring to
/// 16 kHz, cut into fixed-size chunks. Kept apart from `cpal` so the whole
/// path is tested with synthetic buffers, sized like the ones macOS delivers.
pub struct CapturePipeline {
    channels: u16,
    resampler: StreamResampler,
    rechunker: Rechunker,
}

impl CapturePipeline {
    /// A pipeline for a device delivering `channels` interleaved channels at
    /// `input_rate` Hz, producing chunks of `chunk_size` 16 kHz samples.
    ///
    /// # Errors
    /// [`AudioError::ConfigFailed`] if the device's rate cannot be converted.
    pub fn new(input_rate: u32, channels: u16, chunk_size: usize) -> Result<CapturePipeline, AudioError> {
        let resampler = StreamResampler::new(input_rate, TARGET_SAMPLE_RATE)
            .map_err(|e| AudioError::ConfigFailed(e.to_string()))?;
        Ok(CapturePipeline { channels, resampler, rechunker: Rechunker::new(chunk_size) })
    }

    /// Feeds one device buffer; returns the chunks it completed.
    pub fn push(&mut self, interleaved: &[f32]) -> Vec<Vec<f32>> {
        let mono = downmix_to_mono(interleaved, self.channels);
        self.rechunker.push(&self.resampler.process(&mono))
    }
}

/// The real `cpal`-backed [`AudioSource`]: the default input device,
/// downmixed to mono and resampled to 16 kHz.
pub struct MicrophoneSource;

/// The sample rate every chunk this crate produces is normalized to.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

struct CpalHandle {
    _stream: cpal::Stream,
}

impl CaptureHandle for CpalHandle {
    fn stop(self: Box<Self>) {
        // Dropping a `cpal::Stream` stops it; nothing else to do, but the
        // explicit method (rather than relying on the caller remembering to
        // drop the box at the right time) makes the intent readable at the
        // call site.
    }
}

/// What the default microphone is, for `eva doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputInfo {
    /// The device's name, as macOS shows it.
    pub name: String,
    /// The rate it delivers, before EVA01 brings it to 16 kHz.
    pub sample_rate: u32,
    /// How many channels it has (mixed down to one).
    pub channels: u16,
}

impl MicrophoneSource {
    /// Describes the default input device without opening a stream (so it
    /// neither asks for the microphone permission nor makes the indicator
    /// light up).
    ///
    /// # Errors
    /// [`AudioError::NoInputDevice`] if there is none, [`AudioError::ConfigFailed`]
    /// if it cannot say how it delivers audio.
    pub fn default_input() -> Result<InputInfo, AudioError> {
        use cpal::traits::{DeviceTrait, HostTrait};

        let device = cpal::default_host().default_input_device().ok_or(AudioError::NoInputDevice)?;
        let config = device.default_input_config().map_err(|e| AudioError::ConfigFailed(e.to_string()))?;
        Ok(InputInfo {
            name: device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "micrófono".to_string()),
            sample_rate: config.sample_rate(),
            channels: config.channels(),
        })
    }
}

impl AudioSource for MicrophoneSource {
    fn start(
        &self,
        chunk_size: usize,
        mut on_chunk: Box<dyn FnMut(Vec<f32>) + Send>,
    ) -> Result<Box<dyn CaptureHandle>, AudioError> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(AudioError::NoInputDevice)?;
        let config = device.default_input_config().map_err(|e| AudioError::ConfigFailed(e.to_string()))?;

        let mut pipeline = CapturePipeline::new(config.sample_rate(), config.channels(), chunk_size)?;
        let data_callback = move |data: &[f32], _: &cpal::InputCallbackInfo| {
            for chunk in pipeline.push(data) {
                on_chunk(chunk);
            }
        };
        let error_callback = |err: cpal::Error| {
            tracing::error!("error en el stream de captura de audio: {err}");
        };

        let stream = device
            .build_input_stream(config.into(), data_callback, error_callback, None)
            .map_err(|e| AudioError::StreamFailed(e.to_string()))?;
        stream.play().map_err(|e| AudioError::StreamFailed(e.to_string()))?;

        Ok(Box::new(CpalHandle { _stream: stream }))
    }
}

/// A test double for [`AudioSource`], per `docs/ENGINEERING.md` #5: replays
/// pre-recorded samples instead of opening a real microphone.
pub mod mock {
    use super::{AudioError, AudioSource, CaptureHandle, Rechunker};

    /// Replays a fixed buffer of already-16kHz-mono samples through
    /// [`AudioSource::start`]'s callback, immediately and synchronously
    /// (no real streaming thread), rechunked exactly as the real source
    /// would.
    pub struct ScriptedSource {
        samples: Vec<f32>,
    }

    impl ScriptedSource {
        /// Builds a source that will replay exactly `samples` once started.
        pub fn new(samples: Vec<f32>) -> Self {
            ScriptedSource { samples }
        }
    }

    struct NoopHandle;
    impl CaptureHandle for NoopHandle {
        fn stop(self: Box<Self>) {}
    }

    impl AudioSource for ScriptedSource {
        fn start(
            &self,
            chunk_size: usize,
            mut on_chunk: Box<dyn FnMut(Vec<f32>) + Send>,
        ) -> Result<Box<dyn CaptureHandle>, AudioError> {
            let mut rechunker = Rechunker::new(chunk_size);
            for chunk in rechunker.push(&self.samples) {
                on_chunk(chunk);
            }
            Ok(Box::new(NoopHandle))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::mock::ScriptedSource;
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_stereo_48k_stream_in_512_frame_buffers_arrives_as_clean_16k_chunks() {
        // The shape of this Mac's own microphone path: 48 kHz, buffers of 512
        // frames (not a multiple of the 3:1 ratio), two channels carrying one
        // 440 Hz tone.
        let mut pipeline = CapturePipeline::new(48_000, 2, 1_600).unwrap();
        let mut chunks = Vec::new();
        for start in (0..48_000 * 2).step_by(512) {
            let frames = 512.min(48_000 * 2 - start);
            let buffer: Vec<f32> = (start..start + frames)
                .flat_map(|i| {
                    let s = (0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48_000.0).sin()) as f32;
                    [s, s]
                })
                .collect();
            chunks.extend(pipeline.push(&buffer));
        }

        assert!(chunks.iter().all(|c| c.len() == 1_600), "every chunk is exactly the size asked for");
        // 2 s at 16 kHz is 32 000 samples = 20 chunks, less what the filter holds back.
        assert!((18..=20).contains(&chunks.len()), "{} chunks", chunks.len());
        let samples: Vec<f32> = chunks.concat();
        let x = &samples[480..];
        let w = 2.0 * std::f64::consts::PI * 440.0 / 16_000.0;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, s) in x.iter().enumerate() {
            re += f64::from(*s) * (w * i as f64).cos();
            im += f64::from(*s) * (w * i as f64).sin();
        }
        let purity = 2.0 * (re * re + im * im) / x.len() as f64 / x.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
        assert!(purity > 0.999, "the tone must survive intact: purity {purity}");
    }

    #[test]
    fn rechunker_buffers_partial_pushes_until_a_full_chunk_is_available() {
        let mut r = Rechunker::new(4);
        assert!(r.push(&[1.0, 2.0]).is_empty(), "only 2 of 4 samples buffered, no chunk yet");
        let chunks = r.push(&[3.0, 4.0, 5.0]);
        assert_eq!(chunks, vec![vec![1.0, 2.0, 3.0, 4.0]]);
        // The leftover `5.0` is retained for the next push.
        let chunks2 = r.push(&[6.0, 7.0, 8.0]);
        assert_eq!(chunks2, vec![vec![5.0, 6.0, 7.0, 8.0]]);
    }

    #[test]
    fn rechunker_can_emit_multiple_chunks_from_one_large_push() {
        let mut r = Rechunker::new(2);
        let chunks = r.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(chunks, vec![vec![1.0, 2.0], vec![3.0, 4.0], vec![5.0, 6.0]]);
    }

    #[test]
    fn rechunker_with_exactly_zero_leftover_starts_clean_next_time() {
        let mut r = Rechunker::new(3);
        assert_eq!(r.push(&[1.0, 2.0, 3.0]), vec![vec![1.0, 2.0, 3.0]]);
        assert!(r.push(&[]).is_empty());
    }

    #[test]
    fn scripted_source_delivers_rechunked_samples_to_the_callback() {
        let source = ScriptedSource::new((0..10).map(|i| i as f32).collect());
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        let handle = source
            .start(
                4,
                Box::new(move |chunk| {
                    #[allow(clippy::unwrap_used)]
                    received_clone.lock().unwrap().push(chunk);
                }),
            )
            .expect("scripted source never fails to start");
        handle.stop();

        let received = received.lock().expect("mutex must not be poisoned");
        assert_eq!(*received, vec![vec![0.0, 1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0, 7.0]]);
    }
}
