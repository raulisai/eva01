//! Measures the on-device formatter against a corpus of dictated phrases,
//! one at a time (the model serializes concurrent sessions, so parallel
//! calls only produce timeouts): how many come back usable, how many the
//! word-count guard rejects because the model answered instead of
//! formatting, and how long each takes. This is how the prompt in
//! `swift/eva_formatter.swift` gets tuned with numbers instead of hunches.
//!
//! ```text
//! cargo run -p eva-text --example format_corpus -- eval/format_corpus.txt
//! ```

use eva_text::{AppleIntelligenceFormatter, FormatError, Formatter};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "eval/format_corpus.txt".to_string());
    let Ok(corpus) = std::fs::read_to_string(&path) else {
        eprintln!("no se pudo leer {path}");
        std::process::exit(2);
    };
    let Some(formatter) = AppleIntelligenceFormatter::new() else {
        eprintln!("Apple Intelligence no está disponible en este equipo");
        std::process::exit(2);
    };

    let (mut ok, mut rejected, mut failed) = (0, 0, 0);
    let mut millis = Vec::new();
    for line in corpus.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let start = Instant::now();
        let result = formatter.format(line);
        millis.push(start.elapsed().as_millis());
        match result {
            Ok(out) => {
                ok += 1;
                println!("  ok      {line}\n          → {out}");
            }
            Err(FormatError::InvalidOutput(why)) => {
                rejected += 1;
                println!("  RECHAZO {line}\n          → {why}");
            }
            Err(other) => {
                failed += 1;
                println!("  FALLO   {line}\n          → {other}");
            }
        }
    }

    millis.sort_unstable();
    let total = ok + rejected + failed;
    println!("\n{ok}/{total} aceptadas · {rejected} rechazadas por la guarda · {failed} con fallo del modelo");
    if let Some(median) = millis.get(millis.len() / 2) {
        println!("latencia mediana {median} ms · máxima {} ms", millis.last().copied().unwrap_or(0));
    }
}
