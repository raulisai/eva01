//! Cutting a long recording at its pauses, so a speech model that only copes
//! with utterance-sized audio can be fed one piece at a time.
//!
//! Measured on `canary-1b-flash` with continuous speech: 16 s and 25 s come
//! out at 0–4 % WER, but 90 s at 110 % (and 29 s of waiting — the cost grows
//! faster than the audio) and 227 s at 93 %. A dictation of a minute is
//! ordinary use, so the audio is cut before it reaches the model, at the
//! quietest moment near the limit — a breath between phrases, not the middle
//! of a word.

use crate::capture::TARGET_SAMPLE_RATE;

const RATE: usize = TARGET_SAMPLE_RATE as usize;

/// The longest piece handed to the model. 20 s sits under the 25 s measured
/// to work, with margin, and keeps each piece's wait short.
const MAX_LEN: usize = 20 * RATE;

/// How far back from the limit to look for a pause.
const SEARCH_LEN: usize = 6 * RATE;

/// The last piece is never shorter than this: a sliver of a word is worse
/// than a slightly early cut.
const MIN_TAIL: usize = 3 * RATE / 2;

/// Energy is measured over 50 ms (one breath of a syllable is longer, a zero
/// crossing inside a vowel is not), sliding every 10 ms.
const FRAME: usize = RATE / 20;
const HOP: usize = RATE / 100;

/// `samples` cut into consecutive pieces of at most 20 s each, at pauses.
/// Audio that already fits comes back whole; nothing is dropped or repeated,
/// so the pieces joined are exactly `samples`.
pub fn split_at_pauses(samples: &[f32]) -> Vec<&[f32]> {
    let mut pieces = Vec::new();
    let mut rest = samples;
    while rest.len() > MAX_LEN {
        // Cut no later than the limit, and early enough to leave a real tail.
        let latest = MAX_LEN.min(rest.len() - MIN_TAIL);
        let earliest = latest.saturating_sub(SEARCH_LEN);
        let cut = quietest_point(rest, earliest, latest);
        let (piece, tail) = rest.split_at(cut);
        pieces.push(piece);
        rest = tail;
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// Nothing is cut while less than this is waiting: a short dictation is
/// transcribed whole, when the key comes up, exactly as before.
const STREAM_MIN: usize = 8 * RATE;

/// A piece cut while recording is at least this long: shorter ones cost a
/// model call each and give it less to go on.
const STREAM_PIECE_MIN: usize = 5 * RATE;

/// The newest audio is never cut off: the speaker may be in the middle of a word.
const STREAM_GUARD: usize = 7 * RATE / 10;

/// A pause is this many quiet frames in a row (300 ms): a breath between
/// phrases, not the gap inside a word.
const PAUSE_FRAMES: usize = 6;

/// A frame is quiet below this share of how loud speech is in this very
/// recording (its 80th-percentile frame), so it holds for any microphone gain.
const QUIET_SHARE: f64 = 0.04;

/// Below this the recording is silence, whatever the share says.
const SILENCE_ENERGY: f64 = 1e-6;

/// While the user is still speaking: where the audio so far can be cut so that
/// the piece before the cut is transcribed now and only the rest waits for
/// the key to come up. `None` until there is enough and a real pause in it.
///
/// A cut is only ever made in a pause of at least 300 ms, as near the newest
/// audio as the 20 s limit allows; with no pause at all before that limit, at
/// the quietest point near it, as [`split_at_pauses`] would.
pub fn cut_while_recording(samples: &[f32]) -> Option<usize> {
    if samples.len() < STREAM_MIN {
        return None;
    }
    let energies: Vec<f64> = samples
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|frame| frame.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / FRAME as f64)
        .collect();
    let mut sorted = energies.clone();
    sorted.sort_by(f64::total_cmp);
    let loud = sorted.get(sorted.len() * 4 / 5).copied().unwrap_or(0.0);
    if loud < SILENCE_ENERGY {
        return None;
    }
    let quiet = |frame: usize| energies[frame] <= loud * QUIET_SHARE;

    let last_frame = ((samples.len() - STREAM_GUARD).min(MAX_LEN) / FRAME).min(energies.len());
    let first_frame = STREAM_PIECE_MIN / FRAME;
    let mut end = last_frame;
    while end >= first_frame + PAUSE_FRAMES {
        // The pause that ends at `end`, if the PAUSE_FRAMES frames before it are all quiet.
        if (end - PAUSE_FRAMES..end).all(quiet) {
            // Extend it back to its start, then cut in the middle.
            let mut start = end - PAUSE_FRAMES;
            while start > first_frame && quiet(start - 1) {
                start -= 1;
            }
            return Some((start + end) / 2 * FRAME);
        }
        end -= 1;
    }
    (samples.len() >= MAX_LEN + STREAM_GUARD).then(|| quietest_point(samples, MAX_LEN - SEARCH_LEN, MAX_LEN))
}

/// Where an utterance said without holding the key stands, so listening can
/// stop on its own: nothing said yet, someone talking, or done talking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// No speech yet.
    Waiting,
    /// Speech has started and has not ended.
    Speaking,
    /// Speech started and has been followed by a long enough pause.
    Ended,
}

