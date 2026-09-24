//! `eva model`: installing, listing and really testing the speech model.
//! Deliberately not a download manager (`docs/PLAN.md` §8): it fetches the
//! pinned files of one of the models in `eva_config::models::MODELS` with the
//! system's own `curl` (progress bar, resume and redirects included) and
//! checks each file's exact size. What matters after a download is whether
//! the model *works*, so [`verify`] loads it and transcribes a sentence spoken
//! by macOS itself.

use eva_config::models::{self, ModelFile, ModelSpec};
use eva_config::{support_dir, Config};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The sentence [`verify`] has macOS speak. Everyday Spanish with accents and
/// a number, since those are what a model that "works" in English but not
/// here gets wrong.
const VERIFY_SENTENCE: &str = "Hola, quiero abrir el navegador y buscar el clima de mañana en Ciudad de México";

/// `eva model list`.
pub fn list() -> i32 {
    let support = support_dir();
    println!("Modelos de voz (se instalan en {}/models):\n", support.display());
    for spec in models::MODELS {
        let installed = spec.is_installed(&models::model_dir(&support, spec.id));
        println!("  {} {:<18} {}", if installed { "✓" } else { "·" }, spec.id, spec.description);
    }
    println!("\nInstala uno con: eva model install <nombre>");
    0
}

/// `eva model install <id>`.
pub fn install(id: &str, force: bool) -> i32 {
    let Some(spec) = models::find(id) else {
        eprintln!(
            "no conozco el modelo «{id}». Los que hay: {}",
            models::MODELS.iter().map(|m| m.id).collect::<Vec<_>>().join(", ")
        );
        return 2;
    };
    let dir = models::model_dir(&support_dir(), spec.id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("no se pudo crear {}: {e}", dir.display());
        return 2;
    }

    let to_fetch: Vec<&ModelFile> = if force { spec.files.iter().collect() } else { spec.missing_files(&dir) };
    if to_fetch.is_empty() {
        println!("{} ya está instalado en {}.", spec.id, dir.display());
    } else {
        let bytes: u64 = to_fetch.iter().map(|f| f.bytes).sum();
        println!("Descargando {} ({}) en {} …", spec.id, human_size(bytes), dir.display());
        for file in to_fetch {
            println!("\n{} ({})", file.name, human_size(file.bytes));
            if let Err(e) = download(file, &dir) {
                eprintln!("\nno se pudo descargar {}: {e}\nVuelve a correr el mismo comando: la descarga se reanuda donde quedó.", file.name);
                return 1;
            }
        }
    }

    if !spec.is_installed(&dir) {
        eprintln!("faltan archivos o tienen un tamaño inesperado; vuelve a correr `eva model install {id} --force`");
        return 1;
    }
    println!("\nProbando que el modelo funcione de verdad…");
    verify_dir(&dir)
}

/// `eva model verify [id]`: uses the model EVA01 would use, or the named one.
pub fn verify(id: Option<&str>) -> i32 {
    let support = support_dir();
    let dir = match id {
        Some(id) => match models::find(id) {
            Some(spec) => models::model_dir(&support, spec.id),
            None => {
                eprintln!("no conozco el modelo «{id}»");
                return 2;
            }
        },
        None => match models::discover(&Config::load().config, &support, &|key| std::env::var(key).ok()) {
            models::ModelChoice::Canary(dir) => dir,
            models::ModelChoice::Whisper(_) => {
                eprintln!("`eva model verify` prueba modelos Canary; el que se usaría es un Whisper.");
                return 2;
            }
            models::ModelChoice::None => {
                eprintln!("no hay ningún modelo instalado. Instala uno con: eva model install canary-1b-flash");
                return 1;
            }
        },
    };
    verify_dir(&dir)
}

/// Loads the model in `dir`, has macOS speak [`VERIFY_SENTENCE`], transcribes
/// it, and reports what came back and how long it took.
fn verify_dir(dir: &Path) -> i32 {
    let load_start = Instant::now();
    let stt = match eva_audio::CanarySpeechToText::load(dir, "es") {
        Ok(stt) => stt,
        Err(e) => {
            eprintln!("✗ el modelo no carga: {e}");
            return 1;
        }
    };
    println!("  carga: {:.1} s", load_start.elapsed().as_secs_f32());

    let wav = std::env::temp_dir().join(format!("eva-verify-{}.wav", std::process::id()));
    if let Err(e) = speak_to_wav(VERIFY_SENTENCE, &wav) {
        eprintln!("⚠ el modelo carga, pero no pude generar audio de prueba con `say` ({e}); pruébalo dictando.");
        return 0;
    }
    let samples = match transcribe_rs::audio::read_wav_samples(&wav) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("⚠ el modelo carga, pero no pude leer el audio de prueba: {e}");
            let _ = std::fs::remove_file(&wav);
            return 0;
        }
    };
    let _ = std::fs::remove_file(&wav);

    use eva_audio::SpeechToText;
    let start = Instant::now();
    let transcript = match stt.transcribe(&samples) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("✗ el modelo cargó pero no pudo transcribir: {e}");
            return 1;
        }
    };
    let elapsed = start.elapsed();
    let overlap = word_overlap(VERIFY_SENTENCE, &transcript.text);

    println!("  dicho:        {VERIFY_SENTENCE}");
    println!("  transcrito:   {}", transcript.text);
    println!("  transcripción: {:.0} ms para {:.1} s de audio", elapsed.as_millis(), samples.len() as f32 / 16_000.0);
    if overlap >= 0.7 {
        println!("✓ el modelo funciona ({:.0}% de las palabras coinciden).", overlap * 100.0);
        0
    } else {
        eprintln!(
            "✗ el modelo cargó pero la transcripción no se parece a lo dicho ({:.0}% de coincidencia).",
            overlap * 100.0
        );
        1
    }
}

