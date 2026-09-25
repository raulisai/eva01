//! What the user said about a dictation that came out wrong: whether it was
//! a dictation or a command, what they meant, what went wrong and the words
//! that did. The audio and the expected text live in the harvest folder
//! (the eval corpus); this is the part a person wrote, kept so the panel can
//! show it again and the analysis has history to learn from.

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

/// A report on one transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feedback {
    /// The transcript this is about.
    pub transcript_id: Uuid,
    /// `"dictation"` or `"command"`: what the user was doing.
    pub kind: String,
    /// What they meant to get.
    pub intended: String,
    /// JSON array of the causes the user ticked.
    pub causes: String,
    /// Free text.
    pub note: String,
    /// JSON array of the word-level errors, as the panel analysed them.
    pub words: String,
    /// When the first report was made.
    pub created_at: DateTime<Utc>,
    /// When it was last edited.
    pub updated_at: DateTime<Utc>,
}

/// Saves the report, replacing an earlier one about the same transcript
/// (keeping when it was first made).
pub fn save(conn: &Connection, f: &Feedback) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO feedback (transcript_id, kind, intended, causes, note, words, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(transcript_id) DO UPDATE SET kind = excluded.kind, intended = excluded.intended,
            causes = excluded.causes, note = excluded.note, words = excluded.words, updated_at = excluded.updated_at",
        params![f.transcript_id.to_string(), f.kind, f.intended, f.causes, f.note, f.words, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Feedback> {
    let id: String = row.get(0)?;
    let parse_time = |i: usize| -> rusqlite::Result<DateTime<Utc>> {
        let text: String = row.get(i)?;
        DateTime::parse_from_rfc3339(&text)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))
    };
    Ok(Feedback {
        transcript_id: Uuid::parse_str(&id)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        kind: row.get(1)?,
        intended: row.get(2)?,
        causes: row.get(3)?,
        note: row.get(4)?,
        words: row.get(5)?,
        created_at: parse_time(6)?,
        updated_at: parse_time(7)?,
    })
}

const COLUMNS: &str = "transcript_id, kind, intended, causes, note, words, created_at, updated_at";

/// The report about `transcript_id`, if there is one.
pub fn get(conn: &Connection, transcript_id: Uuid) -> Result<Option<Feedback>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM feedback WHERE transcript_id = ?1"))?;
    let mut rows = stmt.query_map(params![transcript_id.to_string()], row)?;
    rows.next().transpose().map_err(StoreError::from)
}

/// Every report, newest edit first.
pub fn all(conn: &Connection) -> Result<Vec<Feedback>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM feedback ORDER BY updated_at DESC"))?;
    let rows = stmt.query_map([], row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::Store;

    fn report(id: Uuid, intended: &str) -> Feedback {
        Feedback {
            transcript_id: id,
            kind: "command".into(),
            intended: intended.into(),
            causes: "[]".into(),
            note: String::new(),
            words: "[]".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_second_report_replaces_the_first_and_keeps_its_date() {
        let store = Store::open_in_memory().unwrap();
        let id = Uuid::new_v4();
        store.save_feedback(&report(id, "abre Brave")).unwrap();
        let first = store.get_feedback(id).unwrap().unwrap();
        store.save_feedback(&report(id, "abre Spotify")).unwrap();
        let second = store.get_feedback(id).unwrap().unwrap();
        assert_eq!(second.intended, "abre Spotify");
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(store.all_feedback().unwrap().len(), 1);
        assert!(store.get_feedback(Uuid::new_v4()).unwrap().is_none());
    }
}
