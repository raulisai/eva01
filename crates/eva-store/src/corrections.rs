//! Word-level "heard → meant" pairs the user taught by reporting a dictation
//! that came out wrong ("todo eso" was "todo esto"). The worker applies them
//! to every later dictation and command, so a mistake reported once is not
//! made twice.

use crate::error::StoreError;
use rusqlite::{params, Connection};

/// Remembers that `heard` (folded, lowercase) should come out as `meant`.
/// Teaching the same pair again counts one more hit; a different `meant`
/// replaces the old one.
pub fn learn(conn: &Connection, heard: &str, meant: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO corrections (heard, meant, hits) VALUES (?1, ?2, 1)
         ON CONFLICT(heard) DO UPDATE SET
            hits = CASE WHEN meant = excluded.meant THEN hits + 1 ELSE 1 END,
            meant = excluded.meant",
        params![heard, meant],
    )?;
    Ok(())
}

/// Forgets what `heard` was corrected to. Not an error if there was nothing.
pub fn forget(conn: &Connection, heard: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM corrections WHERE heard = ?1", params![heard])?;
    Ok(())
}

/// Every `(heard, meant, hits)` learned, most confirmed first.
pub fn list(conn: &Connection) -> Result<Vec<(String, String, u32)>, StoreError> {
    let mut stmt = conn.prepare("SELECT heard, meant, hits FROM corrections ORDER BY hits DESC, rowid ASC")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use crate::Store;

    #[test]
    fn a_pair_counts_confirmations_is_replaced_and_forgotten() {
        let store = Store::open_in_memory().unwrap();
        store.learn_correction("esto es", "esto es").unwrap();
        store.learn_correction("adam", "Adán").unwrap();
        store.learn_correction("adam", "Adán").unwrap();
        assert_eq!(store.list_corrections().unwrap()[0], ("adam".to_string(), "Adán".to_string(), 2));
        store.learn_correction("adam", "Adam").unwrap();
        assert_eq!(store.list_corrections().unwrap().iter().find(|c| c.0 == "adam").unwrap().2, 1);
        store.forget_correction("adam").unwrap();
        assert_eq!(store.list_corrections().unwrap().len(), 1);
    }
}