/// Has macOS speak `text` into a 16 kHz mono 16-bit WAV, in a Spanish voice
/// if one is installed.
fn speak_to_wav(text: &str, out: &Path) -> Result<(), String> {
    for voice in ["Mónica", "Paulina", "Jorge"] {
        let status = std::process::Command::new("say")
            .args(["-v", voice, "-o"])
            .arg(out)
            .args(["--file-format=WAVE", "--data-format=LEI16@16000", "--"])
            .arg(text)
            .stderr(std::process::Stdio::null())
            .status();
        if status.is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }
    Err("no hay una voz en español instalada (Ajustes → Accesibilidad → Contenido hablado)".to_string())
}

/// Downloads `file` into `dir` with `curl`, resuming a partial `.part` file,
/// and moves it into place only once its size is exactly what the manifest
/// says — so an interrupted or corrupt download is never mistaken for a
/// finished one.
fn download(file: &ModelFile, dir: &Path) -> Result<(), String> {
    let part = part_path(dir, file);
    if std::fs::metadata(&part).is_ok_and(|m| m.len() > file.bytes) {
        let _ = std::fs::remove_file(&part); // longer than the real file: not a partial, garbage
    }

    let status = std::process::Command::new("curl")
        .args(["--location", "--fail", "--show-error", "--progress-bar", "--continue-at", "-", "--output"])
        .arg(&part)
        .arg(file.url)
        .status()
        .map_err(|e| format!("no se pudo ejecutar curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl terminó con {status}"));
    }

    let size = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
    if size != file.bytes {
        let _ = std::fs::remove_file(&part);
        return Err(format!("llegaron {size} bytes en vez de {}", file.bytes));
    }
    let sum = sha256_of(&part)?;
    if sum != file.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "el archivo llegó dañado o no es el esperado (SHA-256 {sum}, se esperaba {})",
            file.sha256
        ));
    }
    std::fs::rename(&part, dir.join(file.name)).map_err(|e| e.to_string())
}

/// The SHA-256 of `path`, lowercase hex, computed by macOS's own `shasum`
/// (the same "use the system's tool" choice as `curl` for the download).
fn sha256_of(path: &Path) -> Result<String, String> {
    let output = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|e| format!("no se pudo ejecutar shasum: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    match text.split_whitespace().next() {
        Some(sum) if output.status.success() && sum.len() == 64 => Ok(sum.to_lowercase()),
        _ => Err(format!("shasum no pudo leer {}", path.display())),
    }
}

fn part_path(dir: &Path, file: &ModelFile) -> PathBuf {
    dir.join(format!("{}.part", file.name))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod checksum_tests {
    use super::*;

    #[test]
    fn the_checksum_is_the_files_real_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hola.txt");
        std::fs::write(&path, "hola").unwrap();
        // echo -n hola | shasum -a 256
        assert_eq!(sha256_of(&path).unwrap(), "b221d9dbb083a7f33428d7c2a3c3198ae925614d70210e28716ccaa7cd4ddb79");
        assert!(sha256_of(&dir.path().join("no-existe")).is_err());
    }
}

/// The fraction of the words in `expected` that appear in `actual`,
/// ignoring case, accents and punctuation.
fn word_overlap(expected: &str, actual: &str) -> f64 {
    let words = |text: &str| -> Vec<String> {
        eva_text::fold_diacritics(text)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let expected_words = words(expected);
    if expected_words.is_empty() {
        return 1.0;
    }
    let actual_words = words(actual);
    let found = expected_words.iter().filter(|w| actual_words.contains(w)).count();
    found as f64 / expected_words.len() as f64
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    if bytes as f64 >= 1_000.0 * MB {
        format!("{:.2} GB", bytes as f64 / (1_000.0 * MB))
    } else {
        format!("{:.0} MB", (bytes as f64 / MB).max(0.1))
    }
}

/// Every model this build can install, for `doctor` to point at.
pub fn recommended() -> &'static ModelSpec {
    &models::MODELS[0]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn overlap_ignores_case_accents_and_punctuation() {
        assert!((word_overlap("Hola, mañana", "hola manana") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn overlap_is_the_fraction_of_expected_words_found() {
        assert!((word_overlap("uno dos tres cuatro", "uno tres") - 0.5).abs() < f64::EPSILON);
        assert!(word_overlap("uno dos", "nada que ver").abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_expectation_is_trivially_met() {
        assert!((word_overlap("", "cualquier cosa") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(53_555), "0 MB");
        assert_eq!(human_size(133_710_896), "134 MB");
        assert_eq!(human_size(939_000_000), "939 MB");
        assert_eq!(human_size(1_250_000_000), "1.25 GB");
    }

    #[test]
    fn a_partial_download_lives_beside_the_final_file_with_a_part_suffix() {
        let file = &models::MODELS[0].files[0];
        assert_eq!(part_path(Path::new("/m"), file), PathBuf::from("/m/encoder-model.int8.onnx.part"));
    }

    #[test]
    fn an_unknown_model_is_a_usage_error() {
        assert_eq!(install("no-existe", false), 2);
    }

    #[test]
    fn the_recommended_model_is_the_production_default() {
        assert_eq!(recommended().id, "canary-1b-flash");
    }
}
