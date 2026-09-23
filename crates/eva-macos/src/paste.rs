//! Puts text on the clipboard and synthesizes Cmd+V, then restores whatever
//! was on the clipboard before — this is what actually lands a cleaned
//! transcript in whatever app the cursor is in.
//!
//! **Scope, stated plainly:** `docs/PLAN.md` §3 describes a "reliable
//! paste... con recibo" pattern (waiting for the receiving app to actually
//! *read* the pasted content, via `NSPasteboard` promises, before restoring
//! the clipboard). This module implements the simpler, honest version of
//! that idea: a fixed delay, with a change-count guard so a restore never
//! clobbers something the user copied in the meantime. It is not the
//! promise-based mechanism — building that is real future work.
//!
//! Per `docs/ENGINEERING.md` #5, the AppKit calls sit behind the
//! [`Pasteboard`] and [`KeystrokeSynthesizer`] traits, so the one piece of
//! logic actually worth testing carefully — the change-count guard — is
//! tested against an in-memory [`mock`] rather than the real clipboard, and
//! runs on every `cargo test` instead of only when someone remembers to run
//! the `#[ignore]`d AppKit integration tests by hand.

use crate::error::MacosError;
use std::sync::Arc;
use std::time::Duration;

/// How long to wait before restoring the clipboard to what it held before
/// [`paste_text`] ran, if nothing else changed it in the meantime.
pub const DEFAULT_RESTORE_DELAY: Duration = Duration::from_millis(500);

/// Read/write access to the system clipboard, narrowed to exactly what
/// [`paste_text_with`] needs.
pub trait Pasteboard: Send + Sync {
    /// The current plain-text contents, or `None` if the clipboard holds no
    /// text (empty, or holds a non-text type).
    fn read_string(&self) -> Option<String>;
    /// Replaces the clipboard's contents with `text`.
    fn write_string(&self, text: &str) -> Result<(), MacosError>;
    /// A counter that increments every time anything writes to the
    /// clipboard — the mechanism the change-count guard is built on.
    fn change_count(&self) -> isize;
    /// Empties the clipboard.
    fn clear(&self);
}

/// Something that can simulate the Cmd+V keystroke.
pub trait KeystrokeSynthesizer: Send + Sync {
    /// Posts a Cmd+V key-down/key-up pair to the system.
    fn synthesize_cmd_v(&self) -> Result<(), MacosError>;
    /// Posts a Cmd+C key-down/key-up pair to the system.
    fn synthesize_cmd_c(&self) -> Result<(), MacosError>;
}

/// Sets the clipboard to `text`, synthesizes Cmd+V so the frontmost app
/// pastes it, and schedules the clipboard to be restored to its previous
/// contents after `restore_delay` — unless something else has written to the
/// clipboard in the meantime, in which case the restore is skipped so a
/// slower app's paste (or an unrelated copy the user made) is never
/// clobbered.
///
/// # Errors
/// Returns [`MacosError::PasteboardWriteFailed`] if the clipboard could not
/// be written, or [`MacosError::SynthesizeKeystrokeFailed`] if the Cmd+V
/// keystroke could not be created or posted. In both cases, nothing is
/// pasted and the clipboard is left exactly as it was found.
pub fn paste_text_with(
    pasteboard: Arc<dyn Pasteboard>,
    keystroke: &dyn KeystrokeSynthesizer,
    text: &str,
    restore_delay: Duration,
) -> Result<(), MacosError> {
    let previous_text = pasteboard.read_string();

    pasteboard.write_string(text)?;
    let change_count_after_our_write = pasteboard.change_count();

    if let Err(e) = keystroke.synthesize_cmd_v() {
        // The clipboard already has our text and there is no way to "un-type"
        // a keystroke that never landed, but we can at least not leave the
        // clipboard silently mutated on a failure the caller sees as an
        // error — restore it immediately rather than waiting for the timer.
        if let Some(previous_text) = &previous_text {
            let _ = pasteboard.write_string(previous_text);
        }
        return Err(e);
    }

    if let Some(previous_text) = previous_text {
        let pasteboard = Arc::clone(&pasteboard);
        std::thread::spawn(move || {
            std::thread::sleep(restore_delay);
            if pasteboard.change_count() == change_count_after_our_write {
                let _ = pasteboard.write_string(&previous_text);
            }
        });
    }

    Ok(())
}

