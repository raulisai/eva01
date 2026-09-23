#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! EVA01's configuration file: `~/Library/Application Support/EVA01/config.toml`
//! (`docs/PLAN.md` §10 decision 9 — "config + CLI para el MVP", no settings
//! window). Everything has a default, so the file is optional and any part
//! of it may be left out.
//!
//! A config file is written by a person, and people make typos. Loading
//! therefore never fails the app: [`Config::load`] returns the defaults plus
//! a list of human-readable problems ([`Loaded::warnings`]) that `eva doctor`
//! and the startup notification show — the "never crash on user input" rule
//! from `docs/PLAN.md` §3.3, applied to the one input that is hand-edited.
//! Unknown keys are an error for the same reason: a misspelled
//! `[gateway.agent] clsoe_app = "block"` silently doing nothing is worse than
//! being told about it.

pub mod models;
mod paths;
mod policy;
mod projects;

pub use paths::{expand_home, support_dir};
pub use policy::{ActionKind, GatewayConfig, Origin, Policy};
pub use projects::{Project, ProjectIndex, Resolution};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The whole configuration.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The word that turns dictation into a command. A wake word saved with
    /// `eva wake-word` (kept in the database) takes precedence over this.
    pub wake_word: WakeWord,
    /// The dictation hotkey.
    pub hotkey: HotkeyConfig,
    /// Speech-to-text model locations.
    pub stt: SttConfig,
    /// Agent CLIs and where they run.
    pub agents: AgentsConfig,
    /// What the gateway lets through, asks about, or refuses.
    pub gateway: GatewayConfig,
    /// Spoken and on-screen feedback.
    pub feedback: FeedbackConfig,
    /// How dictated text is pasted.
    pub dictation: DictationConfig,
    /// Per-app formatting styles.
    pub styles: StylesConfig,
    /// The optional remote (OpenAI-compatible) text model.
    pub remote: RemoteConfig,
}

/// The default wake word. Kept as a type so `Config::default()` can carry it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct WakeWord(pub String);

impl Default for WakeWord {
    fn default() -> Self {
        WakeWord("Adán".to_string())
    }
}

/// The dictation hotkey.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    /// `"fn"` to hold the Fn/Globe key, or a combination such as
    /// `"cmd+shift+space"` or `"alt+space"`. Combinations are pressed and
    /// held, like Fn.
    pub dictation: String,
    /// The key that answers a confirmation prompt with "yes", pressed while
    /// the prompt is on screen (never by voice — `docs/PLAN.md` §6).
    pub confirm: String,
    /// The key that answers it with "no" (also cancels a recording).
    pub cancel: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig {
            dictation: "fn".to_string(),
            confirm: "cmd+return".to_string(),
            cancel: "cmd+escape".to_string(),
        }
    }
}

/// Where the speech-to-text model lives.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    /// A Canary model directory (the production default, `docs/PLAN.md` §5).
    /// Left empty, `models/canary-1b-flash` and then `models/canary-180m-flash`
    /// inside the support directory are tried.
    pub canary_dir: Option<String>,
    /// A Whisper `ggml-*.bin` file, used only if no Canary model is found.
    pub whisper_path: Option<String>,
    /// The spoken language, as an ISO 639-1 code.
    pub language: String,
}

impl Default for SttConfig {
    fn default() -> Self {
        SttConfig { canary_dir: None, whisper_path: None, language: "es".to_string() }
    }
}

/// Agent CLIs and where they run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentsConfig {
    /// Which agent to try first, in order; ids are `"codex"` and
    /// `"claude_code"`. Agents left out are still available when named
    /// explicitly ("usa Claude y…"), just never picked automatically.
    pub priority: Vec<String>,
    /// Run each dictated task in a throwaway git worktree on its own branch.
    pub worktree: bool,
    /// The project to use when nothing on screen says which one is meant.
    pub default_project: Option<String>,
    /// Folders whose immediate subfolders are the projects EVA knows about
    /// (`list_projects`, and recognizing a project from a window title).
    pub project_roots: Vec<String>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig {
            priority: vec!["codex".to_string(), "claude_code".to_string()],
            worktree: true,
            default_project: None,
            project_roots: vec!["~/code".to_string(), "~/Developer".to_string(), "~/projects".to_string()],
        }
    }
}

/// Spoken and on-screen feedback.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedbackConfig {
    /// The `say` voice used to announce results.
    pub voice: String,
    /// Speak a short summary when an agent task finishes.
    pub speak_task_results: bool,
    /// Show a system notification when an agent task finishes.
    pub notify_task_results: bool,
    /// Seconds an agent task may run before it is stopped; `0` means never.
    pub task_timeout_secs: u64,
}

