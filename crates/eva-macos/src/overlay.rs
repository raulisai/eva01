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
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSImage, NSImageView, NSPanel, NSTextField, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CALayer, CAMediaTimingFunction, CATransaction};
use std::cell::Cell;
use std::ptr::NonNull;
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
    /// A command (open, search, an agent…): violet, so it never looks like plain dictation.
    Command,
}

impl Tone {
    /// The tint laid over the frosted glass, as (red, green, blue, alpha).
    /// Light on purpose: the blur underneath does most of the work, and the
    /// island should look like glass, not like a dark slab.
    fn background(self) -> (f64, f64, f64, f64) {
        match self {
            Tone::Neutral => (0.05, 0.05, 0.07, 0.30),
            Tone::Ok => (0.05, 0.40, 0.20, 0.55),
            Tone::Error => (0.55, 0.10, 0.10, 0.60),
            Tone::Ask => (0.10, 0.25, 0.60, 0.55),
            Tone::Command => (0.28, 0.20, 0.62, 0.50),
        }
    }

    /// The colour of the animation and icon: white, or a soft violet for a command.
    fn accent(self) -> (f64, f64, f64) {
        match self {
            Tone::Command => (0.78, 0.74, 1.0),
            _ => (1.0, 1.0, 1.0),
        }
    }
}

/// What EVA is doing while it works: drawn as a small animation next to the
/// text (or on its own), so the state reads without reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Nothing animated: a plain message (done, error, a question).
    None,
    /// Recording: the moving voice bars (with the name "Eva" beside them once
    /// the wake word is heard, alone while it is only dictation).
    Listening,
    /// Working on the words: three dots that rise in turn.
    Thinking,
    /// Running an action: a ring of dots turning (or the action's icon, if it has one).
    Executing,
    /// The arrow that sends the words off: it shows for this many milliseconds
    /// (the wait before the text lands) and then, for a moment more, rises and
    /// fades away.
    Sending(u32),
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

/// A button on the island: what it does, and the key that does it too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What it says: "Sí", "No".
    pub label: String,
    /// The shortcut, shown beside it: "⌘⏎".
    pub shortcut: String,
    /// The answer most people will give: drawn as the solid one.
    pub primary: bool,
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
    /// Buttons under the text, clickable and each with its shortcut (a
    /// question's yes and no). Empty for everything else.
    pub choices: Vec<Choice>,
}

/// Where things go inside the body, worked out once per content.
#[derive(Debug, Clone, Copy)]
struct Layout {
    /// One line beside the animation or icon (a pill), or wrapped text (a card).
    compact: bool,
    /// Width of the space for the animation or icon before the text.
    slot: f64,
    /// Whether there are buttons under the text.
    buttons: bool,
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
    /// The shape the frosted glass behind it is cut to: it follows the body.
    glass_mask: Retained<CALayer>,
    tone: Cell<Tone>,
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
    /// The buttons: their backgrounds and labels, where they are in the panel
    /// (shared with the click monitor), and which one was clicked.
    button_layers: Vec<Retained<CALayer>>,
    button_labels: Vec<Retained<NSTextField>>,
    button_rects: Rc<Cell<[Option<NSRect>; 2]>>,
    clicked: Rc<Cell<Option<usize>>>,
    click_monitor: Option<Retained<AnyObject>>,
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
/// The voice bars while listening, and the tallest one.
const BAR_COUNT: usize = 5;
const BAR_MAX: f64 = 22.0;
/// The animation layers: the spinner needs eight, the others use the first few.
const GLYPH_COUNT: usize = 8;
/// The icon's side.
const ICON_SIZE: f64 = 22.0;
/// Space between the text and the panel's edge.
const H_PADDING: f64 = 18.0;
const V_PADDING: f64 = 12.0;
/// Even a one-word status is a comfortable target, not a sliver.
const MIN_HEIGHT: f64 = 44.0;
const BOTTOM_MARGIN: f64 = 80.0;
/// How long the body takes to change size, and when the text follows it.
/// Buttons: their height and the gap between them.
const BUTTON_HEIGHT: f64 = 30.0;
const BUTTON_GAP: f64 = 10.0;
/// The "sent" arrow: it shows for `ms` (the wait before the paste) plus this
/// tail, rising and fading; a short hold first, so it reads before it moves.
pub const SEND_TAIL_SECS: f64 = 0.20;
const SEND_RISE: f64 = 34.0;
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

