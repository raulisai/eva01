//! Holding the `fn` / 🌐 key as the dictation hotkey (`docs/PLAN.md` §10
//! decision 5). `global-hotkey` binds named key *combinations*; a lone
//! modifier held down is not one, so this watches modifier changes directly
//! with an `NSEvent` global monitor and reports when the `fn` flag goes up
//! and down.
//!
//! Needs the Accessibility permission (macOS only delivers other apps' key
//! events to a trusted process), which `eva-shell` already requires for
//! pasting. One system setting can still get in the way: if *Keyboard → Press
//! 🌐 to* is not "Do Nothing", macOS itself reacts to the key (emoji picker,
//! dictation) alongside EVA — `eva doctor` says so.
//!
//! On a MacBook keyboard `fn` is also half of everyday shortcuts: fn+Delete
//! deletes forward, fn+arrows are Home/End/Page Up/Down, fn+F-keys. Each of
//! those used to start a recording (and flash "Escuchando" on screen), so a
//! second monitor watches key presses: a key pressed while `fn` is down means
//! the `fn` was part of a shortcut, reported as [`FnKeyEvent::Combined`].

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

/// The `fn` key changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKeyEvent {
    /// The key went down.
    Pressed,
    /// The key came back up.
    Released,
    /// Another key was pressed while `fn` was down: a shortcut, not
    /// dictation. Reported once per press of `fn`.
    Combined,
}

/// Turns each flags-changed observation into at most one press or release.
/// Kept apart from the AppKit callback so the state machine — the part with
/// logic — is testable without a run loop.
#[derive(Debug, Default)]
pub struct FnTracker {
    down: Cell<bool>,
    combined: Cell<bool>,
}

impl FnTracker {
    /// Feeds the current state of the `fn` flag; returns the transition, if
    /// there was one. Modifier changes that leave `fn` alone (Shift going
    /// down, say) produce nothing.
    pub fn observe(&self, fn_flag_set: bool) -> Option<FnKeyEvent> {
        if self.down.replace(fn_flag_set) == fn_flag_set {
            return None;
        }
        self.combined.set(false);
        Some(if fn_flag_set { FnKeyEvent::Pressed } else { FnKeyEvent::Released })
    }

    /// Feeds a key press (any key but a modifier); returns
    /// [`FnKeyEvent::Combined`] the first time one lands while `fn` is down.
    pub fn key_pressed(&self) -> Option<FnKeyEvent> {
        (self.down.get() && !self.combined.replace(true)).then_some(FnKeyEvent::Combined)
    }
}

/// A running monitor. The monitors are removed when this is dropped.
pub struct FnKeyMonitor {
    monitors: Vec<Retained<AnyObject>>,
}

impl FnKeyMonitor {
    /// Starts watching. `on_event` runs on the main thread, from AppKit's
    /// event delivery, so it must be quick — send the event down a channel.
    /// Returns `None` if macOS refused to install the monitor.
    pub fn start(on_event: impl Fn(FnKeyEvent) + 'static) -> Option<FnKeyMonitor> {
        let tracker = Rc::new(FnTracker::default());
        let on_event = Rc::new(on_event);

        let (flags_tracker, flags_event) = (Rc::clone(&tracker), Rc::clone(&on_event));
        let flags = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event that outlives this call.
            let event = unsafe { event.as_ref() };
            let fn_down = event.modifierFlags().contains(NSEventModifierFlags::Function);
            if let Some(change) = flags_tracker.observe(fn_down) {
                flags_event(change);
            }
        });
        let keys = RcBlock::new(move |_event: NonNull<NSEvent>| {
            if let Some(change) = tracker.key_pressed() {
                on_event(change);
            }
        });

        let flags_monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &flags)?;
        let mut monitor = FnKeyMonitor { monitors: vec![flags_monitor] };
        // Without the key monitor the `fn` key still works, just without
        // telling shortcuts apart; it is not worth refusing the key for.
        if let Some(keys_monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &keys)
        {
            monitor.monitors.push(keys_monitor);
        }
        Some(monitor)
    }
}

impl Drop for FnKeyMonitor {
    fn drop(&mut self) {
        for monitor in &self.monitors {
            // SAFETY: each is exactly what `addGlobalMonitor…` returned.
            unsafe { NSEvent::removeMonitor(monitor) };
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn a_press_then_a_release_are_each_reported_once() {
        let tracker = FnTracker::default();
        assert_eq!(tracker.observe(true), Some(FnKeyEvent::Pressed));
        assert_eq!(tracker.observe(false), Some(FnKeyEvent::Released));
    }

    #[test]
    fn repeated_observations_of_the_same_state_report_nothing() {
        let tracker = FnTracker::default();
        assert_eq!(tracker.observe(false), None, "starting up with fn released is not a release");
        assert_eq!(tracker.observe(true), Some(FnKeyEvent::Pressed));
        assert_eq!(tracker.observe(true), None, "another modifier changing while fn stays down");
        assert_eq!(tracker.observe(true), None);
        assert_eq!(tracker.observe(false), Some(FnKeyEvent::Released));
        assert_eq!(tracker.observe(false), None);
    }

    #[test]
    fn a_key_pressed_while_fn_is_down_is_a_shortcut_reported_once() {
        let tracker = FnTracker::default();
        assert_eq!(tracker.key_pressed(), None, "typing without fn is not a shortcut");
        tracker.observe(true);
        assert_eq!(tracker.key_pressed(), Some(FnKeyEvent::Combined), "fn+Delete, fn+arrow…");
        assert_eq!(tracker.key_pressed(), None, "once per press of fn");
        assert_eq!(tracker.observe(false), Some(FnKeyEvent::Released));
        tracker.observe(true);
        assert_eq!(tracker.key_pressed(), Some(FnKeyEvent::Combined), "a new press starts over");
    }

    #[test]
    fn it_can_press_again_after_a_release() {
        let tracker = FnTracker::default();
        tracker.observe(true);
        tracker.observe(false);
        assert_eq!(tracker.observe(true), Some(FnKeyEvent::Pressed));
    }
}
