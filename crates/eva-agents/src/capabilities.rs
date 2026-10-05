//! What each agent's own account and configuration say about it: the
//! subscription plan, and the model and reasoning effort it is pinned to —
//! read from the files the CLIs already keep on disk. No process is
//! spawned: Codex's own app-server can answer this too, but OpenAI marks it
//! experimental and unsupported for production (`developers.openai.com/codex/app-server`),
//! and a live query on every doctor run or panel load is a cost this
//! read-only, informational display does not need to pay.
//!
//! Never a hard failure: a file that is missing, unreadable, or in a shape
//! this reading does not expect degrades a field to `None` — "not known
//! from here" — never a panic and never a guess. This is purely cosmetic:
//! nothing here gates whether a task may run (that is still
//! [`crate::AgentProvider::detect`] and the rate-limit cooldown in
//! `dispatch.rs`), so a wrong or stale read can only make a label in the
//! panel look off, never send a real dictated command to the wrong agent.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// What is known about one agent's account and configured model, best-effort.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentCapabilities {
    /// The subscription plan, in the CLI's own vocabulary ("plus",
    /// "claude_pro") — never translated or guessed at, so a name this
    /// reading does not recognize is still shown verbatim rather than
    /// hidden. `None` if it could not be read at all.
    pub plan: Option<String>,
    /// The model this agent is pinned to. `None` means nothing pins it —
    /// it runs with whatever its account's own default is, which this
    /// reading does not claim to know without asking the CLI.
    pub model: Option<String>,
    /// The reasoning effort it is pinned to, same convention as `model`.
    pub effort: Option<String>,
}

/// Where `codex` keeps its own state, `~/.codex` — the same place
/// `codex login status` and `codex exec` themselves read.
fn codex_home() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".codex")
}

/// Codex's plan, model and effort, read from this Mac's `~/.codex`.
pub fn codex_capabilities() -> AgentCapabilities {
    let home = codex_home();
    codex_capabilities_at(&home.join("auth.json"), &home.join("config.toml"))
}

fn codex_capabilities_at(auth_json: &Path, config_toml: &Path) -> AgentCapabilities {
    let (model, effort) = read_codex_config(config_toml);
    AgentCapabilities { plan: read_codex_plan(auth_json), model, effort }
}

/// The plan is a claim inside the ChatGPT login's own JWT
/// (`tokens.id_token`, a standard three-part `header.payload.signature`),
/// under the non-standard, URL-shaped claim name `https://api.openai.com/auth`
/// (confirmed by decoding this account's own real token). The signature is
/// never checked here — this only *reads* a claim the CLI already trusted
/// when it logged in, it never *authenticates* anything, so an unverified
/// read is the right amount of trust for a cosmetic label.
fn read_codex_plan(auth_json: &Path) -> Option<String> {
    let text = std::fs::read_to_string(auth_json).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let id_token = doc.pointer("/tokens/id_token")?.as_str()?;
    let payload = id_token.split('.').nth(1)?;
    let claims = decode_jwt_payload(payload)?;
    claims.get("https://api.openai.com/auth")?.get("chatgpt_plan_type")?.as_str().map(str::to_string)
}

