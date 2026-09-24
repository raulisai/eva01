//! The island: a small, borderless panel that shows what EVA is doing without
//! ever stealing keyboard focus from whatever app the user is dictating into.
//! Backed by `NSPanel` with the `NonactivatingPanel` style mask — the same
//! technique `tauri-nspanel` wraps for Tauri apps, used here directly.
//!
//! Like the iPhone's Dynamic Island it is one shape that changes size: the
//! panel itself never resizes (it is a fixed, see-through, click-through
//! frame) and the tinted body inside it is animated by Core Animation, on the
//! GPU, from whatever it was to whatever the new content needs. Text and
//! icon fade in as the body settles.
//!
//! The panel only draws: *what* to say is decided by `eva-shell`'s state
//! model, which hands over an [`OverlayContent`] and this shows it. Must be
//! constructed on the main thread — [`Overlay::new`] takes a
//! [`MainThreadMarker`] as proof, per objc2's convention for AppKit types
//! that are not thread-safe.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSImage, NSImageView, NSPanel, NSTextField, NSView, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CALayer, CAMediaTimingFunction, CATransaction};
use std::cell::Cell;
use std::rc::Rc;

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
    /// Running an action: three dots, faster (or the action's icon, if it has one).
    Executing,
}

/// The picture that says what is being done, in place of the animation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Icon {
    /// No picture.
    None,
    /// An SF Symbol by name (`"globe"`, `"magnifyingglass"`).
    Symbol(&'static str),
    /// An installed app's own icon, by the name of its bundle ("Spotify").
    App(String),
}

/// What the overlay shows: one or more lines of text, their tone, the
/// activity animation and the icon that go with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayContent {
    /// The text; `\n` starts a new line.
    pub text: String,
    /// The tint.
    pub tone: Tone,
    /// The animation beside the text ([`Activity::None`] for a message).
    pub activity: Activity,
    /// The picture beside the text, instead of the animation.
    pub icon: Icon,
}

/// Where things go inside the body, worked out once per content.
#[derive(Debug, Clone, Copy)]
struct Layout {
    /// One line beside the animation or icon (a pill), or wrapped text (a card).
    compact: bool,
    /// Width of the space for the animation or icon before the text.
    slot: f64,
    /// The text's size.
    text: NSSize,
    /// The body's size.
    body: NSSize,
}

/// The overlay panel itself.
pub struct Overlay {
    panel: Retained<NSPanel>,
    label: Retained<NSTextField>,
    icon_view: Retained<NSImageView>,
    /// The rounded, tinted body of the island — the only thing that changes size.
    body: Retained<CALayer>,
    /// The layers the activity animation moves: bars when listening, dots
    /// when thinking or executing.
    glyphs: Vec<Retained<CALayer>>,
    activity: Cell<Activity>,
    has_icon: Cell<bool>,
    layout: Cell<Layout>,
    /// Whether the panel is on screen, and which show/hide is the latest (a
    /// hide finishing after a newer show must not close the panel).
    visible: Cell<bool>,
    generation: Rc<Cell<u64>>,
    /// Seconds (animation clock) at which the current content appeared.
    appeared: Cell<f64>,
    mtm: MainThreadMarker,
}

/// AppKit's floating window level (`NSFloatingWindowLevel` in
/// `NSWindow.h`) — a stable, publicly documented constant (value `3`),
/// used here as a plain integer because `objc2-app-kit` exposes
/// `NSWindow::setLevel` as taking the raw `NSInteger`, not a named enum.
const FLOATING_WINDOW_LEVEL: isize = 3;

