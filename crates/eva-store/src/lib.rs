#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! History, personal dictionary, settings, and gateway audit trail — all in
//! one local SQLite file. See `docs/PLAN.md` §3 (`eva-store` in the crate
//! list) and §3.3 point 5 (the corrupted-database recovery this crate does
//! on open).
//!
//! [`Store`] is the single entry point; the submodules are free functions
//! over a plain `&rusqlite::Connection` so they can be unit-tested against an
//! in-memory database without going through the `Arc<Mutex<_>>` wrapper.
//! `Store` itself exists to make that wrapper a non-issue for callers: it is
//! cheap to [`Clone`] and safe to share across the tokio tasks `eva-worker`
//! runs concurrently.
//!
//! `rusqlite::Connection` is blocking. Every [`Store`] method is therefore
//! blocking too — callers on an async runtime should run them inside
//! `tokio::task::spawn_blocking`, the same way any other blocking I/O is
//! handled. This crate does not hide that behind a fake `async fn`, because
//! pretending a mutex-guarded SQLite call is non-blocking would be a lie the
//! scheduler pays for later.

pub mod app_aliases;
pub mod audit;
pub mod command_phrases;
pub mod corrections;
pub mod dictionary;
mod error;
pub mod feedback;
pub mod schema;
pub mod sessions;
pub mod settings;
pub mod tasks;
pub mod transcripts;
pub mod wake_variants;

