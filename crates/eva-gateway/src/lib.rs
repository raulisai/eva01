#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! The gateway (`docs/PLAN.md` fase 5): the one place every action EVA takes
//! on the user's machine is ruled on, whether the user asked for it by voice
//! or an agent asked for it through MCP. Each [`Action`] gets a policy —
//! run it, ask first, or refuse — from the config and from safety rules that
//! no config can loosen, and every ruling lands in the audit trail with what
//! happened afterwards. "Ask first" is answered by a click or a hotkey on the
//! overlay, never by voice (`docs/PLAN.md` §6): a [`Confirmer`] is the seam
//! to whatever shows that question.

mod action;
mod confirmer;
mod gateway;
mod rules;

pub use action::Action;
pub use confirmer::{mock, Confirmer, DenyAll};
pub use gateway::{Gateway, GatewayError, Ticket, Verdict};
pub use rules::safety_floor;
