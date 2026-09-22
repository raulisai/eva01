//! Scans a corpus directory for `.wav`/`.txt` pairs — the eval corpus from
//! `docs/PLAN.md` fase 3, harvested from real use (the "esto salió mal"
//! hotkey and `eva-store`'s saved transcripts), not written by hand.

use std::path::{Path, PathBuf};

/// One evaluation sample: an audio file and the text it is expected to
/// transcribe to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The sample's name (the shared file stem), used in reports.
    pub name: String,
    /// Path to the `.wav` file.
    pub wav_path: PathBuf,
    /// The expected transcript, exactly as the pipeline should produce it
    /// (after cleaning) — trimmed of surrounding whitespace.
    pub reference: String,
}

/// Scans `dir` for every `<name>.wav` that has a matching `<name>.txt`,
/// returned in a fixed, deterministic order (sorted by name) so repeated
/// runs are directly comparable. A `.wav` with no matching `.txt` is
/// skipped with a warning printed to stderr, not an error — a corpus
/// growing incrementally (new recordings dropped in before their reference
/// text is written) is the expected, normal state, not a broken one.
pub fn scan(dir: &Path) -> std::io::Result<Vec<Sample>> {
    let mut samples = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "wav") {
            continue;
        }

        let txt_path = path.with_extension("txt");
        match std::fs::read_to_string(&txt_path) {
            Ok(reference) => {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(nombre inválido)")
                    .to_string();
                samples.push(Sample { name, wav_path: path, reference: reference.trim().to_string() });
            }
            Err(_) => {
                eprintln!("aviso: {} no tiene un .txt de referencia; se omite", path.display());
            }
        }
    }

    samples.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(samples)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).expect("writing a fixture file must succeed");
    }

    #[test]
    fn finds_a_wav_txt_pair() {
        let dir = tempfile::tempdir().expect("tempdir must succeed");
        write(dir.path(), "hola.wav", "");
        write(dir.path(), "hola.txt", "Hola, mundo.\n");

        let samples = scan(dir.path()).expect("scan must succeed");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].name, "hola");
        assert_eq!(samples[0].reference, "Hola, mundo.");
    }

    #[test]
    fn skips_a_wav_with_no_matching_txt_instead_of_erroring() {
        let dir = tempfile::tempdir().expect("tempdir must succeed");
        write(dir.path(), "huerfano.wav", "");

        let samples = scan(dir.path()).expect("scan must not error just because a pair is incomplete");
        assert!(samples.is_empty());
    }

    #[test]
    fn ignores_non_wav_files() {
        let dir = tempfile::tempdir().expect("tempdir must succeed");
        write(dir.path(), "notas.txt", "esto no es un par");
        write(dir.path(), "README.md", "documentación del corpus");

        let samples = scan(dir.path()).expect("scan must succeed");
        assert!(samples.is_empty());
    }

    #[test]
    fn returns_samples_in_a_deterministic_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir must succeed");
        for name in ["zeta", "alfa", "medio"] {
            write(dir.path(), &format!("{name}.wav"), "");
            write(dir.path(), &format!("{name}.txt"), name);
        }

        let samples = scan(dir.path()).expect("scan must succeed");
        let names: Vec<&str> = samples.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alfa", "medio", "zeta"]);
    }

    #[test]
    fn an_empty_directory_yields_an_empty_corpus_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir must succeed");
        let samples = scan(dir.path()).expect("scan of an empty directory must succeed");
        assert!(samples.is_empty());
    }
}
