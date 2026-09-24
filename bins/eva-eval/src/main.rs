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
///
/// Most of these are also ordinary words ("este coche", "será muy bueno"),
/// so a word only counts in a filler's position: set off by a comma, as the
/// speech model and the formatter punctuate a filler ("Bueno, vamos…").
/// "o sea" is a filler wherever it appears.
const FILLER_WATCHLIST: &[&str] = &["eh", "este", "pues", "bueno", "digo", "osea"];

/// How long a clip may be and still count toward the latency goal
/// (`docs/PLAN.md` §7: "p95 < 1200 ms, frase de 10 s").
const SHORT_CLIP_SECS: f64 = 15.0;

#[derive(Parser)]
#[command(name = "eva-eval", about = "Corre el corpus de EVA01 y reporta WER, latencia y muletillas")]
struct Cli {
    /// Carpeta con pares `nombre.wav` / `nombre.txt`.
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

    /// Qué rodea cada audio antes de la voz: `auto`, `silence` o `none` (como
    /// `[stt] padding` en la configuración). Sirve para compararlos con tu corpus.
    #[arg(long, default_value = "auto")]
    padding: String,

    /// Formatea con Apple Intelligence en vez de solo reglas, para medir el
    /// camino real de un dictado (y su latencia) en este equipo.
    #[arg(long)]
    apple_intelligence: bool,

    /// Carpeta de un segundo modelo Canary (canary-180m-flash) para volver a oír
    /// las frases cortas, como hace EVA01 cuando está instalado.
    #[arg(long)]
    second_opinion: Option<PathBuf>,

    /// La palabra de activación con la que se decide la segunda opinión.
    #[arg(long, default_value = "Adán")]
    wake_word: String,

    /// Mide como si el audio se hubiera hablado en vivo: se transcribe y formatea
    /// lo ya dicho mientras se habla (como hace EVA01) y solo cuenta la espera
    /// de después de soltar la tecla: la última cola de audio y la última frase.
    #[arg(long)]
    streaming: bool,

    /// Muestra también lo que devolvió el modelo de voz antes de formatear,
    /// para saber si un error es del oído o del formato.
    #[arg(long)]
    raw: bool,
}

struct SampleResult {
    name: String,
    wer: wer::WerResult,
    audio_secs: f64,
    stt_latency: Duration,
    format_latency: Duration,
    reference: String,
    transcript: String,
    hypothesis: String,
    has_surviving_filler: bool,
}

fn main() {
    let cli = Cli::parse();

    let Some(padding) = eva_audio::transcribe::Padding::from_name(&cli.padding) else {
        eprintln!("--padding {}: usa auto, silence o none", cli.padding);
        std::process::exit(2);
    };
    let stt = match load_stt(padding) {
        Ok(stt) => stt,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let second: Option<std::sync::Arc<dyn SpeechToText>> = match &cli.second_opinion {
        Some(dir) => match eva_audio::CanarySpeechToText::load(dir, "es") {
            Ok(model) => Some(std::sync::Arc::new(model.with_padding(padding))),
            Err(e) => {
                eprintln!("no se pudo cargar la segunda opinión en {}: {e}", dir.display());
                std::process::exit(2);
            }
        },
        None => None,
    };
    let wake_word = cli.wake_word.clone();
    let wanted = std::sync::Arc::new(move |text: &str| {
        eva_intent::looks_like_command(&eva_text::filler::remove_universal_fillers(text), &wake_word, &[])
    });
    // What EVA01 itself uses: the main model, the optional second opinion,
    // and loops collapsed.
    let stt = eva_audio::second_opinion::SecondOpinion::new(std::sync::Arc::from(stt), second, wanted);

    let samples = match corpus::scan(&cli.corpus) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("no se pudo leer el corpus en {}: {e}", cli.corpus.display());
            std::process::exit(2);
        }
    };

    if samples.is_empty() {
        println!("el corpus en {} está vacío (o solo tiene .wav sin su .txt de referencia).", cli.corpus.display());
        println!("agrega pares <nombre>.wav / <nombre>.txt — ver eval/README.md.");
        return;
    }

    let formatter: std::sync::Arc<dyn Formatter> = if cli.apple_intelligence {
        match eva_text::AppleIntelligenceFormatter::new() {
            Some(formatter) => std::sync::Arc::new(formatter),
            None => {
                eprintln!("Apple Intelligence no está disponible en este equipo; usa el eval sin --apple-intelligence");
                std::process::exit(2);
            }
        }
    } else {
        std::sync::Arc::new(RuleOnlyFormatter)
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
        .filter_map(|sample| run_one_sample(sample, &stt, &dictionary, &formatter, cli.strict, cli.streaming))
        .collect();

    print_report(&results, cli.strict, cli.apple_intelligence, cli.raw);
}

