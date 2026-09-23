//! The "Adán, continúa" session table from `docs/PLAN.md` fase 7: which
//! agent provider and session id last ran in a given project, so a bare
//! "continúa" can resume it without the user naming either.

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

/// The most recent agent session run in a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRecord {
    /// The provider that ran it ("codex", "claude_code").
    pub provider_id: String,
    /// The session id `eva-agents` assigned when it started.
    pub session_id: Uuid,
    /// The directory the session actually ran in — a worktree, when one
    /// was used — which "continúa" must resume from, because the CLIs key
    /// their sessions to the working directory. `None` for rows written
    /// before this column existed, meaning "the project directory itself".
    pub work_dir: Option<String>,
    /// When this record was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Records that `provider_id`'s `session_id` is now the most recent session
/// for `project_dir`, overwriting whatever was there before — "continúa"
/// always means the *last* thing you asked for in this project, not a
/// history to pick from.
pub fn save_last_session(
    conn: &Connection,
    project_dir: &str,
    provider_id: &str,
    session_id: Uuid,
    work_dir: Option<&str>,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO agent_sessions (project_dir, provider_id, session_id, work_dir, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_dir) DO UPDATE SET
             provider_id = excluded.provider_id,
             session_id = excluded.session_id,
             work_dir = excluded.work_dir,
             updated_at = excluded.updated_at",
        params![project_dir, provider_id, session_id.to_string(), work_dir, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Looks up the most recent session for `project_dir`, or `None` if nothing
/// has ever run there.
pub fn get_last_session(conn: &Connection, project_dir: &str) -> Result<Option<AgentSessionRecord>, StoreError> {
    conn.query_row(
        "SELECT provider_id, session_id, updated_at, work_dir FROM agent_sessions WHERE project_dir = ?1",
        params![project_dir],
        |row| {
            let provider_id: String = row.get(0)?;
            let session_id_text: String = row.get(1)?;
            let updated_at_text: String = row.get(2)?;
            let work_dir: Option<String> = row.get(3)?;
            Ok((provider_id, session_id_text, updated_at_text, work_dir))
        },
    )
    .optional()?
    .map(|(provider_id, session_id_text, updated_at_text, work_dir)| {
        let session_id = Uuid::parse_str(&session_id_text).map_err(|e| {
            StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))
        })?;
        let updated_at =
            DateTime::parse_from_rfc3339(&updated_at_text).map(|dt| dt.with_timezone(&Utc)).map_err(|e| {
                StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))
            })?;
        Ok(AgentSessionRecord { provider_id, session_id, work_dir, updated_at })
    })
    .transpose()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::schema::open_in_memory;

    #[test]
    fn saves_and_reads_back_the_last_session_for_a_project() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let session_id = Uuid::new_v4();

        save_last_session(&conn, "/repos/iam", "codex", session_id, None).expect("save must succeed");

        let record = get_last_session(&conn, "/repos/iam").expect("get must succeed").expect("must exist");
        assert_eq!(record.provider_id, "codex");
        assert_eq!(record.session_id, session_id);
    }

    #[test]
    fn a_project_with_no_prior_session_returns_none() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        assert_eq!(get_last_session(&conn, "/repos/nunca-usado").expect("must succeed"), None);
    }

    #[test]
    fn saving_a_new_session_overwrites_the_previous_one_for_the_same_project() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        save_last_session(&conn, "/repos/iam", "codex", first, None).expect("save must succeed");
        save_last_session(&conn, "/repos/iam", "claude_code", second, None).expect("save must succeed");

        let record = get_last_session(&conn, "/repos/iam").expect("get must succeed").expect("must exist");
        assert_eq!(record.provider_id, "claude_code");
        assert_eq!(record.session_id, second);
    }

    #[test]
    fn different_projects_keep_independent_sessions() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();

        save_last_session(&conn, "/repos/a", "codex", session_a, None).expect("save must succeed");
        save_last_session(&conn, "/repos/b", "claude_code", session_b, None).expect("save must succeed");

        assert_eq!(get_last_session(&conn, "/repos/a").expect("must succeed").map(|r| r.session_id), Some(session_a));
        assert_eq!(get_last_session(&conn, "/repos/b").expect("must succeed").map(|r| r.session_id), Some(session_b));
    }

    #[test]
    fn the_work_dir_a_session_ran_in_round_trips() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        save_last_session(&conn, "/repos/iam", "claude_code", Uuid::new_v4(), Some("/wt/iam-1a2b"))
            .expect("save must succeed");
        let record = get_last_session(&conn, "/repos/iam").expect("get must succeed").expect("must exist");
        assert_eq!(record.work_dir.as_deref(), Some("/wt/iam-1a2b"));
    }
}
