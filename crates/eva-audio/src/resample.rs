//! Converts audio from the microphone's native sample rate to the 16 kHz
//! mono the STT models expect.
//!
//! The first version of this module interpolated linearly, one microphone
//! callback at a time. Measured against a pure tone, that was wrong in two
//! ways that matter for a live stream: it restarted at every callback (the
//! output for a 512-frame buffer at 48 kHz is 170.67 samples, so rounding to
//! 171 stretched the audio and repeated a sample every ~10 ms — the 440 Hz
//! tone kept 2 % of its energy), and it had no anti-aliasing filter (a 12 kHz
//! tone, above what 16 kHz can carry, came out at full strength as a 4 kHz
//! one). [`StreamResampler`] keeps its state between calls and filters.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use thiserror::Error;

/// Samples fed to the resampler per processing step. Only a latency/quality
/// knob: callers may push any amount at a time.
const CHUNK_FRAMES: usize = 1_024;

/// Why a resampler could not be built.
#[derive(Debug, Error)]
#[error("no se pudo crear el remuestreador de {input_rate} Hz a {output_rate} Hz: {reason}")]
pub struct ResampleError {
    input_rate: u32,
    output_rate: u32,
    reason: String,
}

/// A mono resampler for a live stream: push audio in whatever sizes it
/// arrives, get back everything that is ready.
///
/// The filter delays the audio by a few milliseconds, and up to one chunk
/// (about 20 ms) of the very end of a recording stays inside it when the
/// stream stops — a push-to-talk release lands after the last word, and the
/// speech model is given silence after it anyway.
pub struct StreamResampler {
    /// `None` when the rates already match.
    fft: Option<Fft<f32>>,
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

impl StreamResampler {
    /// A resampler from `input_rate` to `output_rate` Hz.
    ///
    /// # Errors
    /// [`ResampleError`] if a rate is zero.
    pub fn new(input_rate: u32, output_rate: u32) -> Result<StreamResampler, ResampleError> {
        let fail = |reason: String| ResampleError { input_rate, output_rate, reason };
        if input_rate == 0 || output_rate == 0 {
            return Err(fail("la frecuencia no puede ser cero".to_string()));
        }
        if input_rate == output_rate {
            return Ok(StreamResampler { fft: None, pending: Vec::new(), scratch: Vec::new() });
        }
        let fft = Fft::<f32>::new(input_rate as usize, output_rate as usize, CHUNK_FRAMES, 1, FixedSync::Input)
            .map_err(|e| fail(e.to_string()))?;
        let scratch = vec![0.0; fft.output_frames_max()];
        Ok(StreamResampler { fft: Some(fft), pending: Vec::new(), scratch })
    }

    /// Resamples `mono`, returning whatever output is ready (possibly none
    /// yet: the input is consumed in whole chunks, the remainder waits for the
    /// next call).
    pub fn process(&mut self, mono: &[f32]) -> Vec<f32> {
        let Some(fft) = self.fft.as_mut() else { return mono.to_vec() };
        self.pending.extend_from_slice(mono);

        let mut output = Vec::new();
        loop {
            let needed = fft.input_frames_next();
            if self.pending.len() < needed {
                break;
            }
            let written = {
                let Ok(input) = InterleavedSlice::new(&self.pending[..needed], 1, needed) else { break };
                let frames = self.scratch.len();
                let Ok(mut out) = InterleavedSlice::new_mut(&mut self.scratch[..], 1, frames) else { break };
                match fft.process_into_buffer(&input, &mut out, None) {
                    Ok((_, written)) => written,
                    Err(e) => {
                        tracing::error!("el remuestreo falló y se descartó un bloque de audio: {e}");
                        0
                    }
                }
            };
            output.extend_from_slice(&self.scratch[..written]);
            self.pending.drain(..needed);
        }
        output
    }
}

/// Downmixes interleaved multi-channel audio to mono by averaging channels.
/// Returns `input` unchanged (as a `Vec`) if `channels <= 1`.
pub fn downmix_to_mono(input: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return input.to_vec();
    }
    let channels = channels as usize;
    input.chunks(channels).map(|frame| frame.iter().sum::<f32>() / frame.len() as f32).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    const OUT_RATE: u32 = 16_000;