fn load_stt(padding: eva_audio::transcribe::Padding) -> Result<Box<dyn SpeechToText>, String> {
    if let Ok(dir) = std::env::var("EVA_CANARY_MODEL_DIR") {
        let language = std::env::var("EVA_STT_LANGUAGE").unwrap_or_else(|_| "es".to_string());
        return eva_audio::CanarySpeechToText::load(&PathBuf::from(&dir), language)
            .map(|stt| Box::new(stt.with_padding(padding)) as Box<dyn SpeechToText>)
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
    formatter: &std::sync::Arc<dyn Formatter>,
    strict: bool,
    streaming: bool,
) -> Option<SampleResult> {
    let samples = match transcribe_rs::audio::read_wav_samples(&sample.wav_path) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("{}: no se pudo leer el audio: {e}", sample.name);
            return None;
        }
    };

    let (transcript, stt_latency, cleaned, format_latency) = if streaming {
        match stream_like_a_live_recording(&samples, stt, dictionary, formatter) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("{}: la transcripción falló: {e}", sample.name);
                return None;
            }
        }
    } else {
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
        let cleaned = eva_text::clean(&transcript.text, dictionary, formatter.as_ref());
        (transcript, stt_latency, cleaned, start.elapsed())
    };
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
        audio_secs: samples.len() as f64 / f64::from(eva_audio::TARGET_SAMPLE_RATE),
        stt_latency,
        format_latency,
        reference: sample.reference.clone(),
        transcript: transcript.text.clone(),
        hypothesis,
        has_surviving_filler,
    })
}

/// Replays `samples` the way the worker meets a live recording: every 250 ms
/// the audio so far is looked at for a pause, each finished stretch is
/// transcribed and the text so far formatted ahead (into a remembering
/// formatter); when the "key comes up" only the tail is transcribed and the
/// whole text formatted. Returns the transcript and the two waits *after that
/// moment* — the ones the user feels.
fn stream_like_a_live_recording(
    samples: &[f32],
    stt: &dyn SpeechToText,
    dictionary: &Dictionary,
    formatter: &std::sync::Arc<dyn Formatter>,
) -> Result<(eva_audio::Transcript, Duration, eva_text::CleanedTranscript, Duration), eva_audio::TranscribeError> {
    const STEP: usize = eva_audio::TARGET_SAMPLE_RATE as usize / 4;
    let cache = eva_text::CachingFormatter::new(std::sync::Arc::clone(formatter));
    let (mut upto, mut texts) = (0, Vec::new());
    for heard in (STEP..samples.len()).step_by(STEP) {
        let Some(cut) = eva_audio::segment::cut_while_recording(&samples[upto..heard]) else { continue };
        let text = stt.transcribe(&samples[upto..upto + cut])?.text;
        upto += cut;
        if !text.trim().is_empty() {
            texts.push(text);
        }
        let _ = eva_text::clean(&texts.join(" "), dictionary, &cache);
    }

    let start = Instant::now();
    texts.push(stt.transcribe(&samples[upto..])?.text);
    let stt_latency = start.elapsed();
    let text = texts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
    let start = Instant::now();
    let cleaned = eva_text::clean(&text, dictionary, &cache);
    Ok((eva_audio::Transcript { text }, stt_latency, cleaned, start.elapsed()))
}