/// 30 ms frames for telling speech from room noise.
const EP_FRAME: usize = RATE * 3 / 100;
/// Speech needs this many voiced frames in a row to count (180 ms): a cough or
/// a click is not someone starting to talk.
const EP_ONSET_FRAMES: usize = 6;
/// A pause this long after speech ends the utterance (900 ms): longer than the
/// breath between phrases, short enough not to feel slow.
const EP_END_FRAMES: usize = 30;
/// A frame is voiced above this RMS whatever the room (speech on a laptop mic
/// is well above it), or above three times the room's own quiet level.
const EP_MIN_RMS: f32 = 0.012;
/// …but never above this, so a loud room cannot raise the bar over a voice.
const EP_MAX_THRESHOLD: f32 = 0.035;

/// Where the utterance in `samples` (16 kHz mono, from the start of the
/// listening) stands. The room's quiet level is its quietest tenth — low
/// enough that nine seconds of talking before the first pause still leave it
/// at the room, not at the voice.
pub fn endpoint(samples: &[f32]) -> Endpoint {
    let rms: Vec<f32> = samples
        .chunks(EP_FRAME)
        .filter(|frame| frame.len() == EP_FRAME)
        .map(|frame| (frame.iter().map(|s| s * s).sum::<f32>() / EP_FRAME as f32).sqrt())
        .collect();
    if rms.is_empty() {
        return Endpoint::Waiting;
    }
    let mut sorted = rms.clone();
    sorted.sort_by(f32::total_cmp);
    let quiet = sorted[sorted.len() / 10];
    let threshold = EP_MIN_RMS.max(quiet * 3.0).min(EP_MAX_THRESHOLD);
    let voiced: Vec<bool> = rms.iter().map(|r| *r >= threshold).collect();

    let mut run = 0;
    let onset = voiced.iter().position(|v| {
        run = if *v { run + 1 } else { 0 };
        run >= EP_ONSET_FRAMES
    });
    let Some(onset) = onset else { return Endpoint::Waiting };
    let after = &voiced[onset + 1..];
    let trailing_quiet = after.iter().rev().take_while(|v| !**v).count();
    if trailing_quiet >= EP_END_FRAMES {
        Endpoint::Ended
    } else {
        Endpoint::Speaking
    }
}