        // The glass: a blur of whatever is behind the island, cut to the shape
        // of the body by a mask that moves with it. The tint (the body) is a
        // layer on top of it, so both are clipped by the same mask.
        let glass = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        glass.setMaterial(NSVisualEffectMaterial::HUDWindow);
        glass.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        glass.setState(NSVisualEffectState::Active);
        glass.setWantsLayer(true);
        let glass_mask = CALayer::new();
        glass_mask.setBackgroundColor(Some(&NSColor::blackColor().CGColor()));
        let body = CALayer::new();
        body.setBorderWidth(0.5);
        body.setBorderColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.16).CGColor()));
        #[allow(clippy::expect_used)] // a layer-backed view always has a layer
        let glass_layer = glass.layer().expect("a view with wantsLayer has a layer");
        // SAFETY: `glass_mask` is a live layer that belongs to no other layer, and
        // it is kept (in `Overlay`) for as long as the glass it cuts.
        unsafe { glass_layer.setMask(Some(&glass_mask)) };
        glass_layer.addSublayer(&body);
        container.addSubview(&glass);
        container.addSubview(&label);
        container.addSubview(&icon_view);
        panel.setContentView(Some(&container));

        let glyphs: Vec<_> = (0..GLYPH_COUNT)
            .map(|_| {
                let glyph = CALayer::new();
                glyph.setHidden(true);
                body.addSublayer(&glyph);
                glyph
            })
            .collect();

        let mut button_layers = Vec::new();
        let mut button_labels = Vec::new();
        for _ in 0..2 {
            let layer = CALayer::new();
            layer.setCornerRadius(BUTTON_HEIGHT / 2.0);
            layer.setHidden(true);
            body.addSublayer(&layer);
            button_layers.push(layer);
            let text = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            text.setFont(Some(&NSFont::systemFontOfSize_weight(12.5, 0.3)));
            text.setAlignment(objc2_app_kit::NSTextAlignment::Center);
            text.setHidden(true);
            container.addSubview(&text);
            button_labels.push(text);
        }

        // A click on a button, seen from inside this app (the panel never
        // becomes the active window, so nothing else reports it).
        let button_rects: Rc<Cell<[Option<NSRect>; 2]>> = Rc::new(Cell::new([None, None]));
        let clicked = Rc::new(Cell::new(None));
        let click_monitor = {
            let (rects, clicked, panel) = (Rc::clone(&button_rects), Rc::clone(&clicked), panel.clone());
            let handler = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
                // SAFETY: AppKit passes a live event for the duration of the call.
                let event_ref = unsafe { event.as_ref() };
                if event_ref.windowNumber() == panel.windowNumber() {
                    if let Some(index) = button_at(&rects.get(), event_ref.locationInWindow()) {
                        clicked.set(Some(index));
                    }
                }
                event.as_ptr()
            });
            // SAFETY: the handler returns the event it was given, which is valid.
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &handler) }
        };

        Overlay {
            panel,
            button_layers,
            button_labels,
            glass_mask,
            tone: Cell::new(Tone::Neutral),
            button_rects,
            clicked,
            click_monitor,
            label,
            icon_view,
            body,
            glyphs,
            activity: Cell::new(Activity::None),
            has_icon: Cell::new(false),
            layout: Cell::new(Layout {
                compact: true,
                slot: GLYPH_WIDTH,
                buttons: false,
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
        // A previous "sending" left the panel raised and faded out.
        self.panel.setAlphaValue(1.0);
        let (r, g, b, a) = content.tone.background();
        let tint = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a).CGColor();
        self.label.setStringValue(&NSString::from_str(&content.text));
        self.activity.set(content.activity);
        self.tone.set(content.tone);
        let (ar, ag, ab) = content.tone.accent();
        self.icon_view.setContentTintColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(ar, ag, ab, 1.0)));
        let icon = load_icon(&content.icon);
        self.has_icon.set(icon.is_some());
        self.icon_view.setImage(icon.as_deref());

        let layout = self.measure(content);
        self.layout.set(layout);
        self.clicked.set(None);
        self.set_buttons(&content.choices, layout);
        let target = self.body_frame(layout.body);
        // A card with buttons takes clicks, so its window must be no bigger
        // than it is: the see-through frame above it must not eat the mouse.
        let panel_height = if layout.buttons { layout.body.height } else { PANEL_HEIGHT };
        self.panel.setContentSize(NSSize::new(PANEL_WIDTH, panel_height));
        self.position_panel();

        let was_visible = self.visible.replace(true);
        if !was_visible {
            // A dot at the middle of where the body will be, growing from there.
            let dot = NSSize::new(PILL_HEIGHT / 2.0, PILL_HEIGHT / 2.0);
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.set_shape(self.body_frame(dot), dot.height / 2.0);
            self.body.setBackgroundColor(Some(&tint));
            CATransaction::commit();
        }

        CATransaction::begin();
        CATransaction::setAnimationDuration(MORPH_SECS);
        // Eases out with a little overshoot, like a spring settling.
        CATransaction::setAnimationTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(
            0.3, 1.25, 0.5, 1.0,
        )));
        self.set_shape(target, (layout.body.height / 2.0).min(22.0));
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

    /// Moves the body and the glass mask together, so the blur is always
    /// exactly as big and as round as the tinted shape over it.
    fn set_shape(&self, frame: NSRect, radius: f64) {
        for layer in [&self.body, &self.glass_mask] {
            layer.setFrame(frame);
            layer.setCornerRadius(radius);
        }
    }

    /// How big the body must be for `content`, and how it lays out.
    fn measure(&self, content: &OverlayContent) -> Layout {
        if !content.choices.is_empty() {
            self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH - 2.0 * H_PADDING);
            let text = self.label.fittingSize();
            let text = NSSize::new(PANEL_WIDTH - 2.0 * H_PADDING, text.height.ceil());
            let height = (V_PADDING + BUTTON_HEIGHT + BUTTON_GAP + text.height + V_PADDING).min(PANEL_HEIGHT);
            return Layout { compact: false, slot: 0.0, buttons: true, text, body: NSSize::new(PANEL_WIDTH, height) };
        }
        let compact =
            !content.text.contains('\n') && (content.activity != Activity::None || content.icon != Icon::None);
        if compact {
            self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH);
            let text = self.label.fittingSize();
            let text = if content.text.is_empty() {
                NSSize::new(0.0, 0.0)
            } else {
                NSSize::new(text.width.ceil(), text.height.ceil())
            };
            // An icon is a square; the animations are wider.
            let slot = if content.icon == Icon::None { GLYPH_WIDTH } else { ICON_SIZE };
            // With no words the pill is just its animation, evenly padded.
            let width = if content.text.is_empty() {
                2.0 * (H_PADDING - 2.0) + slot
            } else {
                H_PADDING - 2.0 + slot + GLYPH_GAP + text.width + H_PADDING
            };
            Layout { compact, slot, buttons: false, text, body: NSSize::new(width, PILL_HEIGHT) }
        } else {
            self.label.setPreferredMaxLayoutWidth(PANEL_WIDTH - 2.0 * H_PADDING);
            let text = self.label.fittingSize();
            let text = NSSize::new(PANEL_WIDTH - 2.0 * H_PADDING, text.height.ceil());
            let height = (text.height + 2.0 * V_PADDING).clamp(MIN_HEIGHT, PANEL_HEIGHT);
            Layout { compact, slot: 0.0, buttons: false, text, body: NSSize::new(PANEL_WIDTH, height) }
        }
    }

    /// The body's frame for a given size: centred, resting on the panel's bottom edge.
    fn body_frame(&self, size: NSSize) -> NSRect {
        NSRect::new(NSPoint::new((PANEL_WIDTH - size.width) / 2.0, 0.0), size)
    }

    /// Shows `choices` as buttons in a row under the text (or hides them),
    /// and lets the panel take clicks only while there are any — otherwise
    /// it is see-through to the mouse.
    fn set_buttons(&self, choices: &[Choice], layout: Layout) {
        let mut rects = [None, None];
        let width = (PANEL_WIDTH - 2.0 * H_PADDING - BUTTON_GAP) / 2.0;
        for (index, (layer, text)) in self.button_layers.iter().zip(&self.button_labels).enumerate() {
            let Some(choice) = choices.get(index).filter(|_| layout.buttons) else {
                layer.setHidden(true);
                text.setHidden(true);
                continue;
            };
            let rect = NSRect::new(
                NSPoint::new(H_PADDING + index as f64 * (width + BUTTON_GAP), V_PADDING),
                NSSize::new(width, BUTTON_HEIGHT),
            );
            rects[index] = Some(rect);
            // The solid one is the likely answer; the other a quiet outline of the tint.
            let (fill, ink) = if choice.primary { (1.0, 0.08) } else { (0.18, 1.0) };
            layer.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, fill).CGColor()));
            layer.setFrame(rect);
            layer.setHidden(false);
            let caption = if choice.shortcut.is_empty() {
                choice.label.clone()
            } else {
                format!("{}  {}", choice.label, choice.shortcut)
            };
            text.setStringValue(&NSString::from_str(&caption));
            text.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(ink, ink, ink, 1.0)));
            text.setFrame(NSRect::new(
                NSPoint::new(rect.origin.x, rect.origin.y + (BUTTON_HEIGHT - 16.0) / 2.0),
                NSSize::new(rect.size.width, 16.0),
            ));
            text.setHidden(false);
        }
        self.button_rects.set(rects);
        self.panel.setIgnoresMouseEvents(!layout.buttons);
    }

    /// The button clicked since the last call, as its index in
    /// [`OverlayContent::choices`].
    pub fn take_choice(&self) -> Option<usize> {
        self.clicked.take()
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
        } else if layout.buttons {
            let y = layout.body.height - V_PADDING - layout.text.height;
            self.label.setFrame(NSRect::new(NSPoint::new((PANEL_WIDTH - layout.text.width) / 2.0, y), layout.text));
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
        for (layer, text) in self.button_layers.iter().zip(&self.button_labels) {
            layer.setOpacity(alpha as f32);
            text.setAlphaValue(alpha);
        }
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
        if let Activity::Sending(ms) = self.activity.get() {
            self.fly(since, f64::from(ms) / 1000.0);
        }
    }

    /// The "sent" animation, `since` seconds in. `wait` is how long until the
    /// text lands: the arrow holds still for half of it, then rises and fades,
    /// taking [`SEND_TAIL_SECS`] more after the text has landed.
    fn fly(&self, since: f64, wait: f64) {
        let hold = wait * 0.5 + 0.04;
        let total = wait + SEND_TAIL_SECS;
        let flight = ((since - hold) / (total - hold).max(0.05)).clamp(0.0, 1.0);
        let eased = flight * flight; // gathers speed, like a throw
        self.panel.setAlphaValue(1.0 - eased);
        let Some(screen) = objc2_app_kit::NSScreen::mainScreen(self.mtm) else { return };
        let frame = screen.frame();
        let x = frame.origin.x + (frame.size.width - PANEL_WIDTH) / 2.0;
        let y = frame.origin.y + BOTTOM_MARGIN + SEND_RISE * eased;
        self.panel.setFrameOrigin(NSPoint::new(x, y));
    }

    /// Jumps to the end of every animation: the body at its final size, the
    /// content fully in. For looking at a state (`examples/overlay_gallery`).
    pub fn settle(&self, t: f64) {
        let layout = self.layout.get();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.set_shape(self.body_frame(layout.body), (layout.body.height / 2.0).min(22.0));
        CATransaction::commit();
        self.fade_content(1.0);
        self.draw_glyphs(t, 1.0);
    }

    /// The animation at time `t`, `fade` visible — deliberately abstract and
    /// quiet: voice bars while listening, three dots rising in turn while
    /// thinking, a ring of dots turning while it acts.
    fn draw_glyphs(&self, t: f64, fade: f64) {
        let layout = self.layout.get();
        let activity = self.activity.get();
        let animated = layout.compact && !self.has_icon.get();
        // The animation's slot: a square, centred vertically at its left.
        // Centred when there are no words beside it.
        let centre_x =
            if layout.text.width == 0.0 { layout.body.width / 2.0 } else { H_PADDING - 2.0 + GLYPH_WIDTH / 2.0 };
        let centre = NSPoint::new(centre_x, layout.body.height / 2.0);
        let (r, g, b) = self.tone.get().accent();
        let ink = |alpha: f64| NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha).CGColor();
        let dot = |glyph: &CALayer, at: NSPoint, diameter: f64, alpha: f64| {
            glyph.setHidden(false);
            glyph.setBorderWidth(0.0);
            glyph.setBackgroundColor(Some(&ink(alpha * fade)));
            glyph.setCornerRadius(diameter / 2.0);
            glyph.setFrame(NSRect::new(
                NSPoint::new(at.x - diameter / 2.0, at.y - diameter / 2.0),
                NSSize::new(diameter, diameter),
            ));
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for glyph in &self.glyphs {
            glyph.setHidden(true);
        }
        match activity {
            Activity::Listening if animated => {
                // Voice bars, as they were: two sines out of step, so they
                // move like a voice and not like a metronome.
                let step = GLYPH_WIDTH / BAR_COUNT as f64;
                let left = centre.x - GLYPH_WIDTH / 2.0;
                for (i, glyph) in self.glyphs[..BAR_COUNT].iter().enumerate() {
                    let phase = i as f64 * 1.3;
                    let level = 0.5 + 0.5 * (0.5 * (t * 7.0 + phase).sin() + 0.5 * (t * 11.3 + phase * 2.1).sin());
                    let height = 5.0 + level * (BAR_MAX - 5.0);
                    glyph.setHidden(false);
                    glyph.setBorderWidth(0.0);
                    glyph.setBackgroundColor(Some(&ink(fade)));
                    glyph.setCornerRadius(2.0);
                    glyph.setFrame(NSRect::new(
                        NSPoint::new(left + i as f64 * step + 1.0, centre.y - height / 2.0),
                        NSSize::new(4.0, height),
                    ));
                }
            }
            Activity::Thinking if animated => {
                // Three dots that rise and fall in turn, like a message being typed.
                for (i, glyph) in self.glyphs[..3].iter().enumerate() {
                    let wave = 0.5 + 0.5 * (t * 5.2 - i as f64 * 0.9).sin();
                    let at = NSPoint::new(centre.x + (i as f64 - 1.0) * 9.0, centre.y + 3.5 * (wave - 0.5) * 2.0);
                    dot(glyph, at, 5.5, 0.4 + 0.6 * wave);
                }
            }
            Activity::Executing if animated => {
                // Eight dots on a ring, brightest at the head and fading behind it, turning.
                let head = t * 1.25;
                for (i, glyph) in self.glyphs.iter().enumerate() {
                    let angle = std::f64::consts::TAU * (i as f64 / GLYPH_COUNT as f64);
                    let behind = (head - i as f64 / GLYPH_COUNT as f64).rem_euclid(1.0);
                    let at = NSPoint::new(centre.x + 9.5 * angle.cos(), centre.y + 9.5 * angle.sin());
                    dot(glyph, at, 3.4, 1.0 - 0.8 * behind);
                }
            }
            _ => {}
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
        self.set_shape(self.body_frame(dot), dot.height / 2.0);
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

/// The button under `point` (in the panel's own coordinates), if any.
fn button_at(rects: &[Option<NSRect>; 2], point: NSPoint) -> Option<usize> {
    rects.iter().position(|rect| {
        rect.is_some_and(|r| {
            point.x >= r.origin.x
                && point.x <= r.origin.x + r.size.width
                && point.y >= r.origin.y
                && point.y <= r.origin.y + r.size.height
        })
    })
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if let Some(monitor) = self.click_monitor.take() {
            // SAFETY: `monitor` is the token `addLocalMonitor…` returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
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
    fn every_tone_has_its_own_tint_and_is_translucent_but_not_clear() {
        let tones = [Tone::Neutral, Tone::Ok, Tone::Error, Tone::Ask, Tone::Command];
        let tints: Vec<_> = tones.iter().map(|t| t.background()).collect();
        for (i, a) in tints.iter().enumerate() {
            assert!((0.2..=0.7).contains(&a.3), "glass shows what is behind it, tinted enough to tell the tone: {a:?}");
            for b in &tints[i + 1..] {
                assert_ne!(a, b, "two tones look the same");
            }
        }
    }

    #[test]
    fn a_click_lands_on_the_button_under_it_and_nowhere_else() {
        let yes = NSRect::new(NSPoint::new(18.0, 12.0), NSSize::new(147.0, 30.0));
        let no = NSRect::new(NSPoint::new(175.0, 12.0), NSSize::new(147.0, 30.0));
        let rects = [Some(yes), Some(no)];
        assert_eq!(button_at(&rects, NSPoint::new(30.0, 20.0)), Some(0));
        assert_eq!(button_at(&rects, NSPoint::new(300.0, 40.0)), Some(1));
        assert_eq!(button_at(&rects, NSPoint::new(168.0, 20.0)), None, "the gap between them");
        assert_eq!(button_at(&rects, NSPoint::new(30.0, 80.0)), None, "the text above");
        assert_eq!(button_at(&[None, None], NSPoint::new(30.0, 20.0)), None);
    }

    // Building a real NSPanel needs an actual AppKit application context
    // (NSApplication running on the main thread) that a plain `cargo test`
    // process does not provide — MainThreadMarker::new() legitimately
    // returns None outside of one, so there is nothing meaningful to
    // construct an `Overlay` against here. This is exercised for real in
    // `bins/eva-shell`, which does run inside a real NSApplication event
    // loop on the main thread.
}
