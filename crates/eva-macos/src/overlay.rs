//! The "rayita": a small, borderless panel that shows what EVA is doing
//! without ever stealing keyboard focus from whatever app the user is
//! dictating into. Backed by `NSPanel` with the `NonactivatingPanel` style
//! mask — the same technique `tauri-nspanel` wraps for Tauri apps, used here
//! directly.
//!
//! The panel only draws: *what* to say is decided by `eva-shell`'s state
//! model, which hands over an [`OverlayContent`] (text plus a [`Tone`]) and
//! this shows it. Must be constructed on the main thread — [`Overlay::new`]
//! takes a [`MainThreadMarker`] as proof, per objc2's convention for AppKit
//! types that are not thread-safe.

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSPanel, NSTextField, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// How the panel looks: a tint that says at a glance whether things are
/// fine, done, broken, or waiting for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Working on something: neutral dark.
    Neutral,
    /// It worked: green.
    Ok,
    /// It did not: red.
    Error,
    /// A question for the user: blue.
    Ask,
}

impl Tone {
    /// The panel's background as (red, green, blue, alpha).
    fn background(self) -> (f64, f64, f64, f64) {
        match self {
            Tone::Neutral => (0.10, 0.10, 0.10, 0.88),
            Tone::Ok => (0.05, 0.30, 0.15, 0.92),
            Tone::Error => (0.42, 0.09, 0.09, 0.93),
            Tone::Ask => (0.08, 0.20, 0.42, 0.95),
        }
    }
}

/// What the overlay shows: one or more lines of text and their tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayContent {
    /// The text; `\n` starts a new line.
    pub text: String,
    /// The tint.
    pub tone: Tone,
}

/// The overlay panel itself.
pub struct Overlay {
    panel: objc2::rc::Retained<NSPanel>,
    label: objc2::rc::Retained<NSTextField>,
    mtm: MainThreadMarker,
}

/// AppKit's floating window level (`NSFloatingWindowLevel` in
/// `NSWindow.h`) — a stable, publicly documented constant (value `3`),
/// used here as a plain integer because `objc2-app-kit` exposes
/// `NSWindow::setLevel` as taking the raw `NSInteger`, not a named enum.
const FLOATING_WINDOW_LEVEL: isize = 3;

const PANEL_WIDTH: f64 = 340.0;
const LINE_HEIGHT: f64 = 20.0;
const PANEL_PADDING: f64 = 20.0;
const BOTTOM_MARGIN: f64 = 80.0;

impl Overlay {
    /// Builds the overlay panel, initially hidden.
    pub fn new(mtm: MainThreadMarker) -> Self {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, LINE_HEIGHT + PANEL_PADDING));

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

        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setBackgroundColor(None);
        label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        panel.setContentView(Some(&label));

        Overlay { panel, label, mtm }
    }

    /// Shows `content`, resizing the panel to fit its lines and keeping it
    /// at the bottom center of the main screen, where Wispr Flow's overlay
    /// (and Handy's) conventionally sit.
    pub fn show(&self, content: &OverlayContent) {
        let lines = content.text.lines().count().max(1);
        let height = LINE_HEIGHT * lines as f64 + PANEL_PADDING;
        let size = NSSize::new(PANEL_WIDTH, height);

        let (r, g, b, a) = content.tone.background();
        self.panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a)));
        self.label.setStringValue(&NSString::from_str(&content.text));
        self.label.setFrame(NSRect::new(
            NSPoint::new(0.0, PANEL_PADDING / 2.0 - 2.0),
            NSSize::new(PANEL_WIDTH, height - PANEL_PADDING / 2.0),
        ));
        self.panel.setContentSize(size);
        self.position_bottom_center(size);

        // `orderFrontRegardless` shows the panel without activating the app
        // or stealing focus from whatever the user is dictating into — the
        // entire point of using an `NSPanel` here.
        self.panel.orderFrontRegardless();
    }

    /// Hides the panel.
    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    fn position_bottom_center(&self, size: NSSize) {
        let Some(screen) = objc2_app_kit::NSScreen::mainScreen(self.mtm) else {
            return; // headless session with no screen — nothing to position against
        };
        let frame = screen.frame();
        let x = frame.origin.x + (frame.size.width - size.width) / 2.0;
        let y = frame.origin.y + BOTTOM_MARGIN;
        self.panel.setFrameOrigin(NSPoint::new(x, y));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_its_own_background_and_is_mostly_opaque() {
        let tones = [Tone::Neutral, Tone::Ok, Tone::Error, Tone::Ask];
        let backgrounds: Vec<_> = tones.iter().map(|t| t.background()).collect();
        for (i, a) in backgrounds.iter().enumerate() {
            assert!(a.3 > 0.8, "the text must stay readable over any app: {a:?}");
            for b in &backgrounds[i + 1..] {
                assert_ne!(a, b, "two tones look the same");
            }
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