impl Default for FeedbackConfig {
    fn default() -> Self {
        FeedbackConfig {
            voice: "Mónica".to_string(),
            speak_task_results: true,
            notify_task_results: true,
            task_timeout_secs: 30 * 60,
        }
    }
}

/// How dictated text is pasted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DictationConfig {
    /// Add a space after each pasted dictation, so the next one does not
    /// glue itself to the last word ("Hola.Cómo estás"). Never added in a
    /// terminal, where a trailing space is a stray character in a command.
    pub trailing_space: bool,
}

impl Default for DictationConfig {
    fn default() -> Self {
        DictationConfig { trailing_space: true }
    }
}

/// How the formatter styles text for the app being dictated into
/// (`docs/PLAN.md` fase 9).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct StylesConfig {
    /// Extra bundle-id → style rules, checked before the built-in ones.
    pub apps: Vec<AppStyle>,
}

/// One bundle-id → style rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppStyle {
    /// The app's bundle identifier, e.g. `com.tinyspeck.slackmacgap`.
    pub bundle_id: String,
    /// `"default"`, `"casual"`, `"formal"` or `"terminal"`.
    pub style: String,
}

/// The optional remote text model. Off unless explicitly enabled, because
/// transcripts carry secrets (`docs/PLAN.md` §6) — nothing leaves the machine
/// by default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    /// The master switch. Nothing is ever sent anywhere while this is `false`.
    pub enabled: bool,
    /// The API root, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// The environment variable that holds the API key (the key itself is
    /// never stored in this file).
    pub api_key_env: String,
    /// The model name.
    pub model: String,
    /// What the remote model may be used for: `"edit"` (rewrite selected
    /// text) and/or `"format"` (every dictation).
    pub use_for: Vec<String>,
}

/// The result of [`Config::load`].
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The configuration to use — the defaults, if the file was unreadable.
    pub config: Config,
    /// Human-readable problems found while loading, safe to show as-is.
    pub warnings: Vec<String>,
    /// The file that was (or would have been) read.
    pub path: PathBuf,
}

impl Config {
    /// The default location of the config file.
    pub fn default_path() -> PathBuf {
        support_dir().join("config.toml")
    }

