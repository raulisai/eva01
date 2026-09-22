//! The "rayita": a small, borderless panel that shows the current
//! [`OverlayState`] without ever stealing keyboard focus from whatever app
//! the user is dictating into. Backed by `NSPanel` with the
//! `NonactivatingPanel` style mask — the same technique `tauri-nspanel`
//! wraps for Tauri apps, used here directly.
//!
//! Must be constructed on the main thread — `Overlay::new` takes a
//! [`MainThreadMarker`] as proof, per objc2's convention for AppKit types
//! that are not thread-safe.

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSTextField, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// The states the overlay can show, matching `docs/PLAN.md` §3.3 point 4's
/// worker state machine (`eva_ipc::WorkerState`) plus the neutral "nothing
/// happening, hide the panel entirely" state — this crate does not depend on
/// `eva-ipc` to avoid a cross-cutting dependency for four strings, so
/// `eva-worker` maps `WorkerState` to this enum at the boundary instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    /// Hide the panel; nothing is happening.
    Idle,
    /// Actively capturing audio.
    Listening,
    /// Transcribing, formatting, or waiting on an agent.
    Thinking,
    /// An OS action or agent task is running.
    Executing,
    /// The last request finished successfully.
    Done,
    /// The last request failed.
    Failed,
}

impl OverlayState {
    fn label_text(self) -> &'static str {
        match self {
            OverlayState::Idle => "",
            OverlayState::Listening => "● Escuchando…",
            OverlayState::Thinking => "◌ Pensando…",
            OverlayState::Executing => "▶ Ejecutando…",
            OverlayState::Done => "✓ Listo",
            OverlayState::Failed => "✗ Algo falló",
        }
    }
}

/// The overlay panel itself.
pub struct Overlay {
    panel: objc2::rc::Retained<NSPanel>,
    label: objc2::rc::Retained<NSTextField>,
}

/// AppKit's floating window level (`NSFloatingWindowLevel` in
/// `NSWindow.h`) — a stable, publicly documented constant (value `3`),
/// used here as a plain integer because `objc2-app-kit` exposes
/// `NSWindow::setLevel` as taking the raw `NSInteger`, not a named enum.
const FLOATING_WINDOW_LEVEL: isize = 3;

const PANEL_WIDTH: f64 = 280.0;
const PANEL_HEIGHT: f64 = 40.0;

impl Overlay {
    /// Builds the overlay panel, initially hidden ([`OverlayState::Idle`]).
    pub fn new(mtm: MainThreadMarker) -> Self {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, PANEL_HEIGHT));

        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            content_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );

        panel.setLevel(FLOATING_WINDOW_LEVEL);
        panel.setOpaque(false);
        panel.setHasShadow(true);
        panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.1, 0.1, 0.85)));

        let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        label.setFrame(content_rect);
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setBackgroundColor(None);
        label.setAlignment(objc2_app_kit::NSTextAlignment::Center);

        panel.setContentView(Some(&label));

        let overlay = Overlay { panel, label };
        overlay.position_bottom_center(mtm);
        overlay
    }

    /// Moves the panel to bottom-center of the main screen, where Handy's
    /// overlay (and Wispr Flow's) conventionally sit.
    fn position_bottom_center(&self, mtm: MainThreadMarker) {
        let Some(screen) = objc2_app_kit::NSScreen::mainScreen(mtm) else {
            return; // headless session with no screen — nothing to position against
        };
        let screen_frame = screen.frame();
        let x = screen_frame.origin.x + (screen_frame.size.width - PANEL_WIDTH) / 2.0;
        let y = screen_frame.origin.y + 80.0; // a bit above the very bottom edge
        self.panel.setFrameOrigin(NSPoint::new(x, y));
    }

    /// Updates the panel to reflect `state`, showing or hiding it as needed.
    /// [`OverlayState::Idle`] hides the panel; every other state shows it
    /// with the matching label text.
    pub fn set_state(&self, state: OverlayState) {
        self.label.setStringValue(&NSString::from_str(state.label_text()));
        if state == OverlayState::Idle {
            self.panel.orderOut(None);
        } else {
            // `orderFrontRegardless` shows the panel without activating the
            // app or stealing focus from whatever the user is dictating
            // into — the entire point of using an `NSPanel` here.
            self.panel.orderFrontRegardless();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn label_text_is_empty_only_for_idle() {
        assert_eq!(OverlayState::Idle.label_text(), "");
        for state in [
            OverlayState::Listening,
            OverlayState::Thinking,
            OverlayState::Executing,
            OverlayState::Done,
            OverlayState::Failed,
        ] {
            assert!(!state.label_text().is_empty(), "{state:?} must have a non-empty label");
        }
    }

    // Building a real NSPanel needs an actual AppKit application context
    // (NSApplication running on the main thread) that a plain `cargo test`
    // process does not provide — MainThreadMarker::new() legitimately
    // returns None outside of one, so there is nothing meaningful to
    // construct an `Overlay` against here. This is exercised for real in
    // `bins/eva-shell`, which does run inside a real NSApplication event
    // loop on the main thread.
}
