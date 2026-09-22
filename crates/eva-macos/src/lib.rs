#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]
#![cfg(target_os = "macos")]

//! Everything in `docs/PLAN.md`'s crate list that has to talk to AppKit
//! directly: the overlay panel, reliable(-ish) paste, frontmost-app
//! detection and app control, and secure-input detection. See
//! `docs/PLAN.md` §3 and `docs/ENGINEERING.md` #5 (why the AppKit-touching
//! parts sit behind traits with mocks, not just get called from everywhere).
//!
//! This crate only compiles on macOS — Windows support is `docs/PLAN.md`
//! fase 10 "later" work, and there is no value in a stub implementation
//! that would compile elsewhere but panic or no-op at runtime.

pub mod error;
pub mod overlay;
pub mod paste;
pub mod secure_input;
pub mod workspace;

pub use error::MacosError;
pub use overlay::{Overlay, OverlayState};
pub use paste::{paste_text, DEFAULT_RESTORE_DELAY};
pub use secure_input::{is_accessibility_trusted, is_secure_input_enabled};
pub use workspace::{close_app, frontmost_app, open_app, open_url, RunningAppInfo};