fn decode_jwt_payload(payload_b64url: &str) -> Option<serde_json::Value> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let bytes = URL_SAFE_NO_PAD.decode(payload_b64url).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The model and effort pinned in `~/.codex/config.toml` (`model`,
/// `model_reasoning_effort`) — the same file `codex exec` itself reads, and
/// the reason a voice command's tasks can run a different model than what
/// `codex` answers interactively if the two were ever pointed at different
/// homes; here they are always this Mac's one real Codex config.
fn read_codex_config(config_toml: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(config_toml) else { return (None, None) };
    let Ok(doc) = text.parse::<toml::Value>() else { return (None, None) };
    let get = |key: &str| doc.get(key).and_then(|v| v.as_str()).map(str::to_string);
    (get("model"), get("model_reasoning_effort"))
}

/// Claude Code's plan, model and effort, read from this Mac's `~/.claude.json`
/// and `~/.claude/settings.json`, with the environment variables the model
/// and effort are documented to be overridable by
/// (`code.claude.com/docs/en/model-config`): `ANTHROPIC_DEFAULT_MODEL` and
/// `CLAUDE_CODE_EFFORT_LEVEL`.
///
/// This reads only the user's own global settings file — a project's
/// `.claude/settings.json` (which can itself override the model) is not
/// consulted, since there is no one project to read it from here; a task
/// dispatched into a particular project may end up using a different model
/// than this label shows, exactly the way a project-level override would.
pub fn claude_code_capabilities() -> AgentCapabilities {
    let home = dirs::home_dir().unwrap_or_default();
    claude_code_capabilities_at(&home.join(".claude.json"), &home.join(".claude").join("settings.json"), &|key| {
        std::env::var(key).ok()
    })
}

fn claude_code_capabilities_at(
    claude_json: &Path,
    settings_json: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> AgentCapabilities {
    let model = env("ANTHROPIC_DEFAULT_MODEL").or_else(|| read_json_str(settings_json, "model"));
    let effort = env("CLAUDE_CODE_EFFORT_LEVEL").or_else(|| read_json_str(settings_json, "effortLevel"));
    AgentCapabilities { plan: read_claude_plan(claude_json), model, effort }
}

/// The plan lives in `~/.claude.json`'s `oauthAccount.organizationType`
/// (real values seen on this account: `"claude_pro"`) — set once at login,
/// not something a live query is needed for.
fn read_claude_plan(claude_json: &Path) -> Option<String> {
    let text = std::fs::read_to_string(claude_json).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    doc.pointer("/oauthAccount/organizationType")?.as_str().map(str::to_string)
}

fn read_json_str(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    doc.get(key)?.as_str().map(str::to_string)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake_jwt(claims: serde_json::Value) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{payload}.unsigned")
    }

    fn write(dir: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).expect("write fixture");
        path
    }

    #[test]
    fn the_plan_is_read_out_of_a_real_shaped_codex_auth_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let jwt = fake_jwt(serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_plan_type": "plus", "chatgpt_account_id": "acc_123" }
        }));
        let auth = write(&dir, "auth.json", &serde_json::json!({"tokens": {"id_token": jwt}}).to_string());
        let config = dir.path().join("config.toml");

        let caps = codex_capabilities_at(&auth, &config);
        assert_eq!(caps.plan.as_deref(), Some("plus"));
        assert_eq!(caps.model, None);
        assert_eq!(caps.effort, None);
    }

    #[test]
    fn the_model_and_effort_are_read_out_of_config_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = write(&dir, "config.toml", "model = \"gpt-5.6-terra\"\nmodel_reasoning_effort = \"low\"\n");
        let auth = dir.path().join("auth.json");

        let caps = codex_capabilities_at(&auth, &config);
        assert_eq!(caps.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(caps.effort.as_deref(), Some("low"));
        assert_eq!(caps.plan, None, "no auth.json to read a plan from");
    }

    #[test]
    fn missing_or_unreadable_codex_files_degrade_to_none_not_a_panic() {
        let ghost = PathBuf::from("/does/not/exist/on/this/mac");
        assert_eq!(codex_capabilities_at(&ghost, &ghost), AgentCapabilities::default());

        let dir = tempfile::tempdir().expect("tempdir");
        let garbage_auth = write(&dir, "auth.json", "not json at all {{{");
        let garbage_config = write(&dir, "config.toml", "this is not = valid [[[ toml");
        assert_eq!(codex_capabilities_at(&garbage_auth, &garbage_config), AgentCapabilities::default());
    }

    #[test]
    fn a_token_with_an_unexpected_shape_never_panics_and_yields_no_plan() {
        let dir = tempfile::tempdir().expect("tempdir");
        for id_token in ["not.a.jwt.at.all", "onlyonepart", "", "garbage.garbage.garbage"] {
            let auth = write(&dir, "auth.json", &serde_json::json!({"tokens": {"id_token": id_token}}).to_string());
            assert_eq!(read_codex_plan(&auth), None, "{id_token}");
        }
    }

    #[test]
    fn claude_reads_the_plan_from_the_oauth_account() {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude_json = write(
            &dir,
            ".claude.json",
            &serde_json::json!({"oauthAccount": {"organizationType": "claude_pro"}}).to_string(),
        );
        let settings = dir.path().join("settings.json");

        let caps = claude_code_capabilities_at(&claude_json, &settings, &|_| None);
        assert_eq!(caps.plan.as_deref(), Some("claude_pro"));
        assert_eq!(caps.model, None);
    }

    #[test]
    fn claude_env_vars_win_over_the_settings_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude_json = dir.path().join(".claude.json");
        let settings =
            write(&dir, "settings.json", &serde_json::json!({"model": "sonnet", "effortLevel": "high"}).to_string());

        let mut env = HashMap::new();
        env.insert("ANTHROPIC_DEFAULT_MODEL", "opus");
        let caps = claude_code_capabilities_at(&claude_json, &settings, &|k| env.get(k).map(|v| (*v).to_string()));
        assert_eq!(caps.model.as_deref(), Some("opus"), "the env var overrides the settings file");
        assert_eq!(caps.effort.as_deref(), Some("high"), "nothing overrides effort here, so the file wins");
    }

    #[test]
    fn with_neither_env_nor_settings_both_are_unknown_not_a_wrong_guess() {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude_json = dir.path().join(".claude.json");
        let settings = dir.path().join("settings.json");
        let caps = claude_code_capabilities_at(&claude_json, &settings, &|_| None);
        assert_eq!(caps, AgentCapabilities::default());
    }
}
