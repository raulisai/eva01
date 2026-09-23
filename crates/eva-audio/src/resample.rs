//! Converts audio from the microphone's native sample rate to the 16 kHz
//! mono the VAD and STT models expect.
//!
//! **Scope, stated plainly:** `docs/PLAN.md` §3 names `rubato` for this.
//! This module instead implements linear interpolation directly: it is
//! simple enough to write and test with total confidence in the time this
//! increment had, and linear interpolation is a standard, adequate choice
//! for speech resampling (it is not audio mastering — a few dB of aliasing
//! above the speech band does not move WER the way a wrong VAD threshold or
//! a missing accent-fold does). If the eval corpus (`docs/PLAN.md` fase 3)
//! ever shows resampling quality is the bottleneck, swapping in `rubato`'s
//! sinc interpolation behind this same function signature is a contained,
//! measurable follow-up — not a guess made now without the data to justify it.

/// Resamples `input` (mono) from `input_rate` Hz to `output_rate` Hz using
/// linear interpolation. Returns an empty vector if `input` is empty.
pub fn resample_linear(input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if input.is_empty() || input_rate == 0 || output_rate == 0 {
        return Vec::new();
    }
    if input_rate == output_rate {
        return input.to_vec();
    }

    let ratio = output_rate as f64 / input_rate as f64;
    let output_len = ((input.len() as f64) * ratio).round() as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        // The fractional position this output sample falls at in the input.
        let src_pos = i as f64 / ratio;
        let src_index = src_pos.floor() as usize;
        let frac = (src_pos - src_index as f64) as f32;

        let sample = if src_index + 1 < input.len() {
            input[src_index] * (1.0 - frac) + input[src_index + 1] * frac
        } else {
            // Past the last full interval: hold the final sample rather than
            // reading out of bounds.
            *input.last().unwrap_or(&0.0)
        };
        output.push(sample);
    }

    output
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

    #[test]
    fn same_rate_is_a_no_op() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input);
    }

    #[test]
    fn downsampling_halves_the_length() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let output = resample_linear(&input, 32_000, 16_000);
        assert_eq!(output.len(), 50);
    }

    #[test]
    fn upsampling_doubles_the_length() {
        let input: Vec<f32> = (0..50).map(|i| i as f32).collect();
        let output = resample_linear(&input, 16_000, 32_000);
        assert_eq!(output.len(), 100);
    }

    #[test]
    fn a_constant_signal_resamples_to_the_same_constant() {
        let input = vec![0.5_f32; 100];
        let output = resample_linear(&input, 48_000, 16_000);
        for sample in output {
            assert!((sample - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn empty_input_produces_empty_output() {
        assert!(resample_linear(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn zero_rates_do_not_panic_or_divide_by_zero() {
        assert!(resample_linear(&[1.0, 2.0], 0, 16_000).is_empty());
        assert!(resample_linear(&[1.0, 2.0], 16_000, 0).is_empty());
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
        fn resample_linear_never_panics(
            input in proptest::collection::vec(-1.0f32..1.0f32, 0..200),
            input_rate in 0u32..96_000,
            output_rate in 0u32..96_000,
        ) {
            let _ = resample_linear(&input, input_rate, output_rate);
        }
    }
}
