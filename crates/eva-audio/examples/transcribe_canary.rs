//! Manual smoke test: loads a real Canary model and transcribes a real WAV
//! file end to end through this crate's `CanarySpeechToText`, mirroring
//! `transcribe_file.rs`'s Whisper version. `docs/PLAN.md` §5 names Canary
//! (native Spanish, no translation bolt-on) as the actual production
//! default — this is the increment that finally exercises that real code
//! path instead of only Whisper's.
//!
//! Run with: `cargo run --example transcribe_canary -p eva-audio --features onnx -- <model_dir> <audio.wav> [lang]`

use eva_audio::{CanarySpeechToText, SpeechToText};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let model_dir =
        PathBuf::from(args.next().expect("usage: transcribe_canary <model_dir> <audio.wav> [lang]"));
    let wav_path = PathBuf::from(args.next().expect("usage: transcribe_canary <model_dir> <audio.wav> [lang]"));
    let lang = args.next().unwrap_or_else(|| "es".to_string());

    println!("Cargando modelo Canary desde: {} (idioma: {lang})", model_dir.display());
    let stt = CanarySpeechToText::load(&model_dir, lang).expect("el modelo Canary debe cargar");

    println!("Leyendo audio: {}", wav_path.display());
    let samples = transcribe_rs::audio::read_wav_samples(&wav_path).expect("el WAV debe leerse");

    println!("Transcribiendo {} muestras…", samples.len());
    let result = stt.transcribe(&samples).expect("la transcripción debe completarse");

    println!("\n=== RESULTADO (Canary) ===\n{}\n===========================", result.text);
}
