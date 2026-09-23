//! The corpus-harvesting table from `docs/PLAN.md` fase 3: every dictation
//! saves its raw, dictionary-corrected, and formatted forms, and a "this was
//! wrong" hotkey flips [`TranscriptRecord::marked_bad`] — building the eval
//! corpus for free during ordinary use instead of a separate recording
//! session.

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

/// One saved transcript, at every stage of the text pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRecord {
    /// Unique id, generated when the transcript is saved.
    pub id: Uuid,
    /// When it was saved.
    pub created_at: DateTime<Utc>,
    /// Exactly what the STT engine produced.
    pub raw: String,
    /// After filler removal and dictionary correction, before formatting.
    pub pre_formatted: String,
    /// The final, pasted text.
    pub formatted: String,
    /// Set by the "this was wrong" hotkey; marks this row as an eval failure case.
    pub marked_bad: bool,
}

/// Saves a new transcript and returns its generated id.
pub fn save(conn: &Connection, raw: &str, pre_formatted: &str, formatted: &str) -> Result<Uuid, StoreError> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    conn.execute(
        "INSERT INTO transcripts (id, created_at, raw, pre_formatted, formatted, marked_bad)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id.to_string(), now.to_rfc3339(), raw, pre_formatted, formatted],
    )?;
    Ok(id)
}

/// Flags a transcript as a failure case for the eval corpus.
///
/// # Errors
/// Returns [`StoreError::NotFound`] if `id` does not match any saved transcript.
pub fn mark_bad(conn: &Connection, id: Uuid) -> Result<(), StoreError> {
    let updated = conn.execute("UPDATE transcripts SET marked_bad = 1 WHERE id = ?1", params![id.to_string()])?;
    if updated == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Returns the `limit` most recently saved transcripts, newest first.
pub fn recent(conn: &Connection, limit: u32) -> Result<Vec<TranscriptRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, created_at, raw, pre_formatted, formatted, marked_bad
         FROM transcripts ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], row_to_record)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

/// Returns every transcript ever marked bad, oldest first — the eval corpus's
/// "known failure" set.
pub fn all_marked_bad(conn: &Connection) -> Result<Vec<TranscriptRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, created_at, raw, pre_formatted, formatted, marked_bad
         FROM transcripts WHERE marked_bad = 1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_record)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

/// Deletes the transcripts saved before `cutoff` that nobody flagged as wrong
/// (those are the eval corpus and are kept for good). Returns how many went.
pub fn prune_unflagged_before(conn: &Connection, cutoff: DateTime<Utc>) -> Result<usize, StoreError> {
    let deleted =
        conn.execute("DELETE FROM transcripts WHERE marked_bad = 0 AND created_at < ?1", params![cutoff.to_rfc3339()])?;
    Ok(deleted)
}

/// How many transcripts were flagged as wrong since `since` — the "retrabajos"
/// number of `docs/PLAN.md` §7 (the target is fewer than five a day).
///
/// A flagged row keeps its original `created_at`, which is what this counts
/// by: the day the dictation happened, the moment it was wrong.
pub fn count_marked_bad_since(conn: &Connection, since: DateTime<Utc>) -> Result<u32, StoreError> {
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM transcripts WHERE marked_bad = 1 AND created_at >= ?1",
        params![since.to_rfc3339()],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptRecord> {
    let id_text: String = row.get(0)?;
    let created_at_text: String = row.get(1)?;
    Ok(TranscriptRecord {
        id: parse_uuid_column(&id_text, 0)?,
        created_at: parse_datetime_column(&created_at_text, 1)?,
        raw: row.get(2)?,
        pre_formatted: row.get(3)?,
        formatted: row.get(4)?,
        marked_bad: row.get::<_, i64>(5)? != 0,
    })
}

fn parse_uuid_column(text: &str, col: usize) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(text)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_datetime_column(text: &str, col: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::schema::open_in_memory;

    #[test]
    fn save_and_read_back_a_transcript() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let id = save(&conn, "eh hola mundo", "hola mundo", "Hola mundo.").expect("save must succeed");

        let recent = recent(&conn, 10).expect("recent must succeed");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, id);
        assert_eq!(recent[0].raw, "eh hola mundo");
        assert_eq!(recent[0].formatted, "Hola mundo.");
        assert!(!recent[0].marked_bad);
    }

    #[test]
    fn recent_orders_newest_first_and_respects_the_limit() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        for i in 0..5 {
            save(&conn, &format!("raw {i}"), &format!("pre {i}"), &format!("Formatted {i}."))
                .expect("save must succeed");
        }

        let recent = recent(&conn, 3).expect("recent must succeed");
        assert_eq!(recent.len(), 3);
        // Newest first: the last one saved ("raw 4") comes back first.
        assert_eq!(recent[0].raw, "raw 4");
        assert_eq!(recent[2].raw, "raw 2");
    }

    #[test]
    fn mark_bad_flips_the_flag_and_is_reflected_in_recent_and_all_marked_bad() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let id = save(&conn, "raw", "pre", "Formatted.").expect("save must succeed");

        mark_bad(&conn, id).expect("mark_bad on an existing row must succeed");

        let recent = recent(&conn, 10).expect("recent must succeed");
        assert!(recent[0].marked_bad);

        let bad = all_marked_bad(&conn).expect("all_marked_bad must succeed");
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].id, id);
    }

    fn save_at(conn: &Connection, raw: &str, when: DateTime<Utc>, bad: bool) -> Uuid {
        let id = save(conn, raw, raw, raw).expect("save");
        conn.execute(
            "UPDATE transcripts SET created_at = ?1, marked_bad = ?2 WHERE id = ?3",
            params![when.to_rfc3339(), i64::from(bad), id.to_string()],
        )
        .expect("backdate");
        id
    }

    #[test]
    fn pruning_deletes_old_transcripts_but_never_the_flagged_ones() {
        let conn = open_in_memory().expect("open");
        let now = Utc::now();
        let old = now - chrono::Duration::days(40);
        save_at(&conn, "vieja", old, false);
        let kept_flagged = save_at(&conn, "vieja marcada", old, true);
        save_at(&conn, "reciente", now - chrono::Duration::days(2), false);

        let deleted = prune_unflagged_before(&conn, now - chrono::Duration::days(30)).expect("prune");

        assert_eq!(deleted, 1);
        let left: Vec<String> = recent(&conn, 10).expect("recent").into_iter().map(|r| r.raw).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"reciente".to_string()));
        assert!(all_marked_bad(&conn).expect("bad").iter().any(|r| r.id == kept_flagged));
    }

    #[test]
    fn the_rework_count_only_includes_flagged_transcripts_from_the_window() {
        let conn = open_in_memory().expect("open");
        let now = Utc::now();
        save_at(&conn, "hoy mal", now - chrono::Duration::hours(1), true);
        save_at(&conn, "hoy bien", now - chrono::Duration::hours(2), false);
        save_at(&conn, "ayer mal", now - chrono::Duration::hours(30), true);

        assert_eq!(count_marked_bad_since(&conn, now - chrono::Duration::hours(24)).expect("count"), 1);
        assert_eq!(count_marked_bad_since(&conn, now - chrono::Duration::hours(48)).expect("count"), 2);
    }

    #[test]
    fn mark_bad_on_an_unknown_id_returns_not_found() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let result = mark_bad(&conn, Uuid::new_v4());
        assert!(matches!(result, Err(StoreError::NotFound(_))));
    }
}
