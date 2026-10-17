//! What kind of failure an agent run that died early was — the difference
//! between "try the next agent", "fix it and try again" and "the user has to
//! sign in again". Read off the CLIs' own error text, the only signal they
//! give; every pattern here was seen for real on this Mac:
//!
//! - Claude Code, signed out: `Failed to authenticate: OAuth session expired
//!   and could not be refreshed`.
//! - Codex, a model the account does not take: `The 'gpt-6.1-sol' model is
//!   not supported when using Codex with a ChatGPT account.`; a CLI too old
//!   for its configured model: `The 'gpt-5.6-terra' model requires a newer
//!   version of Codex.`

/// Why an agent run failed before doing any work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The CLI's sign-in expired or is missing: only signing in again fixes
    /// it, and only the user can do that.
    SignedOut,
    /// The CLI rejected the model it was configured with: another model of
    /// its own catalog may well work.
    ModelRejected,
    /// Anything else.
    Other,
}

/// Phrases of a signed-out CLI, lowercase.
const SIGNED_OUT: &[&str] = &[
    "failed to authenticate",
    "oauth session expired",
    "could not be refreshed",
    "not logged in",
    "please run /login",
    "please log in",
    "run `codex login`",
    "run codex login",
    "invalid api key",
    "authentication_error",
    "unauthorized",
    "token expired",
    "refresh token",
    "401",
];

/// Phrases of a CLI that will not use its configured model, lowercase.
const MODEL_REJECTED: &[&str] = &[
    "model is not supported",
    "requires a newer version of codex",
    "model_not_found",
    "does not exist or you do not have access",
    "unknown model",
    "invalid model",
];

/// The kind of failure `message` describes.
pub fn classify(message: &str) -> FailureKind {
    let message = message.to_lowercase();
    if MODEL_REJECTED.iter().any(|phrase| message.contains(phrase)) {
        FailureKind::ModelRejected
    } else if SIGNED_OUT.iter().any(|phrase| message.contains(phrase)) {
        FailureKind::SignedOut
    } else {
        FailureKind::Other
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_real_messages_of_this_mac_are_told_apart() {
        assert_eq!(
            classify("Failed to authenticate: OAuth session expired and could not be refreshed"),
            FailureKind::SignedOut
        );
        assert_eq!(
            classify("The 'gpt-6.1-sol' model is not supported when using Codex with a ChatGPT account."),
            FailureKind::ModelRejected
        );
        assert_eq!(
            classify("The 'gpt-5.6-terra' model requires a newer version of Codex. Please upgrade."),
            FailureKind::ModelRejected
        );
        assert_eq!(classify("se agotó el tiempo límite de 30 minutos"), FailureKind::Other);
        assert_eq!(classify("no such file or directory"), FailureKind::Other);
    }

    #[test]
    fn a_rejected_model_is_not_mistaken_for_a_sign_in_even_with_a_status_code() {
        // Codex wraps the model error in a 400 JSON body; nothing about it says 401.
        let codex = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The 'x' model is not supported when using Codex with a ChatGPT account."}}"#;
        assert_eq!(classify(codex), FailureKind::ModelRejected);
    }
}
