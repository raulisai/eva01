//! Whether a recording has a voice in it, as opposed to a room. Used to tell
//! "the model heard nothing because there was nothing" (drop it) from "the model
//! heard nothing although somebody spoke" (ask again, a second way).

use crate::TARGET_SAMPLE_RATE;

/// A 20 ms window this loud (RMS) is speech, not a room: a quiet room sits at
/// 0.001–0.005, a fan at 0.01, this user's voice at 0.03–0.1.
const WINDOW_LEVEL: f32 = 0.02;
/// This many such windows (0.16 s) make it a voice: one click is not.
const MIN_WINDOWS: usize = 8;

/// Whether `samples` (16 kHz) hold something that sounds like a person talking.
pub fn has_speech(samples: &[f32]) -> bool {
    let window = TARGET_SAMPLE_RATE as usize / 50;
    samples
        .chunks_exact(window)
        .filter(|chunk| (chunk.iter().map(|s| s * s).sum::<f32>() / window as f32).sqrt() >= WINDOW_LEVEL)
        .take(MIN_WINDOWS)
        .count()
        >= MIN_WINDOWS
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn a_room_a_fan_and_a_click_are_not_a_voice_but_talking_is() {
        assert!(!has_speech(&[]));
        assert!(!has_speech(&vec![0.003; 48_000]), "a quiet room");
        assert!(!has_speech(&vec![0.012; 48_000]), "a fan");
        let mut click = vec![0.002; 48_000];
        click[1_000..1_050].fill(0.9);
        assert!(!has_speech(&click), "a click");
        let mut talking = vec![0.002; 48_000];
        talking[8_000..20_000].fill(0.06);
        assert!(has_speech(&talking));
    }
}