    /// The commented starting file `Abrir configuración…` creates: every
    /// setting, its default, and a line on what it does — the config's own
    /// documentation, where the person editing it will actually look.
    pub fn template() -> &'static str {
        include_str!("config.template.toml")
    }

    /// Makes sure a config file exists at `path`, writing [`Config::template`]
    /// if not. Never overwrites an existing file.
    ///
    /// # Errors
    /// The I/O error if the file or its folder could not be created.
    pub fn ensure_file(path: &Path) -> std::io::Result<()> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, Config::template())
    }

    /// Loads the config from [`Config::default_path`].
    pub fn load() -> Loaded {
        Config::load_from(&Config::default_path())
    }

    /// Loads the config from `path`. A missing file is not a problem (all
    /// defaults); an unreadable or invalid one yields the defaults and a
    /// warning saying why.
    pub fn load_from(path: &Path) -> Loaded {
        let mut warnings = Vec::new();
        let config = match std::fs::read_to_string(path) {
            Ok(text) => match Config::parse(&text) {
                Ok(config) => config,
                Err(message) => {
                    warnings.push(format!("{}: {message} — se usan los valores por defecto", path.display()));
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => {
                warnings.push(format!("no se pudo leer {}: {e} — se usan los valores por defecto", path.display()));
                Config::default()
            }
        };
        warnings.extend(config.validate());
        Loaded { config, warnings, path: path.to_path_buf() }
    }

    /// Parses config text.
    ///
    /// # Errors
    /// The TOML parser's own message, which names the line and the offending
    /// key.
    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Values that parse fine but cannot work. Reported, not fatal.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for id in &self.agents.priority {
            if !["codex", "claude_code"].contains(&id.as_str()) {
                problems
                    .push(format!("agents.priority: no conozco el agente \"{id}\" (usa \"codex\" o \"claude_code\")"));
            }
        }
        for rule in &self.styles.apps {
            if !["default", "casual", "formal", "terminal"].contains(&rule.style.as_str()) {
                problems.push(format!(
                    "styles.apps: el estilo \"{}\" de {} no existe (usa default, casual, formal o terminal)",
                    rule.style, rule.bundle_id
                ));
            }
        }
        if self.remote.enabled {
            if self.remote.base_url.trim().is_empty() || self.remote.model.trim().is_empty() {
                problems.push("remote.enabled está activo pero faltan remote.base_url o remote.model".to_string());
            }
            if self.remote.api_key_env.trim().is_empty() {
                problems.push("remote.enabled está activo pero falta remote.api_key_env".to_string());
            }
        }
        if self.wake_word.0.trim().is_empty() {
            problems.push("wake_word no puede estar vacía".to_string());
        }
        problems
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_all_defaults() {
        assert_eq!(Config::parse("").expect("empty is valid"), Config::default());
    }

    #[test]
    fn a_missing_file_is_all_defaults_with_no_warnings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loaded = Config::load_from(&dir.path().join("config.toml"));
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_partial_file_overrides_only_what_it_names() {
        let config = Config::parse("[hotkey]\ndictation = \"alt+space\"\n\n[agents]\nworktree = false").expect("valid");
        assert_eq!(config.hotkey.dictation, "alt+space");
        assert_eq!(config.hotkey.confirm, "cmd+return", "untouched fields keep their defaults");
        assert!(!config.agents.worktree);
        assert_eq!(config.agents.priority, vec!["codex", "claude_code"]);
    }

    #[test]
    fn defaults_match_the_plan() {
        let config = Config::default();
        assert_eq!(config.wake_word.0, "Adán");
        assert_eq!(config.hotkey.dictation, "fn");
        assert!(config.agents.worktree, "docs/PLAN.md fase 6: worktree por defecto");
        assert!(!config.remote.enabled, "docs/PLAN.md §6: nothing leaves the machine by default");
        assert_eq!(config.stt.language, "es");
    }

    #[test]
    fn dictation_pastes_a_trailing_space_by_default_and_can_turn_it_off() {
        assert!(Config::default().dictation.trailing_space);
        let config = Config::parse("[dictation]\ntrailing_space = false").expect("valid");
        assert!(!config.dictation.trailing_space);
    }

    #[test]
    fn a_typo_in_a_key_is_an_error_not_silently_ignored() {
        let error = Config::parse("[hotkey]\ndictaton = \"fn\"").expect_err("unknown key must fail");
        assert!(error.contains("dictaton"), "the message must name the bad key: {error}");
    }

    #[test]
    fn an_invalid_file_falls_back_to_defaults_with_a_warning_instead_of_failing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "esto no es toml [[[").expect("write");

        let loaded = Config::load_from(&path);
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("valores por defecto"));
    }

    #[test]
    fn validation_flags_values_that_parse_but_cannot_work() {
        let config = Config::parse(
            "[agents]\npriority = [\"codex\", \"gemini\"]\n\n[[styles.apps]]\nbundle_id = \"a.b\"\nstyle = \"pirata\"",
        )
        .expect("parses");
        let problems = config.validate();
        assert!(problems.iter().any(|p| p.contains("gemini")));
        assert!(problems.iter().any(|p| p.contains("pirata")));
    }

    #[test]
    fn enabling_the_remote_model_without_its_settings_is_flagged() {
        let config = Config::parse("[remote]\nenabled = true").expect("parses");
        assert!(config.validate().iter().any(|p| p.contains("remote.base_url")));
    }

    #[test]
    fn the_template_is_valid_and_changes_nothing_until_a_line_is_uncommented() {
        assert_eq!(Config::parse(Config::template()).expect("the template must parse"), Config::default());
    }

    #[test]
    fn the_template_mentions_every_top_level_section() {
        let template = Config::template();
        for section in
            ["hotkey", "stt", "agents", "gateway.voice", "gateway.agent", "feedback", "dictation", "styles", "remote"]
        {
            assert!(
                template.contains(&format!("[{section}]")) || template.contains(&format!("# [{section}]")),
                "missing [{section}]"
            );
        }
    }

    #[test]
    fn ensure_file_writes_the_template_once_and_never_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("EVA01").join("config.toml");

        Config::ensure_file(&path).expect("creates the folder and the file");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), Config::template());

        std::fs::write(&path, "wake_word = \"Eva\"").expect("the user edits it");
        Config::ensure_file(&path).expect("second call");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "wake_word = \"Eva\"",
            "the user's file is never touched"
        );
    }

    #[test]
    fn a_config_round_trips_through_toml() {
        let mut config = Config::default();
        config.agents.default_project = Some("~/code/eva01".to_string());
        let text = toml::to_string(&config).expect("serializes");
        assert_eq!(Config::parse(&text).expect("re-parses"), config);
    }
}
