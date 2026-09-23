//! EVA's own record of every agent task it dispatched — the panel's data
//! source (`docs/PLAN.md` fase 7: "un registro propio en `eva-store` para
//! Codex", whose CLI has no `agents --json` of its own) and the way a task
//! that was running when the worker crashed is still visible afterwards,
//! marked as interrupted instead of hanging around as "running" forever.

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

/// One dispatched agent task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    /// EVA's id for the task (the request that started it).
    pub id: Uuid,
    /// Which agent ran it (`"codex"`, `"claude_code"`).
    pub provider_id: String,
    /// What the agent was asked to do.
    pub prompt: String,
    /// The project the user was working in.
    pub project_dir: String,
    /// The directory the agent actually ran in (a worktree, when used).
    pub work_dir: Option<String>,
    /// The branch created for the task's worktree, when one was used.
    pub branch: Option<String>,
    /// When the task started.
    pub started_at: DateTime<Utc>,
    /// When it ended; `None` while it is still running.
    pub finished_at: Option<DateTime<Utc>>,
    /// Whether it succeeded; `None` while it is still running.
    pub success: Option<bool>,
    /// The agent's own summary, or the failure reason.
    pub summary: Option<String>,
}

/// The fields known when a task starts.
#[derive(Debug, Clone, Copy)]
pub struct NewTask<'a> {
    /// EVA's id for the task.
    pub id: Uuid,
    /// Which agent runs it.
    pub provider_id: &'a str,
    /// What it was asked to do.
    pub prompt: &'a str,
    /// The project the user was working in.
    pub project_dir: &'a str,
    /// The directory the agent runs in.
    pub work_dir: Option<&'a str>,
    /// The worktree branch, if any.
    pub branch: Option<&'a str>,
}

/// Records that a task started.
pub fn start(conn: &Connection, task: NewTask<'_>) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO agent_tasks (id, provider_id, prompt, project_dir, work_dir, branch, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            task.id.to_string(),
            task.provider_id,
            task.prompt,
            task.project_dir,
            task.work_dir,
            task.branch,
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

/// Records that a task moved to another agent (the first one failed before
/// doing any work, so the next took over).
pub fn set_provider(conn: &Connection, id: Uuid, provider_id: &str) -> Result<(), StoreError> {
    let changed =
        conn.execute("UPDATE agent_tasks SET provider_id = ?2 WHERE id = ?1", params![id.to_string(), provider_id])?;
    if changed == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Records how a task ended.
///
/// # Errors
/// [`StoreError::NotFound`] if no task has that id.
pub fn finish(conn: &Connection, id: Uuid, success: bool, summary: &str) -> Result<(), StoreError> {
    let changed = conn.execute(
        "UPDATE agent_tasks SET finished_at = ?2, success = ?3, summary = ?4 WHERE id = ?1",
        params![id.to_string(), Utc::now().to_rfc3339(), success, summary],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Marks every task that never got a `finish` as failed with `reason` — run
/// once at worker startup, when any such task can only be one whose worker
/// died mid-run. Returns how many were marked.
pub fn mark_unfinished_as_interrupted(conn: &Connection, reason: &str) -> Result<usize, StoreError> {
    let changed = conn.execute(
        "UPDATE agent_tasks SET finished_at = ?1, success = 0, summary = ?2 WHERE finished_at IS NULL",
        params![Utc::now().to_rfc3339(), reason],
    )?;
    Ok(changed)
}

/// The most recent tasks, newest first.
pub fn recent(conn: &Connection, limit: u32) -> Result<Vec<TaskRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, provider_id, prompt, project_dir, work_dir, branch, started_at, finished_at, success, summary
         FROM agent_tasks ORDER BY started_at DESC, rowid DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<bool>>(8)?,
            row.get::<_, Option<String>>(9)?,
        ))
    })?;

    let mut records = Vec::new();
    for row in rows {
        let (id, provider_id, prompt, project_dir, work_dir, branch, started, finished, success, summary) = row?;
        records.push(TaskRecord {
            id: parse_uuid(&id, 0)?,
            provider_id,
            prompt,
            project_dir,
            work_dir,
            branch,
            started_at: parse_time(&started, 6)?,
            finished_at: finished.as_deref().map(|t| parse_time(t, 7)).transpose()?,
            success,
            summary,
        });
    }
    Ok(records)
}

