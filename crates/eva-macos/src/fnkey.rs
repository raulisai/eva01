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

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
use std::cell::Cell;
use std::ptr::NonNull;

/// The `fn` key changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKeyEvent {
    /// The key went down.
    Pressed,
    /// The key came back up.
    Released,
}

/// Turns each flags-changed observation into at most one press or release.
/// Kept apart from the AppKit callback so the state machine — the part with
/// logic — is testable without a run loop.
#[derive(Debug, Default)]
pub struct FnTracker {
    down: Cell<bool>,
}

impl FnTracker {
    /// Feeds the current state of the `fn` flag; returns the transition, if
    /// there was one. Modifier changes that leave `fn` alone (Shift going
    /// down, say) produce nothing.
    pub fn observe(&self, fn_flag_set: bool) -> Option<FnKeyEvent> {
        if self.down.replace(fn_flag_set) == fn_flag_set {
            return None;
        }
        Some(if fn_flag_set { FnKeyEvent::Pressed } else { FnKeyEvent::Released })
    }
}

/// A running monitor. The monitor is removed when this is dropped.
pub struct FnKeyMonitor {
    monitor: Retained<AnyObject>,
}

impl FnKeyMonitor {
    /// Starts watching. `on_event` runs on the main thread, from AppKit's
    /// event delivery, so it must be quick — send the event down a channel.
    /// Returns `None` if macOS refused to install the monitor.
    pub fn start(on_event: impl Fn(FnKeyEvent) + 'static) -> Option<FnKeyMonitor> {
        let tracker = FnTracker::default();
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event that outlives this call.
            let event = unsafe { event.as_ref() };
            let fn_down = event.modifierFlags().contains(NSEventModifierFlags::Function);
            if let Some(change) = tracker.observe(fn_down) {
                on_event(change);
            }
        });
        let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &handler)?;
        Some(FnKeyMonitor { monitor })
    }
}

impl Drop for FnKeyMonitor {
    fn drop(&mut self) {
        // SAFETY: `monitor` is exactly what `addGlobalMonitor…` returned.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
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
    fn it_can_press_again_after_a_release() {
        let tracker = FnTracker::default();
        tracker.observe(true);
        tracker.observe(false);
        assert_eq!(tracker.observe(true), Some(FnKeyEvent::Pressed));
    }
}
