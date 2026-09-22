//! `eva-eval`: runs the harvested corpus (`docs/PLAN.md` fase 3) through the
//! real text pipeline and reports WER, p50/p95 latency, and how many
//! samples still show a filler word after cleaning — the diagnostic numbers
//! from `docs/PLAN.md` §7. WER is a diagnostic here, never a release gate
//! (only p95 latency and daily-rework count block a release, per that
//! section) — this tool prints it for exactly that comparative purpose:
//! "did the last model/prompt change help or hurt."
//!
//! Usage:
//! ```text
//! EVA_CANARY_MODEL_DIR=/path/to/canary-1b-flash eva-eval --corpus eval/corpus
//! EVA_STT_MODEL_PATH=/path/to/ggml-base.bin eva-eval --corpus eval/corpus
//! ```

mod corpus;
mod percentile;
mod wer;

use clap::Parser;
use eva_audio::SpeechToText;
use eva_text::{Dictionary, RuleOnlyFormatter};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Interjections/fillers worth flagging if they survive to the final,
/// cleaned output. This is the eval tool's own diagnostic list — separate
/// from `eva-text::filler`'s universal list, which only removes sounds that
/// are never real words; the ambiguous ones below ("este", "pues", "bueno")
/// are deliberately left alone by the rule-based pipeline
/// (`docs/PLAN.md` §2A) and need the context-aware formatter to go, so
/// their presence here is exactly the signal this eval is meant to catch —
/// not a bug in the pipeline being measured.
const FILLER_WATCHLIST: &[&str] = &["eh", "este", "o sea", "pues", "bueno", "digo", "osea"];

#[derive(Parser)]
#[command(name = "eva-eval", about = "Corre el corpus de EVA01 y reporta WER, latencia y muletillas")]
struct Cli {
    /// Carpeta con pares <nombre>.wav / <nombre>.txt.
    #[arg(long, default_value = "eval/corpus")]
    corpus: PathBuf,

    /// Palabras del diccionario personal a aplicar (repetible), para que el
    /// eval refleje el mismo texto que el dictado real produciría con tu
    /// propio diccionario, no una versión desnuda del pipeline.
    #[arg(long = "word")]
    custom_words: Vec<String>,
}

struct SampleResult {
    name: String,
    wer: wer::WerResult,
    latency: Duration,
    reference: String,
    hypothesis: String,
    has_surviving_filler: bool,
}

fn main() {
    let cli = Cli::parse();

    let stt = match load_stt() {
        Ok(stt) => stt,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let samples = match corpus::scan(&cli.corpus) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("no se pudo leer el corpus en {}: {e}", cli.corpus.display());
            std::process::exit(2);
        }
    };

    if samples.is_empty() {
        println!(
            "el corpus en {} está vacío (o solo tiene .wav sin su .txt de referencia).",
            cli.corpus.display()
        );
        println!("agrega pares <nombre>.wav / <nombre>.txt — ver eval/README.md.");
        return;
    }

    let dictionary = Dictionary::new(cli.custom_words);
    let results: Vec<SampleResult> = samples
        .iter()
        .filter_map(|sample| run_one_sample(sample, stt.as_ref(), &dictionary))
        .collect();

    print_report(&results);
}

fn load_stt() -> Result<Box<dyn SpeechToText>, String> {
    if let Ok(dir) = std::env::var("EVA_CANARY_MODEL_DIR") {
        let language = std::env::var("EVA_STT_LANGUAGE").unwrap_or_else(|_| "es".to_string());
        return eva_audio::CanarySpeechToText::load(&PathBuf::from(&dir), language)
            .map(|stt| Box::new(stt) as Box<dyn SpeechToText>)
            .map_err(|e| format!("no se pudo cargar el modelo Canary en {dir}: {e}"));
    }
    if let Ok(path) = std::env::var("EVA_STT_MODEL_PATH") {
        return eva_audio::WhisperSpeechToText::load(&PathBuf::from(&path))
            .map(|stt| Box::new(stt) as Box<dyn SpeechToText>)
            .map_err(|e| format!("no se pudo cargar el modelo Whisper en {path}: {e}"));
    }
    Err("configura EVA_CANARY_MODEL_DIR o EVA_STT_MODEL_PATH antes de correr eva-eval".to_string())
}

