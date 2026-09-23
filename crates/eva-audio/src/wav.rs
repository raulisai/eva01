//! Writing a recording to disk as a WAV file: the format the eval corpus
//! (`eval/README.md`) and `eva-eval` read — 16 kHz, mono, 16-bit.

use crate::capture::TARGET_SAMPLE_RATE;
use std::path::Path;
use thiserror::Error;

/// Why a WAV file could not be written.
#[derive(Debug, Error)]
#[error("no se pudo escribir el audio en {path}: {source}")]
pub struct WavError {
    path: String,
    source: hound::Error,
}

/// Writes `samples` (16 kHz mono, `-1.0..=1.0`) to `path` as 16-bit PCM.
/// Samples outside the range are clipped rather than wrapped.
///
/// # Errors
/// [`WavError`] if the file cannot be created or written.
pub fn write_mono_16k(path: &Path, samples: &[f32]) -> Result<(), WavError> {
    let fail = |source| WavError { path: path.display().to_string(), source };
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(fail)?;
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        writer.write_sample(pcm).map_err(fail)?;
    }
    writer.finalize().map_err(fail)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn what_is_written_reads_back_as_16k_mono_with_the_same_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let samples = [0.0, 0.5, -0.5, 0.25];

        write_mono_16k(&path, &samples).unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        let read: Vec<f32> = reader.samples::<i16>().map(|s| f32::from(s.unwrap()) / f32::from(i16::MAX)).collect();
        assert_eq!(read.len(), samples.len());
        for (wrote, got) in samples.iter().zip(read) {
            assert!((wrote - got).abs() < 1e-3, "{wrote} vs {got}");
        }
    }

    #[test]
    fn out_of_range_samples_are_clipped_not_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loud.wav");

        write_mono_16k(&path, &[3.0, -3.0]).unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        let read: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(read, vec![i16::MAX, -i16::MAX]);
    }

    #[test]
    fn a_missing_folder_is_a_typed_error_naming_the_path() {
        let error = write_mono_16k(Path::new("/no/existe/a.wav"), &[0.0]).unwrap_err();
        assert!(error.to_string().contains("/no/existe/a.wav"));
    }
}
