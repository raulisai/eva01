//! The gateway's policy vocabulary (`docs/PLAN.md` fase 5): every action EVA
//! can take is one of a small set of [`ActionKind`]s, each with a [`Policy`]
//! — allow it, ask first, or refuse — and the policy differs by [`Origin`],
//! because "the user said it" and "an agent decided to" deserve different
//! trust.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What the gateway does with an action.
///
/// Ordered by strictness (`Auto < Confirm < Block`), so combining a
/// configured policy with a built-in safety floor is just `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Run it.
    Auto,
    /// Ask the user first — by click or hotkey, never by voice.
    Confirm,
    /// Refuse it.
    Block,
}

/// Who asked for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The user, by voice command (through the wake word).
    Voice,
    /// An agent, through EVA's MCP server.
    Agent,
}

/// Every kind of action the gateway rules on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Open or focus an application.
    OpenApp,
    /// Quit an application.
    CloseApp,
    /// Open a URL.
    OpenUrl,
    /// A web search.
    WebSearch,
    /// Paste text at the cursor on an agent's behalf. (The user's own
    /// dictation is never gated — it is their own speech.)
    InsertText,
    /// Start an agent task.
    AgentTask,
    /// Rewrite the selected text ("hazlo más formal").
    EditSelection,
    /// Read the selected text out to an agent.
    ReadSelection,
    /// Show a notification.
    Notify,
    /// Speak text aloud.
    Speak,
}

impl ActionKind {
    /// Every kind, in a stable order — for listing policies.
    pub const ALL: [ActionKind; 10] = [
        ActionKind::OpenApp,
        ActionKind::CloseApp,
        ActionKind::OpenUrl,
        ActionKind::WebSearch,
        ActionKind::InsertText,
        ActionKind::AgentTask,
        ActionKind::EditSelection,
        ActionKind::ReadSelection,
        ActionKind::Notify,
        ActionKind::Speak,
    ];

    /// The snake_case name used in the config file and in audit entries.
    pub fn name(self) -> &'static str {
        match self {
            ActionKind::OpenApp => "open_app",
            ActionKind::CloseApp => "close_app",
            ActionKind::OpenUrl => "open_url",
            ActionKind::WebSearch => "web_search",
            ActionKind::InsertText => "insert_text",
            ActionKind::AgentTask => "agent_task",
            ActionKind::EditSelection => "edit_selection",
            ActionKind::ReadSelection => "read_selection",
            ActionKind::Notify => "notify",
            ActionKind::Speak => "speak",
        }
    }
}

/// The `[gateway]` section: a policy table per origin. Only what a table
/// names is overridden; everything else keeps the default below.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    /// Policies for what the user asks for by voice.
    pub voice: BTreeMap<ActionKind, Policy>,
    /// Policies for what an agent asks for through MCP.
    pub agent: BTreeMap<ActionKind, Policy>,
    /// Seconds a confirmation waits for an answer before counting as "no".
    pub confirm_timeout_secs: Option<u64>,
}

/// How long a confirmation waits when the config does not say.
pub const DEFAULT_CONFIRM_TIMEOUT_SECS: u64 = 30;

impl GatewayConfig {
    /// The policy for `kind` when asked by `origin`: the config's override if
    /// it has one, else the built-in default.
    pub fn policy(&self, origin: Origin, kind: ActionKind) -> Policy {
        let overrides = match origin {
            Origin::Voice => &self.voice,
            Origin::Agent => &self.agent,
        };
        overrides.get(&kind).copied().unwrap_or_else(|| default_policy(origin, kind))
    }

    /// The confirmation timeout in effect.
    pub fn confirm_timeout_secs(&self) -> u64 {
        self.confirm_timeout_secs.unwrap_or(DEFAULT_CONFIRM_TIMEOUT_SECS)
    }
}

/// The built-in defaults. What the user says out loud is trusted (it took the
/// wake word to get here, and the destructive-verb blocklist already ran);
/// what an agent decides on its own is trusted less where a mistake is
/// costly: quitting apps, typing into whatever has focus (a paste with a
/// newline into a terminal *runs* it), and reading what the user has
/// selected.
fn default_policy(origin: Origin, kind: ActionKind) -> Policy {
    match (origin, kind) {
        (Origin::Agent, ActionKind::CloseApp | ActionKind::InsertText | ActionKind::ReadSelection) => Policy::Confirm,
        _ => Policy::Auto,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn policies_order_by_strictness() {
        assert!(Policy::Auto < Policy::Confirm);
        assert!(Policy::Confirm < Policy::Block);
        assert_eq!(Policy::Auto.max(Policy::Block), Policy::Block);
    }

    #[test]
    fn what_the_user_says_is_trusted_by_default() {
        let config = GatewayConfig::default();
        for kind in ActionKind::ALL {
            assert_eq!(config.policy(Origin::Voice, kind), Policy::Auto, "{kind:?}");
        }
    }

    #[test]
    fn an_agent_needs_confirmation_for_the_costly_actions_only() {
        let config = GatewayConfig::default();
        for kind in [ActionKind::CloseApp, ActionKind::InsertText, ActionKind::ReadSelection] {
            assert_eq!(config.policy(Origin::Agent, kind), Policy::Confirm, "{kind:?}");
        }
        for kind in [ActionKind::OpenApp, ActionKind::OpenUrl, ActionKind::Notify, ActionKind::Speak] {
            assert_eq!(config.policy(Origin::Agent, kind), Policy::Auto, "{kind:?}");
        }
    }

    #[test]
    fn the_config_can_tighten_or_loosen_a_single_policy_per_origin() {
        let config: GatewayConfig = toml::from_str(
            "[voice]\nagent_task = \"confirm\"\n\n[agent]\nclose_app = \"block\"\nopen_url = \"confirm\"\n",
        )
        .expect("valid");
        assert_eq!(config.policy(Origin::Voice, ActionKind::AgentTask), Policy::Confirm);
        assert_eq!(config.policy(Origin::Voice, ActionKind::OpenApp), Policy::Auto, "untouched stays default");
        assert_eq!(config.policy(Origin::Agent, ActionKind::CloseApp), Policy::Block);
        assert_eq!(config.policy(Origin::Agent, ActionKind::OpenUrl), Policy::Confirm);
        assert_eq!(config.policy(Origin::Agent, ActionKind::InsertText), Policy::Confirm);
    }

    #[test]
    fn an_unknown_action_name_in_the_config_is_rejected() {
        let result: Result<GatewayConfig, _> = toml::from_str("[agent]\nrun_shell = \"auto\"");
        assert!(result.is_err(), "there is no run_shell action, by design (docs/PLAN.md fase 8)");
    }

    #[test]
    fn the_confirmation_timeout_defaults_and_can_be_overridden() {
        assert_eq!(GatewayConfig::default().confirm_timeout_secs(), 30);
        let config: GatewayConfig = toml::from_str("confirm_timeout_secs = 10").expect("valid");
        assert_eq!(config.confirm_timeout_secs(), 10);
    }

    #[test]
    fn every_kind_has_a_unique_name_matching_its_serde_form() {
        let mut names = std::collections::HashSet::new();
        for kind in ActionKind::ALL {
            assert!(names.insert(kind.name()));
            let table = toml::to_string(&BTreeMap::from([(kind, Policy::Auto)])).expect("serializes");
            assert!(table.starts_with(kind.name()), "{table}");
        }
    }
}