pub use audit::{AuditRecord, Decision};
pub use error::StoreError;
pub use feedback::Feedback;
pub use sessions::AgentSessionRecord;
pub use tasks::{NewTask, TaskRecord};
pub use transcripts::TranscriptRecord;

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// A cheaply-cloneable handle to the local database.
///
/// Cloning a `Store` clones an `Arc`, not the connection — every clone talks
/// to the same underlying SQLite file (or the same in-memory database).
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (or creates) the store at `path`, running the integrity check
    /// and quarantine-and-recreate fallback described in the crate docs.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = schema::open_checked(path)?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Opens a private, in-memory store. Used by tests and one-off CLI runs
    /// that should not touch disk.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = schema::open_in_memory()?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `f` with exclusive access to the underlying connection. Every
    /// other method on `Store` is a thin wrapper around this — it exists so
    /// this file has exactly one place that locks the mutex and turns a
    /// poison error into a typed [`StoreError`], instead of that logic being
    /// copied into every method.
    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, StoreError>) -> Result<T, StoreError> {
        let guard = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        f(&guard)
    }

    // -- transcripts ---------------------------------------------------

    /// See [`transcripts::save`].
    pub fn save_transcript(&self, raw: &str, pre_formatted: &str, formatted: &str) -> Result<Uuid, StoreError> {
        self.with_conn(|c| transcripts::save(c, raw, pre_formatted, formatted))
    }

    /// See [`transcripts::mark_bad`].
    pub fn mark_transcript_bad(&self, id: Uuid) -> Result<(), StoreError> {
        self.with_conn(|c| transcripts::mark_bad(c, id))
    }

    /// See [`transcripts::recent`].
    pub fn recent_transcripts(&self, limit: u32) -> Result<Vec<TranscriptRecord>, StoreError> {
        self.with_conn(|c| transcripts::recent(c, limit))
    }

    /// See [`transcripts::all_marked_bad`].
    pub fn transcripts_marked_bad(&self) -> Result<Vec<TranscriptRecord>, StoreError> {
        self.with_conn(transcripts::all_marked_bad)
    }

    /// Deletes the transcripts older than `keep_days` days that were not
    /// flagged as wrong; returns how many. `0` keeps everything.
    pub fn prune_transcripts(&self, keep_days: u32) -> Result<usize, StoreError> {
        if keep_days == 0 {
            return Ok(0);
        }
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(keep_days));
        self.with_conn(|c| transcripts::prune_unflagged_before(c, cutoff))
    }

    /// How many transcripts from the last `hours` hours were flagged as wrong.
    pub fn transcripts_marked_bad_in_last(&self, hours: u32) -> Result<u32, StoreError> {
        let since = chrono::Utc::now() - chrono::Duration::hours(i64::from(hours));
        self.with_conn(|c| transcripts::count_marked_bad_since(c, since))
    }

    // -- personal dictionary --------------------------------------------

    /// See [`dictionary::add`].
    pub fn add_custom_word(&self, word: &str) -> Result<(), StoreError> {
        self.with_conn(|c| dictionary::add(c, word))
    }

    /// See [`dictionary::remove`].
    pub fn remove_custom_word(&self, word: &str) -> Result<(), StoreError> {
        self.with_conn(|c| dictionary::remove(c, word))
    }

    /// See [`dictionary::list`].
    pub fn list_custom_words(&self) -> Result<Vec<String>, StoreError> {
        self.with_conn(dictionary::list)
    }

    // -- app aliases ------------------------------------------------------

    /// See [`app_aliases::learn`].
    pub fn learn_app_alias(&self, heard: &str, app: &str) -> Result<(), StoreError> {
        self.with_conn(|c| app_aliases::learn(c, heard, app))
    }

    /// See [`app_aliases::forget`].
    pub fn forget_app_alias(&self, heard: &str) -> Result<(), StoreError> {
        self.with_conn(|c| app_aliases::forget(c, heard))
    }

    /// See [`app_aliases::list`].
    pub fn list_app_aliases(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.with_conn(app_aliases::list)
    }

    // -- learned corrections ------------------------------------------------

    /// See [`corrections::learn`].
    pub fn learn_correction(&self, heard: &str, meant: &str) -> Result<(), StoreError> {
        self.with_conn(|c| corrections::learn(c, heard, meant))
    }

    /// See [`corrections::forget`].
    pub fn forget_correction(&self, heard: &str) -> Result<(), StoreError> {
        self.with_conn(|c| corrections::forget(c, heard))
    }

    /// See [`corrections::list`].
    pub fn list_corrections(&self) -> Result<Vec<(String, String, u32)>, StoreError> {
        self.with_conn(corrections::list)
    }

    // -- feedback on flagged dictations -------------------------------------

    /// See [`feedback::save`].
    pub fn save_feedback(&self, feedback: &Feedback) -> Result<(), StoreError> {
        self.with_conn(|c| feedback::save(c, feedback))
    }

    /// See [`feedback::get`].
    pub fn get_feedback(&self, transcript_id: Uuid) -> Result<Option<Feedback>, StoreError> {
        self.with_conn(|c| feedback::get(c, transcript_id))
    }

    /// See [`feedback::all`].
    pub fn all_feedback(&self) -> Result<Vec<Feedback>, StoreError> {
        self.with_conn(feedback::all)
    }

    // -- wake word variants -----------------------------------------------

    /// See [`command_phrases::learn`].
    pub fn learn_command_phrase(&self, phrase: &str, command: &str, how: &str) -> Result<(), StoreError> {
        self.with_conn(|c| command_phrases::learn(c, phrase, command, how))
    }

    /// See [`command_phrases::forget`].
    pub fn forget_command_phrase(&self, phrase: &str) -> Result<(), StoreError> {
        self.with_conn(|c| command_phrases::forget(c, phrase))
    }

    /// See [`command_phrases::forget_command`].
    pub fn forget_command_phrases_of(&self, command: &str) -> Result<(), StoreError> {
        self.with_conn(|c| command_phrases::forget_command(c, command))
    }

    /// See [`command_phrases::rename_command`].
    pub fn rename_command_phrases(&self, from: &str, to: &str) -> Result<(), StoreError> {
        self.with_conn(|c| command_phrases::rename_command(c, from, to))
    }

    /// See [`command_phrases::list`].
    pub fn list_command_phrases(&self) -> Result<Vec<command_phrases::LearnedPhrase>, StoreError> {
        self.with_conn(command_phrases::list)
    }

    /// See [`wake_variants::count`].
    pub fn count_wake_variant(&self, heard: &str) -> Result<u32, StoreError> {
        self.with_conn(|c| wake_variants::count(c, heard))
    }

    /// See [`wake_variants::teach`].
    pub fn teach_wake_variant(&self, heard: &str, hits: u32) -> Result<(), StoreError> {
        self.with_conn(|c| wake_variants::teach(c, heard, hits))
    }

    /// See [`wake_variants::forget`].
    pub fn forget_wake_variant(&self, heard: &str) -> Result<(), StoreError> {
        self.with_conn(|c| wake_variants::forget(c, heard))
    }

    /// See [`wake_variants::trust`].
    pub fn trust_wake_variant(&self, heard: &str, hits: u32) -> Result<(), StoreError> {
        self.with_conn(|c| wake_variants::trust(c, heard, hits))
    }

    /// See [`wake_variants::trusted`].
    pub fn trusted_wake_variants(&self, min_hits: u32) -> Result<Vec<String>, StoreError> {
        self.with_conn(|c| wake_variants::trusted(c, min_hits))
    }

    // -- settings ---------------------------------------------------------

    /// See [`settings::set`].
    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<(), StoreError> {
        self.with_conn(|c| settings::set(c, key, value))
    }

    /// See [`settings::get`].
    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StoreError> {
        self.with_conn(|c| settings::get(c, key))
    }

    /// See [`settings::remove`].
    pub fn remove_setting(&self, key: &str) -> Result<(), StoreError> {
        self.with_conn(|c| settings::remove(c, key))
    }

    // -- gateway audit trail ----------------------------------------------

    /// See [`audit::log_decision`].
    pub fn log_decision(
        &self,
        transcript_id: Option<Uuid>,
        intent_json: &serde_json::Value,
        decision: Decision,
    ) -> Result<Uuid, StoreError> {
        self.with_conn(|c| audit::log_decision(c, transcript_id, intent_json, decision))
    }

    /// See [`audit::record_result`].
    pub fn record_audit_result(&self, id: Uuid, summary: &str) -> Result<(), StoreError> {
        self.with_conn(|c| audit::record_result(c, id, summary))
    }

    /// See [`audit::recent`].
    pub fn recent_audit(&self, limit: u32) -> Result<Vec<AuditRecord>, StoreError> {
        self.with_conn(|c| audit::recent(c, limit))
    }

    /// See [`audit::find`].
    pub fn find_audit(&self, id: Uuid) -> Result<Option<AuditRecord>, StoreError> {
        self.with_conn(|c| audit::find(c, id))
    }

    // -- agent sessions ("Adán, continúa") ---------------------------------

    /// See [`sessions::save_last_session`].
    pub fn save_last_session(
        &self,
        project_dir: &str,
        provider_id: &str,
        session_id: Uuid,
        work_dir: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_conn(|c| sessions::save_last_session(c, project_dir, provider_id, session_id, work_dir))
    }

    /// See [`sessions::get_last_session`].
    pub fn get_last_session(&self, project_dir: &str) -> Result<Option<AgentSessionRecord>, StoreError> {
        self.with_conn(|c| sessions::get_last_session(c, project_dir))
    }

    // -- agent task history ------------------------------------------------

    /// See [`tasks::start`].
    pub fn start_task(&self, task: NewTask<'_>) -> Result<(), StoreError> {
        self.with_conn(|c| tasks::start(c, task))
    }

    /// See [`tasks::set_provider`].
    pub fn set_task_provider(&self, id: Uuid, provider_id: &str) -> Result<(), StoreError> {
        self.with_conn(|c| tasks::set_provider(c, id, provider_id))
    }

    /// See [`tasks::finish`].
    pub fn finish_task(&self, id: Uuid, success: bool, summary: &str) -> Result<(), StoreError> {
        self.with_conn(|c| tasks::finish(c, id, success, summary))
    }

    /// See [`tasks::mark_unfinished_as_interrupted`].
    pub fn mark_unfinished_tasks_as_interrupted(&self, reason: &str) -> Result<usize, StoreError> {
        self.with_conn(|c| tasks::mark_unfinished_as_interrupted(c, reason))
    }

    /// See [`tasks::recent`].
    pub fn recent_tasks(&self, limit: u32) -> Result<Vec<TaskRecord>, StoreError> {
        self.with_conn(|c| tasks::recent(c, limit))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn store_wraps_every_submodule_correctly() {
        let store = Store::open_in_memory().expect("open_in_memory must succeed");

        let transcript_id = store.save_transcript("eh hola", "hola", "Hola.").expect("save_transcript must succeed");
        assert_eq!(store.recent_transcripts(10).expect("recent must succeed").len(), 1);

        store.mark_transcript_bad(transcript_id).expect("mark_transcript_bad must succeed");
        assert_eq!(store.transcripts_marked_bad().expect("must succeed").len(), 1);

        store.add_custom_word("García").expect("add_custom_word must succeed");
        assert_eq!(store.list_custom_words().expect("must succeed"), vec!["García"]);

        store.set_setting("wake_word", &"Adán").expect("set_setting must succeed");
        let wake_word: Option<String> = store.get_setting("wake_word").expect("get_setting must succeed");
        assert_eq!(wake_word, Some("Adán".to_string()));

        let intent = serde_json::json!({"kind": "open_app"});
        let audit_id = store
            .log_decision(Some(transcript_id), &intent, Decision::AutoApproved)
            .expect("log_decision must succeed");
        store.record_audit_result(audit_id, "opened Brave").expect("record_audit_result must succeed");
        let found = store.find_audit(audit_id).expect("find_audit must succeed").expect("must exist");
        assert_eq!(found.result_summary, Some("opened Brave".to_string()));
    }

    #[test]
    fn cloning_a_store_shares_the_same_underlying_data() {
        let store = Store::open_in_memory().expect("open_in_memory must succeed");
        let cloned = store.clone();

        store.set_setting("shared", &42u32).expect("set_setting must succeed");
        let read_via_clone: Option<u32> = cloned.get_setting("shared").expect("get_setting must succeed");
        assert_eq!(read_via_clone, Some(42));
    }
}
