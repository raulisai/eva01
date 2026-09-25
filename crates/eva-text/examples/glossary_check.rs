//! Shows what the technical glossary would change in real dictations: reads one
//! text per line from stdin and prints only the lines it changes, before and
//! after. To check it against your own history:
//!
//! ```bash
//! sqlite3 -readonly "$HOME/Library/Application Support/EVA01/eva.sqlite3" \
//!   "select raw from transcripts where raw <> ''" | cargo run -q -p eva-text --example glossary_check
//! ```

use eva_text::Dictionary;
use std::io::BufRead;

fn main() {
    let dictionary = Dictionary::new(Vec::<String>::new()).with_tech_glossary();
    let (mut total, mut changed) = (0, 0);
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let after = dictionary.correct(&line, 0.9);
        if after.split_whitespace().ne(line.split_whitespace()) {
            changed += 1;
            println!("- {line}\n+ {after}\n");
        }
    }
    println!("{changed} of {total} lines changed");
}
