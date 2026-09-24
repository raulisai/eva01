//! Remembers what the formatter answered for a piece of text, so a long
//! dictation can be formatted *while it is being spoken*: each finished
//! sentence is formatted in the background as soon as it has been
//! transcribed, and when the key comes up only what was said last is left to
//! format. Formatting is deterministic (greedy decoding), so an answer
//! remembered is the answer that would be given again.

use crate::formatter::{FormatError, Formatter};
use crate::style::Style;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// How many answers are kept: enough for the longest dictation, few enough
/// that what was dictated does not linger in memory.
const CAPACITY: usize = 128;

/// A [`Formatter`] that remembers successful answers of the one it wraps.
/// Failures are never remembered: a piece that failed is tried again.
pub struct CachingFormatter {
    inner: Arc<dyn Formatter>,
    answers: Mutex<Answers>,
}

#[derive(Default)]
struct Answers {
    by_input: HashMap<(String, Style), String>,
    /// Oldest first, to forget the oldest when full.
    order: VecDeque<(String, Style)>,
}

impl CachingFormatter {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn Formatter>) -> CachingFormatter {
        CachingFormatter { inner, answers: Mutex::new(Answers::default()) }
    }

    fn remembered(&self, key: &(String, Style)) -> Option<String> {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        self.answers.lock().unwrap().by_input.get(key).cloned()
    }

    fn remember(&self, key: (String, Style), answer: &str) {
        #[allow(clippy::unwrap_used)] // as above
        let mut answers = self.answers.lock().unwrap();
        if answers.by_input.insert(key.clone(), answer.to_string()).is_none() {
            answers.order.push_back(key);
            if answers.order.len() > CAPACITY {
                if let Some(oldest) = answers.order.pop_front() {
                    answers.by_input.remove(&oldest);
                }
            }
        }
    }
}

impl Formatter for CachingFormatter {
    fn format(&self, text: &str) -> Result<String, FormatError> {
        self.format_styled(text, Style::Default)
    }

    fn format_styled(&self, text: &str, style: Style) -> Result<String, FormatError> {
        let key = (text.to_string(), style);
        if let Some(answer) = self.remembered(&key) {
            return Ok(answer);
        }
        let answer = self.inner.format_styled(text, style)?;
        self.remember(key, &answer);
        Ok(answer)
    }

    fn rewrite(&self, text: &str, instruction: &str) -> Result<String, FormatError> {
        self.inner.rewrite(text, instruction)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Upper-cases, counting how often it was really asked, failing on "roto".
    struct Counting(Arc<AtomicUsize>);
    impl Formatter for Counting {
        fn format(&self, text: &str) -> Result<String, FormatError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if text == "roto" {
                return Err(FormatError::Unavailable("prueba".into()));
            }
            Ok(text.to_uppercase())
        }
    }

    fn cached() -> (CachingFormatter, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        (CachingFormatter::new(Arc::new(Counting(Arc::clone(&asked)))), asked)
    }

    #[test]
    fn the_same_text_is_formatted_once() {
        let (formatter, asked) = cached();
        assert_eq!(formatter.format("hola").unwrap(), "HOLA");
        assert_eq!(formatter.format("hola").unwrap(), "HOLA");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_different_style_is_a_different_question() {
        let (formatter, asked) = cached();
        formatter.format_styled("hola", Style::Casual).unwrap();
        formatter.format_styled("hola", Style::Formal).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failure_is_not_remembered_so_it_is_tried_again() {
        let (formatter, asked) = cached();
        assert!(formatter.format("roto").is_err());
        assert!(formatter.format("roto").is_err());
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_oldest_answers_are_forgotten_when_full() {
        let (formatter, asked) = cached();
        for i in 0..=CAPACITY {
            formatter.format(&format!("texto {i}")).unwrap();
        }
        let before = asked.load(Ordering::SeqCst);
        formatter.format("texto 0").unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), before + 1, "the first one was forgotten");
        formatter.format(&format!("texto {CAPACITY}")).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), before + 1, "the newest one was kept");
    }
}
