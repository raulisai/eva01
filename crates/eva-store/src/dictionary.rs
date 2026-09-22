//! Persists the personal dictionary words a user adds, so they survive a
//! restart. This is storage only — the actual fuzzy correction lives in
//! `eva-text::Dictionary`, built from the list this module returns.

use crate::error::StoreError;
use rusqlite::{params, Connection};

/// Adds a word to the persisted dictionary. Idempotent: adding the same word
/// twice is a no-op, not an error.
pub fn add(conn: &Connection, word: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO custom_words (word) VALUES (?1) ON CONFLICT(word) DO NOTHING",
        params![word],
    )?;
    Ok(())
}

/// Removes a word from the persisted dictionary. Not an error if it was
/// never there.
pub fn remove(conn: &Connection, word: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM custom_words WHERE word = ?1", params![word])?;
    Ok(())
}

/// Returns every word in the persisted dictionary, in insertion order.
pub fn list(conn: &Connection) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare("SELECT word FROM custom_words ORDER BY rowid ASC")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::schema::open_in_memory;

    #[test]
    fn add_and_list_words_in_insertion_order() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        add(&conn, "García").expect("add must succeed");
        add(&conn, "Núñez").expect("add must succeed");

        assert_eq!(list(&conn).expect("list must succeed"), vec!["García", "Núñez"]);
    }

    #[test]
    fn adding_the_same_word_twice_is_not_an_error_and_does_not_duplicate() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        add(&conn, "García").expect("first add must succeed");
        add(&conn, "García").expect("second add of the same word must not error");

        assert_eq!(list(&conn).expect("list must succeed"), vec!["García"]);
    }

    #[test]
    fn remove_deletes_the_word() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        add(&conn, "García").expect("add must succeed");
        remove(&conn, "García").expect("remove must succeed");

        assert!(list(&conn).expect("list must succeed").is_empty());
    }

    #[test]
    fn removing_a_word_that_was_never_added_is_not_an_error() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        remove(&conn, "no-existe").expect("removing an absent word must not error");
    }
}