/// The middle of the quietest 50 ms window that starts in `earliest..latest`
/// (the latest one on a tie, so pieces come out as long as they can).
fn quietest_point(samples: &[f32], earliest: usize, latest: usize) -> usize {
    let mut best = (f64::INFINITY, latest);
    let mut start = earliest;
    while start + FRAME <= latest.max(earliest + FRAME) && start + FRAME <= samples.len() {
        let energy: f64 = samples[start..start + FRAME].iter().map(|s| f64::from(*s).powi(2)).sum();
        if energy <= best.0 {
            best = (energy, start + FRAME / 2);
        }
        start += HOP;
    }
    best.1.clamp(1, latest.max(1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    /// Loud "speech" everywhere except quiet gaps at the given seconds.
    fn speech_with_gaps(seconds: usize, gaps: &[(f32, f32)]) -> Vec<f32> {
        let mut samples = vec![0.3; seconds * RATE];
        for (from, to) in gaps {
            for s in &mut samples[(from * RATE as f32) as usize..(to * RATE as f32) as usize] {
                *s = 0.001;
            }
        }
        samples
    }

    fn joined(pieces: &[&[f32]]) -> Vec<f32> {
        pieces.concat()
    }

    #[test]
    fn audio_that_fits_is_left_whole() {
        let samples = vec![0.3; 20 * RATE];
        assert_eq!(split_at_pauses(&samples), vec![samples.as_slice()]);
        assert!(split_at_pauses(&[]).is_empty());
    }

    #[test]
    fn a_long_recording_is_cut_inside_the_pauses_not_inside_the_words() {
        // Pauses at 15 s and 33 s; 50 s in total.
        let samples = speech_with_gaps(50, &[(14.8, 15.2), (33.0, 33.4)]);
        let pieces = split_at_pauses(&samples);

        assert_eq!(joined(&pieces), samples, "nothing dropped, nothing repeated");
        assert!(pieces.iter().all(|p| p.len() <= MAX_LEN));
        let first_cut = pieces[0].len() as f32 / RATE as f32;
        assert!((14.8..=15.2).contains(&first_cut), "first cut at {first_cut} s");
        let second_cut = (pieces[0].len() + pieces[1].len()) as f32 / RATE as f32;
        assert!((33.0..=33.4).contains(&second_cut), "second cut at {second_cut} s");
    }

    #[test]
    fn with_no_pause_at_all_it_still_cuts_at_the_limit() {
        let samples = vec![0.3; 65 * RATE];
        let pieces = split_at_pauses(&samples);
        assert_eq!(joined(&pieces), samples);
        assert!(pieces.iter().all(|p| p.len() <= MAX_LEN && !p.is_empty()));
        assert_eq!(pieces.len(), 4, "65 s is 20 + 20 + 20 + 5");
    }

    #[test]
    fn the_last_piece_is_never_a_sliver() {
        // 20.2 s: cutting at the limit would leave 0.2 s.
        for total in [20 * RATE + 3_200, 20 * RATE + 1, 40 * RATE + 100] {
            let samples = vec![0.3; total];
            let pieces = split_at_pauses(&samples);
            let last = pieces.last().unwrap();
            assert!(last.len() >= MIN_TAIL, "{total}: last piece is {} samples", last.len());
        }
    }

    #[test]
    fn nothing_is_cut_until_there_is_enough_and_a_pause_in_it() {
        assert_eq!(cut_while_recording(&speech_with_gaps(7, &[(3.0, 3.5)])), None, "too short");
        assert_eq!(cut_while_recording(&vec![0.3; 12 * RATE]), None, "12 s with no pause: keep listening");
        assert_eq!(cut_while_recording(&vec![0.0; 12 * RATE]), None, "silence is not speech");
    }

    #[test]
    fn a_pause_in_the_speech_so_far_is_where_the_first_piece_ends() {
        // 12 s so far, a breath at 9.0–9.5 s, speech again after it.
        let samples = speech_with_gaps(12, &[(9.0, 9.5)]);
        let cut = cut_while_recording(&samples).expect("a pause to cut at") as f32 / RATE as f32;
        assert!((9.0..=9.5).contains(&cut), "cut at {cut} s");
    }

    #[test]
    fn the_latest_pause_wins_and_a_pause_too_early_or_too_late_does_not_count() {
        // A breath at 2 s (too early: the piece would be a sliver) and at 6.0 s and 9.2 s;
        // the newest 0.7 s stay unprocessed, so 11.8 s of pause at the end is not one.
        let samples = speech_with_gaps(12, &[(2.0, 2.5), (6.0, 6.5), (9.2, 9.6), (11.6, 12.0)]);
        let cut = cut_while_recording(&samples).unwrap() as f32 / RATE as f32;
        assert!((9.2..=9.6).contains(&cut), "cut at {cut} s");
    }

    #[test]
    fn a_short_gap_inside_a_word_is_not_a_pause() {
        let samples = speech_with_gaps(12, &[(9.0, 9.15)]);
        assert_eq!(cut_while_recording(&samples), None);
    }

    #[test]
    fn the_same_pause_is_found_at_any_microphone_gain() {
        let quiet_mic: Vec<f32> = speech_with_gaps(12, &[(9.0, 9.5)]).iter().map(|s| s * 0.05).collect();
        assert!(cut_while_recording(&quiet_mic).is_some());
    }

    #[test]
    fn with_no_pause_past_the_limit_it_still_cuts_near_it() {
        let samples = vec![0.3; 22 * RATE];
        let cut = cut_while_recording(&samples).unwrap();
        assert!(cut <= MAX_LEN && cut > MAX_LEN - SEARCH_LEN, "cut at {cut}");
    }

    /// `seconds` of something: speech-level noise or room quiet.
    fn stretch(seconds: f32, loud: bool) -> Vec<f32> {
        let n = (seconds * RATE as f32) as usize;
        (0..n).map(|i| if loud { 0.08 * ((i as f32) * 0.3).sin() } else { 0.002 * ((i as f32) * 0.7).sin() }).collect()
    }

    #[test]
    fn listening_waits_while_nothing_is_said_and_a_click_does_not_count() {
        assert_eq!(endpoint(&[]), Endpoint::Waiting);
        assert_eq!(endpoint(&stretch(3.0, false)), Endpoint::Waiting);
        let mut click = stretch(1.0, false);
        click.extend(stretch(0.06, true));
        click.extend(stretch(1.0, false));
        assert_eq!(endpoint(&click), Endpoint::Waiting, "60 ms is a click, not someone talking");
    }

    #[test]
    fn speech_followed_by_a_long_enough_pause_ends_the_utterance() {
        let mut said = stretch(0.5, false);
        said.extend(stretch(1.2, true));
        assert_eq!(endpoint(&said), Endpoint::Speaking);
        said.extend(stretch(0.4, false));
        assert_eq!(endpoint(&said), Endpoint::Speaking, "a breath between phrases is not the end");
        said.extend(stretch(0.7, false));
        assert_eq!(endpoint(&said), Endpoint::Ended);
    }

    #[test]
    fn a_long_sentence_before_the_first_pause_still_ends() {
        let mut said = stretch(0.3, false);
        said.extend(stretch(7.0, true));
        said.extend(stretch(1.0, false));
        assert_eq!(endpoint(&said), Endpoint::Ended);
    }

    #[test]
    fn a_noisy_room_raises_the_bar_instead_of_counting_as_speech() {
        let noisy: Vec<f32> = (0..3 * RATE).map(|i| 0.02 * ((i as f32) * 0.9).sin()).collect();
        assert_eq!(endpoint(&noisy), Endpoint::Waiting);
    }

    proptest::proptest! {
        #[test]
        fn pieces_always_cover_the_audio_exactly_and_respect_the_limit(len in 0usize..(90 * RATE)) {
            let samples: Vec<f32> = (0..len).map(|i| if (i / 4_000) % 7 == 0 { 0.0 } else { 0.2 }).collect();
            let pieces = split_at_pauses(&samples);
            proptest::prop_assert_eq!(&joined(&pieces), &samples);
            proptest::prop_assert!(pieces.iter().all(|p| !p.is_empty() && p.len() <= MAX_LEN));
        }
    }
}