    fn tone(freq: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// How much of the signal's energy sits at `freq`: 1.0 for a clean tone.
    fn purity(samples: &[f32], freq: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / f64::from(OUT_RATE);
        let (mut re, mut im) = (0.0, 0.0);
        for (i, s) in samples.iter().enumerate() {
            re += f64::from(*s) * (w * i as f64).cos();
            im += f64::from(*s) * (w * i as f64).sin();
        }
        let at_freq = 2.0 * (re * re + im * im) / samples.len() as f64;
        at_freq / samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
    }

    /// The whole input, pushed in microphone-callback-sized pieces, minus the
    /// filter's start-up (the first 30 ms).
    fn resample_in_pieces(input: &[f32], rate: u32, piece: usize) -> Vec<f32> {
        let mut resampler = StreamResampler::new(rate, OUT_RATE).unwrap();
        let output: Vec<f32> = input.chunks(piece).flat_map(|p| resampler.process(p)).collect();
        output[480..].to_vec()
    }

    #[test]
    fn a_tone_comes_out_clean_whatever_the_callback_size() {
        // 512 frames at 48 kHz is what macOS hands over: not a multiple of the
        // 3:1 ratio, which is what broke the per-callback linear version.
        for (rate, piece) in [(48_000, 512), (48_000, 480), (44_100, 512), (44_100, 441), (48_000, 7)] {
            let out = resample_in_pieces(&tone(440.0, rate, rate as usize * 2), rate, piece);
            let purity = purity(&out, 440.0);
            assert!(purity > 0.999, "{rate} Hz in pieces of {piece}: purity {purity}");
        }
    }

    #[test]
    fn the_duration_is_preserved() {
        let rate = 44_100;
        let out = resample_in_pieces(&tone(440.0, rate, rate as usize * 4), rate, 512);
        // 4 s in, minus the 30 ms start-up and at most a chunk held back.
        let seconds = out.len() as f64 / f64::from(OUT_RATE);
        assert!((3.9..=4.0).contains(&seconds), "{seconds} s");
    }

    #[test]
    fn a_tone_above_the_new_nyquist_is_filtered_out_not_folded_into_the_band() {
        let input = tone(12_000.0, 48_000, 96_000);
        let out = resample_in_pieces(&input, 48_000, 512);
        // The linear version passed it through at full strength as a 4 kHz tone.
        assert!(rms(&out) < 0.02 * rms(&input), "rms {} vs {}", rms(&out), rms(&input));
    }

    #[test]
    fn speech_band_levels_are_preserved() {
        for freq in [300.0, 1_000.0, 3_000.0, 6_000.0] {
            let input = tone(freq, 48_000, 96_000);
            let out = resample_in_pieces(&input, 48_000, 512);
            let ratio = rms(&out) / rms(&input);
            assert!((0.97..=1.03).contains(&ratio), "{freq} Hz: level ratio {ratio}");
        }
    }

    #[test]
    fn how_the_input_is_split_does_not_change_the_output() {
        let input = tone(700.0, 48_000, 48_000);
        let whole = StreamResampler::new(48_000, OUT_RATE).unwrap().process(&input);
        let mut pieces = StreamResampler::new(48_000, OUT_RATE).unwrap();
        let split: Vec<f32> = input.chunks(97).flat_map(|p| pieces.process(p)).collect();
        assert_eq!(whole.len(), split.len());
        assert!(whole.iter().zip(&split).all(|(a, b)| (a - b).abs() < 1e-5));
    }

    #[test]
    fn the_same_rate_passes_through_untouched() {
        let mut resampler = StreamResampler::new(16_000, 16_000).unwrap();
        assert_eq!(resampler.process(&[0.1, 0.2, 0.3]), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn a_zero_rate_is_a_typed_error() {
        assert!(StreamResampler::new(0, 16_000).is_err());
        assert!(StreamResampler::new(48_000, 0).is_err());
    }

    #[test]
    fn downmix_averages_stereo_to_mono() {
        // L, R, L, R
        let stereo = vec![1.0, 0.0, 0.0, 1.0];
        assert_eq!(downmix_to_mono(&stereo, 2), vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_with_one_channel_is_a_no_op() {
        let mono = vec![0.1, 0.2, 0.3];
        assert_eq!(downmix_to_mono(&mono, 1), mono);
    }

    proptest::proptest! {
        #[test]
        fn never_panics_for_the_rates_real_microphones_use(
            input in proptest::collection::vec(-1.0f32..1.0f32, 0..3_000),
            input_rate in proptest::sample::select(vec![8_000u32, 11_025, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000]),
            piece in 1usize..700,
        ) {
            if let Ok(mut resampler) = StreamResampler::new(input_rate, OUT_RATE) {
                for chunk in input.chunks(piece) {
                    let _ = resampler.process(chunk);
                }
            }
        }
    }
}
