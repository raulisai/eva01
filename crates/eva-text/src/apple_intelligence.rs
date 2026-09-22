//! The real context-aware [`Formatter`]: a thin, safe wrapper around the
//! FFI bridge in `swift/eva_formatter.swift` (docs/PLAN.md §3 point 4),
//! which calls Apple Intelligence's on-device Foundation Models. This is
//! the only place in the workspace that crosses into non-Rust code besides
//! `eva-macos`'s AppKit bindings and `transcribe-rs`'s ONNX runtime — per
//! `docs/PLAN.md` §3.3 point 3, that boundary is treated as a trust
//! boundary, not an ordinary function call: every call carries an explicit
//! timeout enforced on the Swift side (a `DispatchSemaphore` wait with a
//! deadline, since a `@_cdecl` function cannot itself be `async`), and a
//! failure of any kind — unavailable, errored, empty output, timed out —
//! collapses to a single `Err` so `eva_text::clean` degrades to
//! [`RuleOnlyFormatter`](crate::RuleOnlyFormatter) exactly as it would for
//! any other formatter failure. This struct is never "the only way text
//! gets formatted."

use crate::formatter::{FormatError, Formatter, RuleOnlyFormatter};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double};
use std::time::Duration;

extern "C" {
    fn eva_formatter_is_available() -> bool;
    fn eva_formatter_format(text: *const c_char, timeout_seconds: c_double) -> *mut c_char;
    fn eva_formatter_free_string(ptr: *mut c_char);
}

/// How long one [`AppleIntelligenceFormatter::format`] call waits before
/// giving up and letting the caller fall back to rules. A dictated
/// utterance is at most a few sentences — six seconds is generous headroom
/// over the ~1s this took in real, on-device testing (Apple Silicon,
/// macOS 26) while still being short enough that a stalled call cannot
/// noticeably delay pasting.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);

/// Formats text using Apple Intelligence's on-device Foundation Models,
/// on macOS 26+ with Apple Intelligence enabled. Construct with [`new`]
/// (or [`with_timeout`]), which returns `None` when the model is not
/// available right now, so `eva-worker` never holds onto a formatter
/// destined to fail every call.
///
/// [`new`]: AppleIntelligenceFormatter::new
/// [`with_timeout`]: AppleIntelligenceFormatter::with_timeout
#[derive(Debug, Clone, Copy)]
pub struct AppleIntelligenceFormatter {
    timeout: Duration,
}

