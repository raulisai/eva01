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

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSFont, NSPanel, NSTextField, NSView, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CALayer;
use std::cell::Cell;

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

/// What EVA is doing while it works: drawn as a small animation next to the
/// text, so the state reads without reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Nothing animated: a plain message (done, error, a question).
    None,
    /// Recording the user: moving voice bars.
    Listening,
    /// Transcribing and formatting: three pulsing dots.
    Thinking,
    /// Running an action: three dots, faster.
    Executing,
}

/// What the overlay shows: one or more lines of text, their tone and the
/// activity animation that goes with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayContent {
    /// The text; `\n` starts a new line.
    pub text: String,
    /// The tint.
    pub tone: Tone,
    /// The animation beside the text ([`Activity::None`] for a message).
    pub activity: Activity,
}

/// The overlay panel itself.
pub struct Overlay {
    panel: Retained<NSPanel>,
    label: Retained<NSTextField>,
    /// The rounded, tinted body of the pill (the content view's layer).
    body: Retained<CALayer>,
    /// The layers the activity animation moves: bars when listening, dots
    /// when thinking or executing.
    glyphs: Vec<Retained<CALayer>>,
    activity: Cell<Activity>,
    mtm: MainThreadMarker,
}

/// AppKit's floating window level (`NSFloatingWindowLevel` in
/// `NSWindow.h`) — a stable, publicly documented constant (value `3`),
/// used here as a plain integer because `objc2-app-kit` exposes
/// `NSWindow::setLevel` as taking the raw `NSInteger`, not a named enum.
const FLOATING_WINDOW_LEVEL: isize = 3;

const PANEL_WIDTH: f64 = 340.0;
/// Height of the compact pill that shows an activity ("Escuchando").
const PILL_HEIGHT: f64 = 40.0;
/// Width reserved for the animation, and the gap before the text.
const GLYPH_WIDTH: f64 = 28.0;
const GLYPH_GAP: f64 = 10.0;
const GLYPH_COUNT: usize = 5;
/// Tallest a voice bar gets.
const BAR_MAX: f64 = 22.0;
/// Space between the text and the panel's edge.
const H_PADDING: f64 = 18.0;
const V_PADDING: f64 = 12.0;
/// Even a one-word status is a comfortable target, not a sliver.
const MIN_HEIGHT: f64 = 44.0;
const BOTTOM_MARGIN: f64 = 80.0;