const PANEL_WIDTH: f64 = 340.0;
/// The see-through frame the island lives in: room for the tallest card.
const PANEL_HEIGHT: f64 = 170.0;
/// Height of the compact pill that shows an activity ("Escuchando").
const PILL_HEIGHT: f64 = 40.0;
/// Width reserved for the animation or icon, and the gap before the text.
const GLYPH_WIDTH: f64 = 28.0;
const GLYPH_GAP: f64 = 10.0;
const GLYPH_COUNT: usize = 5;
/// The icon's side.
const ICON_SIZE: f64 = 22.0;
/// Tallest a voice bar gets.
const BAR_MAX: f64 = 22.0;
/// Space between the text and the panel's edge.
const H_PADDING: f64 = 18.0;
const V_PADDING: f64 = 12.0;
/// Even a one-word status is a comfortable target, not a sliver.
const MIN_HEIGHT: f64 = 44.0;
const BOTTOM_MARGIN: f64 = 80.0;
/// How long the body takes to change size, and when the text follows it.
const MORPH_SECS: f64 = 0.42;
const HIDE_SECS: f64 = 0.22;
const CONTENT_DELAY_SECS: f64 = 0.10;
const CONTENT_FADE_SECS: f64 = 0.16;

impl Overlay {
    /// Builds the overlay panel, initially hidden.
    pub fn new(mtm: MainThreadMarker) -> Self {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, PANEL_HEIGHT));

        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setLevel(FLOATING_WINDOW_LEVEL);
        panel.setOpaque(false);
        // Only the body is drawn; the rest of the frame is see-through, and
        // must not catch clicks meant for what is under it.
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));

        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.23)));
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setBackgroundColor(None);
        label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        label.setPreferredMaxLayoutWidth(PANEL_WIDTH - 2.0 * H_PADDING);

        let icon_view = NSImageView::initWithFrame(
            NSImageView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(ICON_SIZE, ICON_SIZE)),
        );
        icon_view.setContentTintColor(Some(&NSColor::whiteColor()));
        icon_view.setHidden(true);

        // The label and icon sit in a plain, transparent container; the body
        // is a layer of it, so that its size can be animated by Core
        // Animation while the views stay where the final layout puts them.
        let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
        container.setWantsLayer(true);
        let body = CALayer::new();
        body.setMasksToBounds(true);
        #[allow(clippy::expect_used)] // a layer-backed view always has a layer
        container.layer().expect("a view with wantsLayer has a layer").addSublayer(&body);
        container.addSubview(&label);
        container.addSubview(&icon_view);
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

        Overlay {
            panel,
            label,
            icon_view,
            body,
            glyphs,
            activity: Cell::new(Activity::None),
            has_icon: Cell::new(false),
            layout: Cell::new(Layout {
                compact: true,
                slot: GLYPH_WIDTH,
                text: NSSize::new(0.0, 0.0),
                body: NSSize::new(0.0, 0.0),
            }),
            visible: Cell::new(false),
            generation: Rc::new(Cell::new(0)),
            appeared: Cell::new(0.0),
            mtm,
        }
    }

    /// Shows `content` at the bottom center of the main screen, where Wispr
    /// Flow's overlay (and Handy's) conventionally sit. An activity ("Escuchando")
    /// is a compact pill with its animation or icon beside one line of text; a
    /// message is as wide as the panel and as tall as its wrapped text needs.
    /// Appearing, it grows from a dot; already on screen, it changes size
    /// from what it was — never a jump.
    pub fn show(&self, content: &OverlayContent) {
        self.generation.set(self.generation.get() + 1);
        let (r, g, b, a) = content.tone.background();
        let tint = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a).CGColor();
        self.label.setStringValue(&NSString::from_str(&content.text));
        self.activity.set(content.activity);
        let icon = load_icon(&content.icon);
        self.has_icon.set(icon.is_some());
        self.icon_view.setImage(icon.as_deref());

        let layout = self.measure(content);
        self.layout.set(layout);
        let target = self.body_frame(layout.body);
        self.position_panel();

        let was_visible = self.visible.replace(true);
        if !was_visible {
            // A dot at the middle of where the body will be, growing from there.
            let dot = NSSize::new(PILL_HEIGHT / 2.0, PILL_HEIGHT / 2.0);
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.body.setFrame(self.body_frame(dot));
            self.body.setCornerRadius(dot.height / 2.0);
            self.body.setBackgroundColor(Some(&tint));
            CATransaction::commit();
        }

        CATransaction::begin();
        CATransaction::setAnimationDuration(MORPH_SECS);
        // Eases out with a little overshoot, like a spring settling.
        CATransaction::setAnimationTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(
            0.3, 1.25, 0.5, 1.0,
        )));
        self.body.setFrame(target);
        self.body.setCornerRadius((layout.body.height / 2.0).min(22.0));
        self.body.setBackgroundColor(Some(&tint));
        CATransaction::commit();

        self.place_content(layout);
        // The text and icon come in after the body has started to move.
        self.appeared.set(f64::NAN);
        self.fade_content(0.0);
        // `orderFrontRegardless` shows the panel without activating the app
        // or stealing focus from whatever the user is dictating into — the
        // entire point of using an `NSPanel` here.
        self.panel.orderFrontRegardless();
    }

    /// How big the body must be for `content`, and how it lays out.
    fn measure(&self, content: &OverlayContent) -> Layout {
        let compact =
            !content.text.contains('\n') && (content.activity != Activity::None || content.icon != Icon::None);
        if compact {
            self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH);
            let text = self.label.fittingSize();
            let text = NSSize::new(text.width.ceil(), text.height.ceil());
            // An icon is a square; the animations are wider.
            let slot = if content.icon == Icon::None { GLYPH_WIDTH } else { ICON_SIZE };
            let width = H_PADDING - 2.0 + slot + GLYPH_GAP + text.width + H_PADDING;
            Layout { compact, slot, text, body: NSSize::new(width, PILL_HEIGHT) }
        } else {
            self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH - 2.0 * H_PADDING);
            let text = self.label.fittingSize();
            let text = NSSize::new(PANEL_WIDTH - 2.0 * H_PADDING, text.height.ceil());
            let height = (text.height + 2.0 * V_PADDING).clamp(MIN_HEIGHT, PANEL_HEIGHT);
            Layout { compact, slot: 0.0, text, body: NSSize::new(PANEL_WIDTH, height) }
        }
    }

    /// The body's frame for a given size: centred, resting on the panel's bottom edge.
    fn body_frame(&self, size: NSSize) -> NSRect {
        NSRect::new(NSPoint::new((PANEL_WIDTH - size.width) / 2.0, 0.0), size)
    }

    /// Puts the label and icon where the final layout wants them (absolute
    /// positions in the panel, so they do not move while the body does).
    fn place_content(&self, layout: Layout) {
        let left = (PANEL_WIDTH - layout.body.width) / 2.0 + H_PADDING - 2.0;
        let mid = layout.body.height / 2.0;
        if layout.compact {
            let text_x = left + layout.slot + GLYPH_GAP;
            self.label
                .setFrame(NSRect::new(NSPoint::new(text_x, (mid - layout.text.height / 2.0).floor()), layout.text));
            self.icon_view
                .setFrame(NSRect::new(NSPoint::new(left, mid - ICON_SIZE / 2.0), NSSize::new(ICON_SIZE, ICON_SIZE)));
        } else {
            self.label.setFrame(NSRect::new(
                NSPoint::new((PANEL_WIDTH - layout.text.width) / 2.0, mid - layout.text.height / 2.0),
                layout.text,
            ));
        }
        self.icon_view.setHidden(!(layout.compact && self.has_icon.get()));
    }

    /// Sets how visible the text, icon and animation are, `0.0` to `1.0`.
    fn fade_content(&self, alpha: f64) {
        self.label.setAlphaValue(alpha);
        self.icon_view.setAlphaValue(alpha);
    }

    /// Moves the animations to time `t` (seconds) and fades the text in
    /// once the body has started to settle. Cheap: call it on every tick
    /// while the overlay is up.
    pub fn animate(&self, t: f64) {
        if self.appeared.get().is_nan() {
            self.appeared.set(t);
        }
        let since = t - self.appeared.get();
        let fade = ((since - CONTENT_DELAY_SECS) / CONTENT_FADE_SECS).clamp(0.0, 1.0);
        self.fade_content(fade);
        self.draw_glyphs(t, fade);
    }

    /// Jumps to the end of every animation: the body at its final size, the
    /// content fully in. For looking at a state (`examples/overlay_gallery`).
    pub fn settle(&self, t: f64) {
        let layout = self.layout.get();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.body.setFrame(self.body_frame(layout.body));
        self.body.setCornerRadius((layout.body.height / 2.0).min(22.0));
        CATransaction::commit();
        self.fade_content(1.0);
        self.draw_glyphs(t, 1.0);
    }

    /// The listening bars and thinking dots at time `t`, `fade` visible.
    fn draw_glyphs(&self, t: f64, fade: f64) {
        let layout = self.layout.get();
        let activity = self.activity.get();
        let animated = layout.compact && !self.has_icon.get();
        let mid = layout.body.height / 2.0;
        let left = H_PADDING - 2.0;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        match activity {
            Activity::Listening if animated => {
                let step = GLYPH_WIDTH / GLYPH_COUNT as f64;
                for (i, glyph) in self.glyphs.iter().enumerate() {
                    let phase = i as f64 * 1.3;
                    // Two sines out of step, so the bars move like a voice
                    // and not like a metronome.
                    let level = 0.5 + 0.5 * (0.5 * (t * 7.0 + phase).sin() + 0.5 * (t * 11.3 + phase * 2.1).sin());
                    let height = 5.0 + level * (BAR_MAX - 5.0);
                    glyph.setHidden(false);
                    glyph.setOpacity(fade as f32);
                    glyph.setCornerRadius(2.0);
                    glyph.setFrame(NSRect::new(
                        NSPoint::new(left + i as f64 * step + 1.0, mid - height / 2.0),
                        NSSize::new(4.0, height),
                    ));
                }
            }
            Activity::Thinking | Activity::Executing if animated => {
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
                    glyph.setOpacity(((0.45 + 0.55 * swell) * fade) as f32);
                    glyph.setCornerRadius(diameter / 2.0);
                    glyph.setFrame(NSRect::new(
                        NSPoint::new(left + i as f64 * step + (step - diameter) / 2.0, mid - diameter / 2.0),
                        NSSize::new(diameter, diameter),
                    ));
                }
            }
            _ => {
                for glyph in &self.glyphs {
                    glyph.setHidden(true);
                }
            }
        }
        CATransaction::commit();
    }

    /// Shrinks the island to a dot and takes the panel off screen.
    pub fn hide(&self) {
        if !self.visible.replace(false) {
            return;
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.fade_content(0.0);
        let layout = self.layout.get();
        let dot = NSSize::new(PILL_HEIGHT / 2.0, PILL_HEIGHT / 2.0);

        let latest = Rc::clone(&self.generation);
        let panel = self.panel.clone();
        let closed = block2::RcBlock::new(move || {
            // A newer show (or hide) owns the panel now.
            if latest.get() == generation {
                panel.orderOut(None);
            }
        });
        CATransaction::begin();
        CATransaction::setAnimationDuration(HIDE_SECS);
        CATransaction::setAnimationTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(
            0.4, 0.0, 0.8, 0.4,
        )));
        // SAFETY: the block only reads a counter and orders out a panel it
        // owns, both on the main thread the transaction completes on.
        unsafe { CATransaction::setCompletionBlock(Some(&closed)) };
        self.body.setFrame(self.body_frame(dot));
        self.body.setCornerRadius(dot.height / 2.0);
        CATransaction::commit();
        let _ = layout;
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

    /// The see-through frame, at the bottom centre of the main screen.
    fn position_panel(&self) {
        let Some(screen) = objc2_app_kit::NSScreen::mainScreen(self.mtm) else {
            return; // headless session with no screen — nothing to position against
        };
        let frame = screen.frame();
        let x = frame.origin.x + (frame.size.width - PANEL_WIDTH) / 2.0;
        let y = frame.origin.y + BOTTOM_MARGIN;
        self.panel.setFrameOrigin(NSPoint::new(x, y));
    }
}

/// The picture for `icon`, if it has one: an SF Symbol, or an app's own icon
/// (a generic one if the app cannot be found).
fn load_icon(icon: &Icon) -> Option<Retained<NSImage>> {
    let symbol =
        |name: &str| NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None);
    match icon {
        Icon::None => None,
        Icon::Symbol(name) => symbol(name),
        Icon::App(name) => {
            let path = crate::workspace::app_bundle_path(name).and_then(|path| path.to_str().map(str::to_string));
            match path {
                Some(path) => Some(NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&path))),
                None => symbol("app"),
            }
        }
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
