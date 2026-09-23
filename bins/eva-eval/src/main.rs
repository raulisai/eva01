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
use eva_text::{Dictionary, Formatter, RuleOnlyFormatter};
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

    /// Calcula el WER con puntuación y mayúsculas también (por defecto se
    /// ignoran: eso lo juzga el formateador, no el modelo de voz).
    #[arg(long)]
    strict: bool,

    /// Formatea con Apple Intelligence en vez de solo reglas, para medir el
    /// camino real de un dictado (y su latencia) en este equipo.
    #[arg(long)]
    apple_intelligence: bool,
}

struct SampleResult {
    name: String,
    wer: wer::WerResult,
    stt_latency: Duration,
    format_latency: Duration,
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

    let formatter: Box<dyn Formatter> = if cli.apple_intelligence {
        match eva_text::AppleIntelligenceFormatter::new() {
            Some(formatter) => Box::new(formatter),
            None => {
                eprintln!("Apple Intelligence no está disponible en este equipo; usa el eval sin --apple-intelligence");
                std::process::exit(2);
            }
        }
    } else {
        Box::new(RuleOnlyFormatter)
    };

    // The worker warms the formatter at startup, so the numbers exclude the
    // model's one-time load — that is what a dictation actually pays.
    let warm = eva_text::warm_up(formatter.as_ref());
    if warm.as_millis() > 50 {
        println!("(calentamiento del formateador: {}ms, no cuenta en los tiempos)\n", warm.as_millis());
    }

    let dictionary = Dictionary::new(cli.custom_words);
    let results: Vec<SampleResult> = samples
        .iter()
        .filter_map(|sample| run_one_sample(sample, stt.as_ref(), &dictionary, formatter.as_ref(), cli.strict))
        .collect();

    print_report(&results, cli.strict, cli.apple_intelligence);
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

fn run_one_sample(
    sample: &corpus::Sample,
    stt: &dyn SpeechToText,
    dictionary: &Dictionary,
    formatter: &dyn Formatter,
    strict: bool,
) -> Option<SampleResult> {
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
    let stt_latency = start.elapsed();

    let start = Instant::now();
    let cleaned = eva_text::clean(&transcript.text, dictionary, formatter);
    let format_latency = start.elapsed();
    let hypothesis = cleaned.formatted;

    let wer_result = if strict {
        wer::word_error_rate(&sample.reference, &hypothesis)
    } else {
        wer::word_error_rate(&wer::normalize_for_wer(&sample.reference), &wer::normalize_for_wer(&hypothesis))
    };
    let has_surviving_filler = contains_filler(&hypothesis);

    Some(SampleResult {
        name: sample.name.clone(),
        wer: wer_result,
        stt_latency,
        format_latency,
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

fn print_report(results: &[SampleResult], strict: bool, apple_intelligence: bool) {
    println!(
        "=== EVA01 — resultados del corpus ({} muestras · formateo: {} · WER {}) ===\n",
        results.len(),
        if apple_intelligence { "Apple Intelligence" } else { "solo reglas" },
        if strict { "estricto" } else { "normalizado" },
    );

    for r in results {
        let flag = if r.has_surviving_filler { " [muletilla]" } else { "" };
        println!(
            "{:<24} WER={:>6.1}%  voz {:>5}ms  formato {:>5}ms{flag}",
            r.name,
            r.wer.rate() * 100.0,
            r.stt_latency.as_millis(),
            r.format_latency.as_millis(),
        );
        println!("  ref: {}", r.reference);
        println!("  hyp: {}", r.hypothesis);
    }

    if results.is_empty() {
        println!("(ninguna muestra se pudo procesar)");
        return;
    }
    let rates: Vec<f64> = results.iter().map(|r| r.wer.rate()).collect();
    let mean_wer = rates.iter().sum::<f64>() / rates.len() as f64;
    let perfect = rates.iter().filter(|r| **r == 0.0).count();

    let stt: Vec<Duration> = results.iter().map(|r| r.stt_latency).collect();
    let total: Vec<Duration> = results.iter().map(|r| r.stt_latency + r.format_latency).collect();
    let fmt = |d: Option<Duration>| d.map(|d| format!("{}ms", d.as_millis())).unwrap_or_else(|| "-".to_string());
    let filler_count = results.iter().filter(|r| r.has_surviving_filler).count();

    println!("\n--- resumen ---");
    println!("WER promedio:                  {:.1}%   ({perfect}/{} sin ningún error)", mean_wer * 100.0, results.len());
    println!(
        "Voz      p50 / p95:            {} / {}",
        fmt(percentile::percentile(&stt, 50.0)),
        fmt(percentile::percentile(&stt, 95.0))
    );
    println!(
        "Voz+formato p50 / p95:         {} / {}   (la meta es p95 < 1200ms, sin contar el pegado)",
        fmt(percentile::percentile(&total, 50.0)),
        fmt(percentile::percentile(&total, 95.0))
    );
    println!("Muletillas que sobrevivieron:  {filler_count}/{}", results.len());
    println!("\n(WER es diagnóstico, no bloquea release — docs/PLAN.md §7. Audio sintético, si viene de `say`: mide el modelo, no tu voz.)");
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