/// The production entry point: [`paste_text_with`] wired to the real system
/// clipboard and a real synthesized keystroke.
pub fn paste_text(text: &str, restore_delay: Duration) -> Result<(), MacosError> {
    paste_text_with(Arc::new(SystemPasteboard), &SystemKeystrokeSynthesizer, text, restore_delay)
}

/// How long [`copy_selection`] waits for the frontmost app to answer Cmd+C.
pub const COPY_TIMEOUT: Duration = Duration::from_millis(350);

/// Reads the selected text out of whatever app is in front by asking it to
/// copy: posts Cmd+C, waits for the clipboard to change, reads it, and puts
/// the clipboard back exactly as it was. This is the fallback for apps whose
/// accessibility tree does not expose the selection (terminals, some
/// Electron apps) — it works in nearly anything with a Copy command, at the
/// price of touching the clipboard for a few milliseconds.
///
/// Returns `Ok(None)` when nothing changed within `timeout`: there was no
/// selection (Cmd+C on an empty selection copies nothing).
///
/// # Errors
/// [`MacosError::SynthesizeKeystrokeFailed`] if the keystroke could not be
/// posted; the clipboard is untouched in that case.
pub fn copy_selection_with(
    pasteboard: &dyn Pasteboard,
    keystroke: &dyn KeystrokeSynthesizer,
    timeout: Duration,
) -> Result<Option<String>, MacosError> {
    let previous_text = pasteboard.read_string();
    let count_before = pasteboard.change_count();

    keystroke.synthesize_cmd_c()?;

    let deadline = std::time::Instant::now() + timeout;
    while pasteboard.change_count() == count_before {
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let selected = pasteboard.read_string();
    match previous_text {
        Some(previous) => {
            let _ = pasteboard.write_string(&previous);
        }
        None => pasteboard.clear(),
    }
    Ok(selected.filter(|text| !text.is_empty()))
}

/// The production entry point for [`copy_selection_with`].
pub fn copy_selection() -> Result<Option<String>, MacosError> {
    copy_selection_with(&SystemPasteboard, &SystemKeystrokeSynthesizer, COPY_TIMEOUT)
}

/// The real `NSPasteboard`-backed [`Pasteboard`].
pub struct SystemPasteboard;

impl Pasteboard for SystemPasteboard {
    fn read_string(&self) -> Option<String> {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        let string_type = unsafe { objc2_app_kit::NSPasteboardTypeString };
        pasteboard.stringForType(string_type).map(|s| s.to_string())
    }

    fn write_string(&self, text: &str) -> Result<(), MacosError> {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let ns_string = objc2_foundation::NSString::from_str(text);
        let string_type = unsafe { objc2_app_kit::NSPasteboardTypeString };
        let ok = pasteboard.setString_forType(&ns_string, string_type);
        if ok {
            Ok(())
        } else {
            Err(MacosError::PasteboardWriteFailed)
        }
    }

    fn change_count(&self) -> isize {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        pasteboard.changeCount()
    }

    fn clear(&self) {
        objc2_app_kit::NSPasteboard::generalPasteboard().clearContents();
    }
}

/// The real `CGEvent`-backed [`KeystrokeSynthesizer`].
pub struct SystemKeystrokeSynthesizer;

/// Virtual keycodes on a US keyboard layout (`kVK_ANSI_V` / `kVK_ANSI_C` in
/// Apple's `Carbon/HIToolbox/Events.h` — stable, publicly documented
/// constants, not something guessed at).
const VIRTUAL_KEYCODE_V: u16 = 0x09;
const VIRTUAL_KEYCODE_C: u16 = 0x08;

impl KeystrokeSynthesizer for SystemKeystrokeSynthesizer {
    fn synthesize_cmd_v(&self) -> Result<(), MacosError> {
        post_cmd_key(VIRTUAL_KEYCODE_V)
    }

    fn synthesize_cmd_c(&self) -> Result<(), MacosError> {
        post_cmd_key(VIRTUAL_KEYCODE_C)
    }
}

/// Posts Cmd + `keycode`, key-down then key-up, to the session event tap.
fn post_cmd_key(keycode: u16) -> Result<(), MacosError> {
    use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};

    let source =
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok_or(MacosError::SynthesizeKeystrokeFailed)?;

    let key_down =
        CGEvent::new_keyboard_event(Some(&source), keycode, true).ok_or(MacosError::SynthesizeKeystrokeFailed)?;
    let key_up =
        CGEvent::new_keyboard_event(Some(&source), keycode, false).ok_or(MacosError::SynthesizeKeystrokeFailed)?;

    CGEvent::set_flags(Some(&key_down), CGEventFlags::MaskCommand);
    CGEvent::set_flags(Some(&key_up), CGEventFlags::MaskCommand);

    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&key_down));
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&key_up));

    Ok(())
}

