//! The gateway audit trail from `docs/PLAN.md` fase 5: every intent the
//! gateway decided on — auto-approved, confirmed by the user, or blocked —
//! is logged with what was asked, what was decided, and what happened.

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

/// What the gateway decided to do with an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Executed without asking — the action's policy is `auto`.
    AutoApproved,
    /// The user confirmed by click or hotkey before it ran.
    UserConfirmed,
    /// The user declined the confirmation prompt.
    UserRejected,
    /// The gateway refused outright (a blocklisted destructive verb, for example).
    Blocked,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Decision::AutoApproved => "auto_approved",
            Decision::UserConfirmed => "user_confirmed",
            Decision::UserRejected => "user_rejected",
            Decision::Blocked => "blocked",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "auto_approved" => Some(Decision::AutoApproved),
            "user_confirmed" => Some(Decision::UserConfirmed),
            "user_rejected" => Some(Decision::UserRejected),
            "blocked" => Some(Decision::Blocked),
            _ => None,
        }
    }
}

/// One row of the audit trail.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRecord {
    /// Unique id of this audit entry.
    pub id: Uuid,
    /// When the decision was made.
    pub created_at: DateTime<Utc>,
    /// The transcript this intent came from, if it originated from dictation.
    pub transcript_id: Option<Uuid>,
    /// The parsed intent, as JSON (produced by `eva-intent`).
    pub intent_json: serde_json::Value,
    /// What the gateway decided.
    pub decision: Decision,
    /// A short human-readable summary of what happened after the decision
    /// (e.g. `"opened Brave Browser"`, `"agent task started"`), filled in
    /// after execution via [`record_result`].
    pub result_summary: Option<String>,
}

/// Logs a new gateway decision and returns the audit entry's id.
pub fn log_decision(
    conn: &Connection,
    transcript_id: Option<Uuid>,
    intent_json: &serde_json::Value,
    decision: Decision,
) -> Result<Uuid, StoreError> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    conn.execute(
        "INSERT INTO audit_log (id, created_at, transcript_id, intent_json, decision, result_summary)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
        params![
            id.to_string(),
            now.to_rfc3339(),
            transcript_id.map(|u| u.to_string()),
            intent_json.to_string(),
            decision.as_str(),
        ],
    )?;
    Ok(id)
}

/// Fills in what happened after a logged decision was acted on.
///
/// # Errors
/// Returns [`StoreError::NotFound`] if `id` does not match a logged entry.
pub fn record_result(conn: &Connection, id: Uuid, summary: &str) -> Result<(), StoreError> {
    let updated =
        conn.execute("UPDATE audit_log SET result_summary = ?1 WHERE id = ?2", params![summary, id.to_string()])?;
    if updated == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Returns the `limit` most recent audit entries, newest first.
pub fn recent(conn: &Connection, limit: u32) -> Result<Vec<AuditRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, created_at, transcript_id, intent_json, decision, result_summary
         FROM audit_log ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], row_to_record)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

/// Looks up a single audit entry by id, or `None` if it does not exist.
pub fn find(conn: &Connection, id: Uuid) -> Result<Option<AuditRecord>, StoreError> {
    conn.query_row(
        "SELECT id, created_at, transcript_id, intent_json, decision, result_summary
         FROM audit_log WHERE id = ?1",
        params![id.to_string()],
        row_to_record,
    )
    .optional()
    .map_err(StoreError::from)
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRecord> {
    let id_text: String = row.get(0)?;
    let created_at_text: String = row.get(1)?;
    let transcript_id_text: Option<String> = row.get(2)?;
    let intent_text: String = row.get(3)?;
    let decision_text: String = row.get(4)?;

    Ok(AuditRecord {
        id: parse_uuid(&id_text, 0)?,
        created_at: parse_datetime(&created_at_text, 1)?,
        transcript_id: transcript_id_text.map(|t| parse_uuid(&t, 2)).transpose()?,
        intent_json: serde_json::from_str(&intent_text)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?,
        decision: Decision::from_str(&decision_text).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown decision variant: {decision_text}").into(),
            )
        })?,
        result_summary: row.get(5)?,
    })
}

fn parse_uuid(text: &str, col: usize) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(text)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_datetime(text: &str, col: usize) -> rusqlite::Result<DateTime<Utc>> {
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
    fn logs_and_reads_back_a_decision() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let intent = serde_json::json!({"kind": "open_app", "target": "Brave"});
        let id = log_decision(&conn, None, &intent, Decision::AutoApproved).expect("log must succeed");

        let found = find(&conn, id).expect("find must succeed").expect("must exist");
        assert_eq!(found.intent_json, intent);
        assert_eq!(found.decision, Decision::AutoApproved);
        assert_eq!(found.result_summary, None);
    }

    #[test]
    fn record_result_fills_in_the_summary() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let intent = serde_json::json!({"kind": "open_app", "target": "Brave"});
        let id = log_decision(&conn, None, &intent, Decision::AutoApproved).expect("log must succeed");

        record_result(&conn, id, "opened Brave Browser").expect("record_result must succeed");

        let found = find(&conn, id).expect("find must succeed").expect("must exist");
        assert_eq!(found.result_summary, Some("opened Brave Browser".to_string()));
    }

    #[test]
    fn record_result_on_an_unknown_id_returns_not_found() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let result = record_result(&conn, Uuid::new_v4(), "nope");
        assert!(matches!(result, Err(StoreError::NotFound(_))));
    }

    #[test]
    fn recent_orders_newest_first() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        for i in 0..3 {
            let intent = serde_json::json!({"i": i});
            log_decision(&conn, None, &intent, Decision::Blocked).expect("log must succeed");
        }

        let recent = recent(&conn, 10).expect("recent must succeed");
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].intent_json, serde_json::json!({"i": 2}));
    }

    #[test]
    fn links_to_a_transcript_when_one_is_given() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let transcript_id = crate::transcripts::save(&conn, "raw", "pre", "Formatted.").expect("save must succeed");
        let intent = serde_json::json!({"kind": "dictation"});
        let id = log_decision(&conn, Some(transcript_id), &intent, Decision::AutoApproved).expect("log must succeed");

        let found = find(&conn, id).expect("find must succeed").expect("must exist");
        assert_eq!(found.transcript_id, Some(transcript_id));
    }

    #[test]
    fn find_on_an_unknown_id_returns_none() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        assert_eq!(find(&conn, Uuid::new_v4()).expect("find must succeed"), None);
    }
}