fn parse_uuid(text: &str, column: usize) -> Result<Uuid, StoreError> {
    Uuid::parse_str(text).map_err(|e| {
        StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
    })
}

fn parse_time(text: &str, column: usize) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(text).map(|dt| dt.with_timezone(&Utc)).map_err(|e| {
        StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::schema::open_in_memory;

    fn new_task(id: Uuid, prompt: &str) -> NewTask<'_> {
        NewTask { id, provider_id: "codex", prompt, project_dir: "/repos/iam", work_dir: None, branch: None }
    }

    #[test]
    fn a_started_task_is_listed_as_running() {
        let conn = open_in_memory().expect("open");
        let id = Uuid::new_v4();
        start(&conn, new_task(id, "agrega tests")).expect("start");

        let tasks = recent(&conn, 10).expect("recent");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);
        assert_eq!(tasks[0].finished_at, None);
        assert_eq!(tasks[0].success, None);
    }

    #[test]
    fn finishing_records_success_and_summary() {
        let conn = open_in_memory().expect("open");
        let id = Uuid::new_v4();
        start(&conn, new_task(id, "agrega tests")).expect("start");
        finish(&conn, id, true, "3 archivos").expect("finish");

        let task = &recent(&conn, 10).expect("recent")[0];
        assert_eq!(task.success, Some(true));
        assert_eq!(task.summary.as_deref(), Some("3 archivos"));
        assert!(task.finished_at.is_some());
    }

    #[test]
    fn a_task_can_move_to_another_provider() {
        let conn = open_in_memory().expect("open");
        let id = Uuid::new_v4();
        start(&conn, new_task(id, "x")).expect("start");
        set_provider(&conn, id, "claude_code").expect("set");
        assert_eq!(recent(&conn, 1).expect("recent")[0].provider_id, "claude_code");
        assert!(matches!(set_provider(&conn, Uuid::new_v4(), "codex"), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn finishing_an_unknown_task_is_not_found() {
        let conn = open_in_memory().expect("open");
        assert!(matches!(finish(&conn, Uuid::new_v4(), true, "x"), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn unfinished_tasks_are_marked_interrupted_and_finished_ones_are_left_alone() {
        let conn = open_in_memory().expect("open");
        let running = Uuid::new_v4();
        let done = Uuid::new_v4();
        start(&conn, new_task(running, "una")).expect("start");
        start(&conn, new_task(done, "otra")).expect("start");
        finish(&conn, done, true, "ok").expect("finish");

        assert_eq!(mark_unfinished_as_interrupted(&conn, "interrumpida").expect("mark"), 1);

        let tasks = recent(&conn, 10).expect("recent");
        let interrupted = tasks.iter().find(|t| t.id == running).expect("present");
        assert_eq!(interrupted.success, Some(false));
        assert_eq!(interrupted.summary.as_deref(), Some("interrumpida"));
        let untouched = tasks.iter().find(|t| t.id == done).expect("present");
        assert_eq!(untouched.summary.as_deref(), Some("ok"));
    }

    #[test]
    fn recent_is_newest_first_and_respects_the_limit() {
        let conn = open_in_memory().expect("open");
        for prompt in ["a", "b", "c"] {
            start(&conn, new_task(Uuid::new_v4(), prompt)).expect("start");
        }
        let tasks = recent(&conn, 2).expect("recent");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].prompt, "c");
    }

    #[test]
    fn work_dir_and_branch_round_trip() {
        let conn = open_in_memory().expect("open");
        let id = Uuid::new_v4();
        start(
            &conn,
            NewTask { id, provider_id: "claude_code", prompt: "x", project_dir: "/p", work_dir: Some("/wt/p-1"), branch: Some("eva/1") },
        )
        .expect("start");
        let task = &recent(&conn, 1).expect("recent")[0];
        assert_eq!(task.work_dir.as_deref(), Some("/wt/p-1"));
        assert_eq!(task.branch.as_deref(), Some("eva/1"));
    }
}