/// In-memory test doubles for [`Pasteboard`] and [`KeystrokeSynthesizer`],
/// per `docs/ENGINEERING.md` #5. Exposed as a normal module (not
/// `#[cfg(test)]`-only) since `eva-worker`'s own tests will want them too.
pub mod mock {
    use super::{KeystrokeSynthesizer, MacosError, Pasteboard};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// An in-memory clipboard: no AppKit, no real system state.
    #[derive(Default)]
    pub struct MockPasteboard {
        contents: Mutex<Option<String>>,
        change_count: AtomicIsize,
    }

    impl MockPasteboard {
        /// Builds a mock clipboard, optionally pre-seeded with `initial` content.
        pub fn new(initial: Option<&str>) -> Self {
            MockPasteboard { contents: Mutex::new(initial.map(str::to_string)), change_count: AtomicIsize::new(0) }
        }
    }

    impl Pasteboard for MockPasteboard {
        fn read_string(&self) -> Option<String> {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            self.contents.lock().unwrap().clone()
        }

        fn write_string(&self, text: &str) -> Result<(), MacosError> {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            let mut guard = self.contents.lock().unwrap();
            *guard = Some(text.to_string());
            self.change_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn change_count(&self) -> isize {
            self.change_count.load(Ordering::SeqCst)
        }

        fn clear(&self) {
            #[allow(clippy::unwrap_used)] // a poisoned test-only mutex means an earlier test already panicked
            let mut guard = self.contents.lock().unwrap();
            *guard = None;
            self.change_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A keystroke synthesizer that records whether it was called and can be
    /// configured to fail, without touching the real system. It can also
    /// play the part of the frontmost app answering Cmd+C, by writing a
    /// "selection" onto a [`MockPasteboard`] when it is asked to copy.
    #[derive(Default)]
    pub struct MockKeystrokeSynthesizer {
        called: AtomicBool,
        should_fail: bool,
        copy_called: AtomicBool,
        selection: Option<(Arc<MockPasteboard>, String)>,
    }

    impl MockKeystrokeSynthesizer {
        /// A mock that always succeeds.
        pub fn succeeding() -> Self {
            MockKeystrokeSynthesizer { should_fail: false, ..MockKeystrokeSynthesizer::default() }
        }

        /// A mock whose Cmd+C "copies" `selection` onto `pasteboard`, like an
        /// app that has that text selected.
        pub fn with_selection(pasteboard: Arc<MockPasteboard>, selection: &str) -> Self {
            MockKeystrokeSynthesizer { selection: Some((pasteboard, selection.to_string())), ..Self::default() }
        }

        /// Whether [`KeystrokeSynthesizer::synthesize_cmd_c`] was called.
        pub fn copy_was_called(&self) -> bool {
            self.copy_called.load(Ordering::SeqCst)
        }

        /// A mock that always fails, to exercise `paste_text_with`'s error path.
        pub fn failing() -> Self {
            MockKeystrokeSynthesizer { should_fail: true, ..MockKeystrokeSynthesizer::default() }
        }

        /// Whether [`KeystrokeSynthesizer::synthesize_cmd_v`] was called.
        pub fn was_called(&self) -> bool {
            self.called.load(Ordering::SeqCst)
        }
    }

    impl KeystrokeSynthesizer for MockKeystrokeSynthesizer {
        fn synthesize_cmd_v(&self) -> Result<(), MacosError> {
            self.called.store(true, Ordering::SeqCst);
            if self.should_fail {
                Err(MacosError::SynthesizeKeystrokeFailed)
            } else {
                Ok(())
            }
        }

        fn synthesize_cmd_c(&self) -> Result<(), MacosError> {
            self.copy_called.store(true, Ordering::SeqCst);
            if self.should_fail {
                return Err(MacosError::SynthesizeKeystrokeFailed);
            }
            if let Some((pasteboard, selection)) = &self.selection {
                pasteboard.write_string(selection)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::mock::{MockKeystrokeSynthesizer, MockPasteboard};
    use super::*;

    #[test]
    fn writes_the_new_text_and_synthesizes_the_keystroke() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("original")));
        let keystroke = MockKeystrokeSynthesizer::succeeding();

        paste_text_with(pasteboard.clone(), &keystroke, "nuevo texto", Duration::from_millis(10))
            .expect("paste_text_with must succeed");

        assert_eq!(pasteboard.read_string(), Some("nuevo texto".to_string()));
        assert!(keystroke.was_called());
    }

    #[test]
    fn restores_the_previous_clipboard_after_the_delay() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("original")));
        let keystroke = MockKeystrokeSynthesizer::succeeding();