fn run_one_sample(sample: &corpus::Sample, stt: &dyn SpeechToText, dictionary: &Dictionary) -> Option<SampleResult> {
    let samples = match transcribe_rs::audio::read_wav_samples(&sample.wav_path) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("{}: no se pudo leer el audio: {e}", sample.name);
            return None;
        }
    };

    let start = Instant::now();
    let transcript = match stt.transcribe(&samples) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}: la transcripción falló: {e}", sample.name);
            return None;
        }
    };
    let latency = start.elapsed();

    let cleaned = eva_text::clean(&transcript.text, dictionary, &RuleOnlyFormatter);
    let hypothesis = cleaned.formatted;
    let wer_result = wer::word_error_rate(&sample.reference, &hypothesis);
    let has_surviving_filler = contains_filler(&hypothesis);

    Some(SampleResult {
        name: sample.name.clone(),
        wer: wer_result,
        latency,
        reference: sample.reference.clone(),
        hypothesis,
        has_surviving_filler,
    })
}

fn contains_filler(text: &str) -> bool {
    let folded = eva_text::fold_diacritics(text);
    FILLER_WATCHLIST.iter().any(|filler| {
        folded.split_whitespace().any(|word| word.trim_matches(|c: char| !c.is_alphanumeric()) == *filler)
    })
}

fn print_report(results: &[SampleResult]) {
    println!("=== EVA01 — resultados del corpus ({} muestras) ===\n", results.len());

    for r in results {
        let flag = if r.has_surviving_filler { " [muletilla]" } else { "" };
        println!(
            "{:<20} WER={:>6.1}%  {:>6}ms{flag}",
            r.name,
            r.wer.rate() * 100.0,
            r.latency.as_millis()
        );
        println!("  ref: {}", r.reference);
        println!("  hyp: {}", r.hypothesis);
    }

    let rates: Vec<f64> = results.iter().map(|r| r.wer.rate()).collect();
    let mean_wer = rates.iter().sum::<f64>() / rates.len() as f64;

    let latencies: Vec<Duration> = results.iter().map(|r| r.latency).collect();
    let p50 = percentile::percentile(&latencies, 50.0);
    let p95 = percentile::percentile(&latencies, 95.0);

    let filler_count = results.iter().filter(|r| r.has_surviving_filler).count();

    println!("\n--- resumen ---");
    println!("WER promedio:                  {:.1}%", mean_wer * 100.0);
    println!(
        "Latencia p50 / p95:            {} / {}",
        p50.map(|d| format!("{}ms", d.as_millis())).unwrap_or_else(|| "-".to_string()),
        p95.map(|d| format!("{}ms", d.as_millis())).unwrap_or_else(|| "-".to_string()),
    );
    println!("Muletillas que sobrevivieron:  {filler_count}/{}", results.len());
    println!(
        "\n(WER es diagnóstico, no bloquea release — docs/PLAN.md §7. La meta de p95 es < 1200ms.)"
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn detects_a_watchlisted_filler_as_a_whole_word() {
        assert!(contains_filler("Bueno, vamos a hacerlo."));
        assert!(contains_filler("pues claro que sí"));
    }

    #[test]
    fn is_accent_and_case_insensitive() {
        assert!(contains_filler("ESTE es el punto"));
    }

    #[test]
    fn does_not_flag_a_word_that_merely_contains_a_filler_as_a_substring() {
        // "bueno" must not match inside "buenísimo"/"buenas" etc.
        assert!(!contains_filler("Fue una idea buenísima."));
    }

    #[test]
    fn clean_text_with_no_filler_is_not_flagged() {
        assert!(!contains_filler("Hola, mándale el archivo a Juan."));
    }

    proptest::proptest! {
        #[test]
        fn contains_filler_never_panics(text in ".*") {
            let _ = contains_filler(&text);
        }
    }
}