impl AppleIntelligenceFormatter {
    /// Builds a formatter with [`DEFAULT_TIMEOUT`], or `None` if Foundation
    /// Models reports itself unavailable on this device right now (wrong
    /// hardware, Apple Intelligence disabled, unsupported region/language,
    /// or the model assets are still downloading).
    pub fn new() -> Option<Self> {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    /// Same as [`new`](Self::new), with a caller-chosen timeout — mainly
    /// for tests that want a short deadline instead of waiting out
    /// [`DEFAULT_TIMEOUT`] on a genuinely slow response.
    pub fn with_timeout(timeout: Duration) -> Option<Self> {
        // SAFETY: `eva_formatter_is_available` takes no arguments, has no
        // preconditions, and only reads system state — safe to call at any
        // time from any thread.
        if unsafe { eva_formatter_is_available() } {
            Some(Self { timeout })
        } else {
            None
        }
    }
}

impl Formatter for AppleIntelligenceFormatter {
    fn format(&self, text: &str) -> Result<String, FormatError> {
        if text.trim().is_empty() {
            return Ok(String::new());
        }

        // An embedded NUL byte can't come from real speech-to-text output,
        // but `CString::new` is fallible on principle — handled, not
        // assumed away.
        let c_text = CString::new(text)
            .map_err(|_| FormatError::InvalidOutput("el texto contiene un byte nulo interno".to_string()))?;

        // SAFETY: `c_text` is kept alive until after this call returns, so
        // the pointer handed to Swift stays valid for its whole duration;
        // the Swift side never retains the pointer past the call itself.
        let out_ptr = unsafe { eva_formatter_format(c_text.as_ptr(), self.timeout.as_secs_f64()) };

        if out_ptr.is_null() {
            // The bridge collapses "unavailable", "model error", "empty
            // response", and "deadline exceeded" into one NULL — cheaply
            // re-checking availability here at least distinguishes "it
            // became unavailable mid-session" from "it simply didn't
            // answer in time", which is worth the one extra FFI call for
            // anything logging this error.
            return Err(if unsafe { eva_formatter_is_available() } {
                FormatError::Timeout(self.timeout)
            } else {
                FormatError::Unavailable("Apple Intelligence dejó de estar disponible".to_string())
            });
        }

        // SAFETY: `out_ptr` is non-null, was allocated by Swift's `strdup`
        // (i.e. libc `malloc`) on a valid Swift `String`'s UTF-8 bytes plus
        // a NUL terminator, and is freed exactly once, right after this
        // copy, via the matching `eva_formatter_free_string` (libc `free`)
        // — never Rust's allocator, which would be undefined behavior for
        // a pointer `malloc` produced.
        let result = unsafe { CStr::from_ptr(out_ptr) }.to_string_lossy().into_owned();
        unsafe { eva_formatter_free_string(out_ptr) };

        if result.trim().is_empty() {
            return Err(FormatError::InvalidOutput("Apple Intelligence devolvió una respuesta vacía".to_string()));
        }

        if !is_plausible_correction(text, &result) {
            return Err(FormatError::InvalidOutput(format!(
                "la respuesta ({} palabras) tiene más palabras que el dictado ({} palabras) — \
                 probablemente el modelo respondió o completó el texto en vez de solo corregirlo: {result:?}",
                result.split_whitespace().count(),
                text.split_whitespace().count(),
            )));
        }

        // The model is inconsistent about capitalizing the real first
        // letter right after an opening ¿/¡ it just added, even when its
        // own few-shot examples show the correct form — found by hand
        // testing ("cuando vas a llegar a la oficina" came back as
        // "¿cuándo vas a llegar..." with a lowercase "c"). Capitalization
        // and terminal punctuation are purely mechanical, so running the
        // result through `RuleOnlyFormatter` guarantees that invariant
        // regardless of the model's own consistency, at zero risk: it only
        // ever touches the first alphabetic character and a trailing
        // punctuation mark, never word choice or count.
        #[allow(clippy::expect_used)] // RuleOnlyFormatter::format never returns Err
        Ok(RuleOnlyFormatter.format(&result).expect("RuleOnlyFormatter never fails"))
    }
}

/// A real, on-device failure mode found by hand-testing this bridge (see the
/// long comment on `instructions` in `swift/eva_formatter.swift`): on some
/// inputs that read as a question or an expression of uncertainty, the
/// on-device model answers or continues the sentence instead of correcting
/// it — Foundation Models' `respond(to:)` fundamentally frames every call as
/// a chat turn, and no amount of prompt wording has been found to fully
/// suppress that for every input. Every correction this formatter is
/// actually asked to make — capitalizing, adding punctuation, spelling
/// numbers as digits, dropping a filler word — can only keep the same word
/// count or reduce it, never add words. So a response with strictly more
/// words than the input is never a legitimate correction, and is rejected
/// here rather than trusted, as a second, independently-testable layer of
/// defense behind the prompt itself.
fn is_plausible_correction(input: &str, output: &str) -> bool {
    output.split_whitespace().count() <= input.split_whitespace().count()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn checking_availability_never_panics() {
        // Whichever way this machine's Apple Intelligence is configured,
        // constructing (or declining to construct) a formatter must never
        // crash — this is the one assertion that holds on every machine,
        // real integration behavior is covered by the `#[ignore]`d test
        // below.
        let _ = AppleIntelligenceFormatter::new();
    }

    #[test]
    fn empty_input_is_not_sent_to_the_model_and_returns_empty() {
        // Only meaningful when a formatter is actually available, but safe
        // either way: `format` short-circuits on blank input before ever
        // touching the FFI boundary.
        if let Some(formatter) = AppleIntelligenceFormatter::new() {
            assert_eq!(formatter.format("   ").expect("blank input never fails"), "");
        }
    }

    #[test]
    fn a_correction_that_only_drops_a_filler_word_is_plausible() {
        assert!(is_plausible_correction("o sea mándale el archivo", "Mándale el archivo."));
    }

    #[test]
    fn a_same_length_correction_is_plausible() {
        assert!(is_plausible_correction("hola como estas", "Hola, ¿cómo estás?"));
    }

    #[test]
    fn a_response_with_more_words_than_the_input_is_rejected() {
        // The real failure this guards against, found by hand-testing the
        // bridge: asked to correct "necesito tres archivos y dos carpetas
        // para mañana" (7 words), the on-device model sometimes answers
        // with an invented, multi-line list of fake file names instead —
        // strictly more words, and never a legitimate correction.
        let input = "necesito tres archivos y dos carpetas para mañana"; // 8 words
        let hallucinated = "Archivos: informe de la reunión, lista de compras, resumen semanal. Carpetas: documentos y tareas del mes"; // 16 words
        assert!(!is_plausible_correction(input, hallucinated));
    }

    #[test]
    fn equal_word_count_is_the_boundary_and_is_still_plausible() {
        assert!(is_plausible_correction("una dos tres", "Uno, dos, tres."));
    }

    /// Exercises the real, on-device Foundation Models call — network-free
    /// but genuinely slow (real inference) and only meaningful on a Mac
    /// with Apple Intelligence actually enabled. Run manually with
    /// `cargo test -p eva-text -- --ignored`.
    #[test]
    #[ignore = "calls the real on-device Apple Intelligence model; run manually with --ignored"]
    fn real_model_removes_an_ambiguous_filler_and_punctuates() {
        let formatter = AppleIntelligenceFormatter::new().expect("Apple Intelligence must be enabled for this test");
        let out = formatter
            .format("o sea este mandale el archivo a juan pero antes pregúntale si ya llegó a la oficina")
            .expect("a real, available model must respond within the timeout");
        assert!(out.ends_with('.') || out.ends_with('?') || out.ends_with('!'), "got: {out:?}");
        assert!(!out.to_lowercase().contains("o sea"), "the ambiguous filler should be gone: {out:?}");
    }

    #[test]
    #[ignore = "calls the real on-device Apple Intelligence model; run manually with --ignored"]
    fn real_model_does_not_fulfill_a_dictated_request_as_if_it_were_one() {
        // The other real failure found by hand-testing: asked to correct
        // "manda un correo a soporte diciendo que el servidor está caído",
        // a naively prompted model wrote an entire fake email instead of
        // just cleaning up the dictated sentence.
        let formatter = AppleIntelligenceFormatter::new().expect("Apple Intelligence must be enabled for this test");
        let out = formatter
            .format("manda un correo a soporte diciendo que el servidor está caído")
            .expect("a real, available model must respond within the timeout");
        assert!(out.to_lowercase().contains("correo"), "should still be the one dictated sentence: {out:?}");
        assert!(out.split_whitespace().count() <= 12, "should not have grown into a full email: {out:?}");
    }

    #[test]
    #[ignore = "calls the real on-device Apple Intelligence model; run manually with --ignored"]
    fn an_unreasonably_short_timeout_degrades_instead_of_hanging() {
        let Some(formatter) = AppleIntelligenceFormatter::with_timeout(Duration::from_nanos(1)) else {
            return; // nothing to test on a machine without Apple Intelligence
        };
        let result = formatter.format("dictame cualquier frase para probar el timeout");
        assert!(matches!(result, Err(FormatError::Timeout(_))), "got: {result:?}");
    }
}
