//! What the local model's second look changes in real dictations, and how
//! long it takes — the measurement behind `eva_text::polish`:
//!
//! ```text
//! sqlite3 ~/Library/Application\ Support/EVA01/eva.sqlite3 \
//!     "select replace(raw, char(10), ' ') from transcripts where raw != ''" > /tmp/raws.txt
//! cargo run --release -p eva-text --example polish_probe -- /tmp/raws.txt [modelo]
//! ```
//!
//! Needs Ollama running with the model (`qwen2.5:3b` unless another is named).

use std::time::{Duration, Instant};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let dict = eva_text::Dictionary::new(Vec::<String>::new()).with_tech_glossary();
    let model = std::env::args().nth(2).unwrap_or_else(|| "qwen2.5:3b".to_string());
    let p = eva_text::polish::Polisher::new("http://127.0.0.1:11434/v1", &model).unwrap();
    p.warm_up();
    let vocab = eva_text::polish::vocabulary(&[]);
    let (mut n, mut changed, mut total) = (0, 0, Duration::ZERO);
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let c = eva_text::clean(line, &dict, &eva_text::RuleOnlyFormatter);
        let t = Instant::now();
        let out = p.polish(&c.formatted, &vocab, Duration::from_secs(6));
        total += t.elapsed(); n += 1;
        if out != c.formatted {
            changed += 1;
            let a: Vec<&str> = c.formatted.split_whitespace().collect();
            let b: Vec<&str> = out.split_whitespace().collect();
            let mut i = 0; while i < a.len().min(b.len()) && a[i] == b[i] { i += 1; }
            let (mut ea, mut eb) = (a.len(), b.len()); while ea > i && eb > i && a[ea-1] == b[eb-1] { ea -= 1; eb -= 1; }
            let lo = i.saturating_sub(3);
            println!("[{:.2}s] … {} [{}] {} …  →  [{}]", t.elapsed().as_secs_f64(), a[lo..i].join(" "), a[i..ea].join(" "), a[ea..(ea+3).min(a.len())].join(" "), b[i..eb].join(" "));
        }
    }
    println!("{n} dictados, {changed} cambiados por el pulido, media {:.2}s", total.as_secs_f64() / n as f64);
}
