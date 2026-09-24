//! How this user's speech comes out for the wake word: each spelling that
//! was taken for it, and how many times — the score that, past a threshold,
//! makes it trusted for anything ("adam" is "Adán" for this person).

use crate::error::StoreError;
use rusqlite::{params, Connection};

/// Counts one more time that `heard` was taken for the wake word and
/// returns how many times that is now.
pub fn count(conn: &Connection, heard: &str) -> Result<u32, StoreError> {
    conn.execute(
        "INSERT INTO wake_variants (heard, hits) VALUES (?1, 1) ON CONFLICT(heard) DO UPDATE SET hits = hits + 1",
        params![heard],
    )?;
    Ok(conn.query_row("SELECT hits FROM wake_variants WHERE heard = ?1", params![heard], |row| row.get(0))?)
}

/// The spellings counted at least `min_hits` times.
pub fn trusted(conn: &Connection, min_hits: u32) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare("SELECT heard FROM wake_variants WHERE hits >= ?1 ORDER BY hits DESC")?;
    let rows = stmt.query_map(params![min_hits], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use crate::Store;

    #[test]
    fn a_spelling_becomes_trusted_only_after_enough_hits() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.count_wake_variant("adam").unwrap(), 1);
        assert!(store.trusted_wake_variants(2).unwrap().is_empty());
        assert_eq!(store.count_wake_variant("adam").unwrap(), 2);
        store.count_wake_variant("agan").unwrap();
        assert_eq!(store.trusted_wake_variants(2).unwrap(), vec!["adam".to_string()]);
    }
}