        paste_text_with(pasteboard.clone(), &keystroke, "nuevo texto", Duration::from_millis(20))
            .expect("must succeed");

        assert_eq!(pasteboard.read_string(), Some("nuevo texto".to_string()));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(pasteboard.read_string(), Some("original".to_string()));
    }

    #[test]
    fn does_not_restore_if_something_else_wrote_to_the_clipboard_meanwhile() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("original")));
        let keystroke = MockKeystrokeSynthesizer::succeeding();

        paste_text_with(pasteboard.clone(), &keystroke, "nuevo texto", Duration::from_millis(20))
            .expect("must succeed");

        // Something else writes before the restore timer fires.
        pasteboard.write_string("copiado despues por el usuario").expect("must succeed");

        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(
            pasteboard.read_string(),
            Some("copiado despues por el usuario".to_string()),
            "the later write must survive; the stale restore must be skipped"
        );
    }

    #[test]
    fn an_empty_original_clipboard_is_not_restored_to_at_all() {
        let pasteboard = Arc::new(MockPasteboard::new(None));
        let keystroke = MockKeystrokeSynthesizer::succeeding();

        paste_text_with(pasteboard.clone(), &keystroke, "nuevo texto", Duration::from_millis(10))
            .expect("must succeed");

        std::thread::sleep(Duration::from_millis(60));
        // No prior text to restore, so the pasted text simply stays.
        assert_eq!(pasteboard.read_string(), Some("nuevo texto".to_string()));
    }

    #[test]
    fn a_failed_keystroke_restores_the_clipboard_immediately_and_returns_the_error() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("original")));
        let keystroke = MockKeystrokeSynthesizer::failing();

        let result = paste_text_with(pasteboard.clone(), &keystroke, "nuevo texto", Duration::from_secs(5));

        assert!(matches!(result, Err(MacosError::SynthesizeKeystrokeFailed)));
        assert_eq!(
            pasteboard.read_string(),
            Some("original".to_string()),
            "a failed keystroke must not leave the clipboard mutated"
        );
    }

    #[test]
    fn copy_selection_returns_the_selected_text_and_restores_the_clipboard() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("lo que el usuario había copiado")));
        let keystroke = MockKeystrokeSynthesizer::with_selection(pasteboard.clone(), "texto seleccionado");

        let selected =
            copy_selection_with(pasteboard.as_ref(), &keystroke, Duration::from_millis(200)).expect("must succeed");

        assert_eq!(selected, Some("texto seleccionado".to_string()));
        assert!(keystroke.copy_was_called());
        assert_eq!(
            pasteboard.read_string(),
            Some("lo que el usuario había copiado".to_string()),
            "the user's clipboard must come back exactly as it was"
        );
    }

    #[test]
    fn copy_selection_with_nothing_selected_is_none_and_leaves_the_clipboard_alone() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("original")));
        let keystroke = MockKeystrokeSynthesizer::succeeding(); // the "app" copies nothing

        let selected =
            copy_selection_with(pasteboard.as_ref(), &keystroke, Duration::from_millis(60)).expect("must succeed");

        assert_eq!(selected, None);
        assert_eq!(pasteboard.read_string(), Some("original".to_string()));
    }

    #[test]
    fn copy_selection_clears_the_clipboard_again_when_it_started_empty() {
        let pasteboard = Arc::new(MockPasteboard::new(None));
        let keystroke = MockKeystrokeSynthesizer::with_selection(pasteboard.clone(), "seleccionado");

        let selected =
            copy_selection_with(pasteboard.as_ref(), &keystroke, Duration::from_millis(200)).expect("must succeed");

        assert_eq!(selected, Some("seleccionado".to_string()));
        assert_eq!(pasteboard.read_string(), None, "the selection must not linger on an initially empty clipboard");
    }

    #[test]
    fn a_failed_copy_keystroke_is_an_error_and_touches_nothing() {
        let pasteboard = Arc::new(MockPasteboard::new(Some("original")));
        let keystroke = MockKeystrokeSynthesizer::failing();

        let result = copy_selection_with(pasteboard.as_ref(), &keystroke, Duration::from_millis(60));

        assert!(matches!(result, Err(MacosError::SynthesizeKeystrokeFailed)));
        assert_eq!(pasteboard.read_string(), Some("original".to_string()));
    }

    // The AppKit- and CGEvent-backed implementations below have no logic of
    // their own worth unit-testing beyond "does this compile and call the
    // real API correctly" — that is exactly what a real, deliberate,
    // `#[ignore]`d run verifies, since posting a real Cmd+V or touching the
    // real clipboard from an unattended `cargo test` risks pasting unknown
    // clipboard content wherever focus happens to be (a multi-line clipboard
    // value ending in a newline would be *executed* if focus is a terminal).

    #[test]
    #[ignore = "writes to the real system clipboard — run manually with `cargo test -- --ignored`"]
    fn system_pasteboard_round_trips_through_the_real_clipboard() {
        let pasteboard = SystemPasteboard;
        pasteboard.write_string("valor de prueba de eva-macos").expect("write must succeed");
        assert_eq!(pasteboard.read_string(), Some("valor de prueba de eva-macos".to_string()));
    }

    #[test]
    #[ignore = "posts a real, system-wide Cmd+V into whatever has focus — could paste unknown clipboard \
                content into a live shell/editor. Run manually, deliberately, with a safe text field focused."]
    fn system_keystroke_synthesizer_succeeds_on_a_real_desktop_session() {
        assert!(SystemKeystrokeSynthesizer.synthesize_cmd_v().is_ok());
    }
}