impl Overlay {
    /// Builds the overlay panel, initially hidden.
    pub fn new(mtm: MainThreadMarker) -> Self {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, MIN_HEIGHT));

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
        // The tint and the rounded corners belong to the content view's layer;
        // the window itself is see-through.
        panel.setBackgroundColor(Some(&NSColor::clearColor()));

        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.23)));
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setBackgroundColor(None);
        label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        label.setPreferredMaxLayoutWidth(PANEL_WIDTH - 2.0 * H_PADDING);

        // The label sits inside a plain container: it is positioned and sized
        // by hand (measured, then centred), which the window would override
        // if the label were the content view itself.
        let container = NSView::initWithFrame(NSView::alloc(mtm), content_rect);
        container.setWantsLayer(true);
        #[allow(clippy::expect_used)] // a layer-backed view always has a layer
        let body = container.layer().expect("a view with wantsLayer has a layer");
        body.setMasksToBounds(true);
        container.addSubview(&label);
        panel.setContentView(Some(&container));

        let white = NSColor::whiteColor().CGColor();
        let glyphs: Vec<_> = (0..GLYPH_COUNT)
            .map(|_| {
                let glyph = CALayer::new();
                glyph.setBackgroundColor(Some(&white));
                glyph.setHidden(true);
                body.addSublayer(&glyph);
                glyph
            })
            .collect();

        Overlay { panel, label, body, glyphs, activity: Cell::new(Activity::None), mtm }
    }

    /// Shows `content` at the bottom center of the main screen, where Wispr
    /// Flow's overlay (and Handy's) conventionally sit. An activity ("Escuchando")
    /// is a compact pill with its animation beside one line of text; a message is
    /// as wide as the panel and as tall as its wrapped text needs. Both have
    /// fully rounded ends.
    pub fn show(&self, content: &OverlayContent) {
        let (r, g, b, a) = content.tone.background();
        self.body.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a).CGColor()));
        self.label.setStringValue(&NSString::from_str(&content.text));
        self.activity.set(content.activity);

        let size = if content.activity == Activity::None {
            self.layout_message()
        } else {
            self.layout_activity()
        };
        self.body.setCornerRadius((size.height / 2.0).min(22.0));
        self.panel.setContentSize(size);
        self.position_bottom_center(size);
        self.animate(0.0);

        // `orderFrontRegardless` shows the panel without activating the app
        // or stealing focus from whatever the user is dictating into — the
        // entire point of using an `NSPanel` here.
        self.panel.orderFrontRegardless();
    }

    /// A message: wrapped text centred in a panel of fixed width.
    fn layout_message(&self) -> NSSize {
        self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH - 2.0 * H_PADDING);
        let text_height = self.label.fittingSize().height.ceil();
        let height = (text_height + 2.0 * V_PADDING).max(MIN_HEIGHT);
        self.label.setFrame(NSRect::new(
            NSPoint::new(H_PADDING, (height - text_height) / 2.0),
            NSSize::new(PANEL_WIDTH - 2.0 * H_PADDING, text_height),
        ));
        for glyph in &self.glyphs {
            glyph.setHidden(true);
        }
        NSSize::new(PANEL_WIDTH, height)
    }

    /// An activity: the animation, then one line of text, in a pill as wide as
    /// they need.
    fn layout_activity(&self) -> NSSize {
        self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH);
        let text = self.label.fittingSize();
        let text_x = H_PADDING - 2.0 + GLYPH_WIDTH + GLYPH_GAP;
        let width = text_x + text.width.ceil() + H_PADDING;
        self.label.setFrame(NSRect::new(
            NSPoint::new(text_x, ((PILL_HEIGHT - text.height) / 2.0).floor()),
            NSSize::new(text.width.ceil(), text.height.ceil()),
        ));
        NSSize::new(width, PILL_HEIGHT)
    }

    /// Moves the activity animation to time `t` (seconds). Cheap: call it on
    /// every tick while the overlay is up.
    pub fn animate(&self, t: f64) {
        let activity = self.activity.get();
        let mid = PILL_HEIGHT / 2.0;
        let left = H_PADDING - 2.0;
        match activity {
            Activity::None => {}
            Activity::Listening => {
                let step = GLYPH_WIDTH / GLYPH_COUNT as f64;
                for (i, glyph) in self.glyphs.iter().enumerate() {
                    let phase = i as f64 * 1.3;
                    // Two sines out of step, so the bars move like a voice
                    // and not like a metronome.
                    let level = 0.5 + 0.5 * (0.5 * (t * 7.0 + phase).sin() + 0.5 * (t * 11.3 + phase * 2.1).sin());
                    let height = 5.0 + level * (BAR_MAX - 5.0);
                    glyph.setHidden(false);
                    glyph.setOpacity(1.0);
                    glyph.setCornerRadius(2.0);
                    glyph.setFrame(NSRect::new(
                        NSPoint::new(left + i as f64 * step + 1.0, mid - height / 2.0),
                        NSSize::new(4.0, height),
                    ));
                }
            }
            Activity::Thinking | Activity::Executing => {
                let speed = if activity == Activity::Thinking { 4.0 } else { 7.0 };
                let step = GLYPH_WIDTH / 3.0;
                for (i, glyph) in self.glyphs.iter().enumerate() {
                    if i >= 3 {
                        glyph.setHidden(true);
                        continue;
                    }
                    // Each dot swells in turn.
                    let swell = 0.5 + 0.5 * (t * speed - i as f64 * 1.1).sin();
                    let diameter = 6.0 + 4.0 * swell;
                    glyph.setHidden(false);
                    glyph.setOpacity((0.45 + 0.55 * swell) as f32);
                    glyph.setCornerRadius(diameter / 2.0);
                    glyph.setFrame(NSRect::new(
                        NSPoint::new(left + i as f64 * step + (step - diameter) / 2.0, mid - diameter / 2.0),
                        NSSize::new(diameter, diameter),
                    ));
                }
            }
        }
    }

    /// Hides the panel.
    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    /// The panel as it is drawn right now, as PNG bytes — for looking at every
    /// state without a screen-recording permission (`examples/overlay_gallery`).
    pub fn snapshot_png(&self) -> Option<Vec<u8>> {
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
        use objc2_foundation::NSDictionary;

        // The content view is only the text: the window's tint is drawn by its
        // frame view, one level up.
        let content = self.panel.contentView()?;
        let view = unsafe { content.superview() }.unwrap_or(content);
        let bounds = view.bounds();
        let representation = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &representation);
        // SAFETY: the properties dictionary is empty, so none of its
        // key/value types can be wrong; `representation` is a live bitmap rep.
        let data = unsafe {
            NSBitmapImageRep::representationUsingType_properties(
                &representation,
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }?;
        Some(data.to_vec())
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
