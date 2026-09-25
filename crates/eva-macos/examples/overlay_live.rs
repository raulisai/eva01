//! Puts the island on the real screen, state after state, so the frosted glass
//! and the animations can be seen as they are (the gallery's PNGs cannot show
//! the blur). Run: `cargo run -p eva-macos --example overlay_live` — 3 s per state.

use eva_macos::{Activity, Icon, Overlay, OverlayContent, Tone};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSDate, NSRunLoop};
use std::time::Instant;

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let overlay = Overlay::new(mtm);

    let content = |text: &str, tone, activity, icon| OverlayContent {
        text: text.to_string(),
        tone,
        activity,
        icon,
        choices: Vec::new(),
    };
    let states = [
        // Dictating: the voice bars, no words.
        content("", Tone::Neutral, Activity::Listening, Icon::None),
        // Said "Eva…": the same bars, with the name.
        content("Eva", Tone::Command, Activity::Listening, Icon::None),
        // Working on the words: only dots.
        content("", Tone::Neutral, Activity::Thinking, Icon::None),
        // The arrow that sends the words off: 120 ms before the text lands, and a moment after.
        content("", Tone::Neutral, Activity::Sending(120), Icon::Symbol("arrow.up")),
        content("Abriendo Spotify", Tone::Command, Activity::Executing, Icon::App("Spotify".to_string())),
    ];
    let started = Instant::now();
    for state in &states {
        overlay.show(state);
        let until = Instant::now() + std::time::Duration::from_secs(3);
        // Each state is shown for three seconds; a "sending" one flies away in 0.6 s and stays gone.
        while Instant::now() < until {
            overlay.animate(started.elapsed().as_secs_f64());
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.03));
        }
    }
    overlay.hide();
    NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.5));
}
