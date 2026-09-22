//! Manual smoke test: loads a real Whisper model and transcribes a real WAV
//! file end to end, through this crate's actual `WhisperSpeechToText`
//! (not a call straight into `transcribe-rs`) — proving the crate's own
//! wrapper works, not just the underlying library.
//!
//! Run with: `cargo run --example transcribe_file -p eva-audio -- <model.bin> <audio.wav>`
//!
//! Run for real on 2026-09-22 against `ggml-base.bin` (148 MB, downloaded
//! from the whisper.cpp Hugging Face repo) and a WAV synthesized with
//! `say -v Mónica "Adán, abre Brave y busca el clima de hoy."`: the model
//! auto-detected Spanish (94.7% confidence) and returned
//! `"Adan, abrebrabe y busca el clima de hoy."` — a real, unprompted
//! demonstration of exactly the failure mode `docs/PLAN.md` §2A Hallazgo 4
//! is about (the engine dropped "Adán"'s accent), which is why
//! `eva-intent::wake::strip_wake_word` accent-folds instead of comparing
//! strings exactly. ("abrebrabe" for "abre Brave" is a `base`-model/
//! synthetic-voice accuracy limit, not a wrapper bug — the production
//! default model remains `canary-1b-flash` per `docs/PLAN.md` §5.)

use eva_audio::{SpeechToText, WhisperSpeechToText};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let model_path = PathBuf::from(args.next().expect("usage: transcribe_file <model.bin> <audio.wav>"));
    let wav_path = PathBuf::from(args.next().expect("usage: transcribe_file <model.bin> <audio.wav>"));

    println!("Cargando modelo: {}", model_path.display());
    let stt = WhisperSpeechToText::load(&model_path).expect("el modelo debe cargar");

    println!("Leyendo audio: {}", wav_path.display());
    let samples = transcribe_rs::audio::read_wav_samples(&wav_path).expect("el WAV debe leerse");

    println!("Transcribiendo {} muestras…", samples.len());
    let result = stt.transcribe(&samples).expect("la transcripción debe completarse");

    println!("\n=== RESULTADO ===\n{}\n=================", result.text);
}