fn contains_filler(text: &str) -> bool {
    let folded = eva_text::fold_diacritics(text);
    let words: Vec<&str> = folded.split_whitespace().collect();
    let core = |word: &str| word.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let o_sea = words.windows(2).any(|pair| core(pair[0]) == "o" && core(pair[1]) == "sea");
    let set_off = words.iter().enumerate().any(|(i, word)| {
        let after_comma = i > 0 && words[i - 1].ends_with(',');
        let starts = i == 0 || words[i - 1].ends_with(['.', '?', '!']);
        let followed_by_comma = word.ends_with(',');
        FILLER_WATCHLIST.contains(&core(word).as_str()) && (followed_by_comma || (after_comma && starts))
    });
    o_sea || set_off
}

fn print_report(results: &[SampleResult], strict: bool, apple_intelligence: bool, raw: bool) {
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
        if raw {
            println!("  stt: {}", r.transcript);
        }
        println!("  hyp: {}", r.hypothesis);
    }

    if results.is_empty() {
        println!("(ninguna muestra se pudo procesar)");
        return;
    }
    let rates: Vec<f64> = results.iter().map(|r| r.wer.rate()).collect();
    let mean_wer = rates.iter().sum::<f64>() / rates.len() as f64;
    let perfect = rates.iter().filter(|r| **r == 0.0).count();

    let short: Vec<&SampleResult> = results.iter().filter(|r| r.audio_secs <= SHORT_CLIP_SECS).collect();
    let long: Vec<&SampleResult> = results.iter().filter(|r| r.audio_secs > SHORT_CLIP_SECS).collect();
    let stt: Vec<Duration> = short.iter().map(|r| r.stt_latency).collect();
    let total: Vec<Duration> = short.iter().map(|r| r.stt_latency + r.format_latency).collect();
    let fmt = |d: Option<Duration>| d.map(|d| format!("{}ms", d.as_millis())).unwrap_or_else(|| "-".to_string());
    let filler_count = results.iter().filter(|r| r.has_surviving_filler).count();

    println!("\n--- resumen ---");
    println!(
        "WER promedio:                  {:.1}%   ({perfect}/{} sin ningún error)",
        mean_wer * 100.0,
        results.len()
    );
    println!(
        "Voz      p50 / p95:            {} / {}   ({} frases de hasta {SHORT_CLIP_SECS:.0} s)",
        fmt(percentile::percentile(&stt, 50.0)),
        fmt(percentile::percentile(&stt, 95.0)),
        short.len()
    );
    println!(
        "Voz+formato p50 / p95:         {} / {}   (la meta es p95 < 1200ms, sin contar el pegado)",
        fmt(percentile::percentile(&total, 50.0)),
        fmt(percentile::percentile(&total, 95.0))
    );
    for r in &long {
        let busy = (r.stt_latency + r.format_latency).as_secs_f64();
        println!(
            "Dictado largo {:<16} {:.0} s de audio → {:.1} s de espera ({:.2} s por segundo dictado)",
            r.name,
            r.audio_secs,
            busy,
            busy / r.audio_secs
        );
    }
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
        assert!(contains_filler("Pues, claro que sí."));
    }

    #[test]
    fn is_accent_and_case_insensitive() {
        assert!(contains_filler("ESTE, es el punto"));
        assert!(contains_filler("Mándalo, PUES, ya."));
    }

    #[test]
    fn the_same_words_used_as_ordinary_words_are_not_fillers() {
        assert!(!contains_filler("El clima de mañana será muy bueno."));
        assert!(!contains_filler("Este es el punto."));
        assert!(!contains_filler("Pues claro que sí."));
    }

    #[test]
    fn o_sea_is_a_filler_wherever_it_is() {
        assert!(contains_filler("Mándale el archivo, o sea, ya."));
        assert!(contains_filler("o sea que no vienes"));
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
