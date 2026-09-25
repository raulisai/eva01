//! The other ways the user says their own commands: each phrase taught in the
//! panel or confirmed with a "sí" is remembered against the command it means,
//! so it is as exact as the phrase they wrote from then on.

use crate::error::StoreError;
use rusqlite::{params, Connection};

/// One remembered way of saying a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedPhrase {
    /// What is said after the wake word, without accents or capitals.
    pub phrase: String,
    /// The command it means, by its main phrase (`say`).
    pub command: String,
    /// `"taught"` (said in the panel) or `"confirmed"` (a "sí" to a question).
    pub how: String,
    /// When it was learned, RFC 3339.
    pub learned_at: String,
}

/// Remembers that `phrase` means `command`, replacing what it meant before.
pub fn learn(conn: &Connection, phrase: &str, command: &str, how: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO command_phrases (phrase, command, how) VALUES (?1, ?2, ?3)
         ON CONFLICT(phrase) DO UPDATE SET command = excluded.command, how = excluded.how",
        params![phrase, command, how],
    )?;
    Ok(())
}

/// Forgets `phrase`. Not an error if it meant nothing.
pub fn forget(conn: &Connection, phrase: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM command_phrases WHERE phrase = ?1", params![phrase])?;
    Ok(())
}

/// Forgets every phrase of `command` (when the command itself is deleted).
pub fn forget_command(conn: &Connection, command: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM command_phrases WHERE command = ?1", params![command])?;
    Ok(())
}

/// Points every phrase of `from` at `to` (when a command is renamed).
pub fn rename_command(conn: &Connection, from: &str, to: &str) -> Result<(), StoreError> {
    conn.execute("UPDATE command_phrases SET command = ?2 WHERE command = ?1", params![from, to])?;
    Ok(())
}

/// Every phrase remembered, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<LearnedPhrase>, StoreError> {
    let mut stmt = conn.prepare("SELECT phrase, command, how, learned_at FROM command_phrases ORDER BY rowid ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok(LearnedPhrase { phrase: row.get(0)?, command: row.get(1)?, how: row.get(2)?, learned_at: row.get(3)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use crate::Store;

    #[test]
    fn phrases_are_learned_replaced_listed_and_forgotten() {
        let store = Store::open_in_memory().unwrap();
        store.learn_command_phrase("ponme mi canal", "ver mi canal", "confirmed").unwrap();
        store.learn_command_phrase("mi canal", "ver mi canal", "taught").unwrap();
        store.learn_command_phrase("mi canal", "otra cosa", "taught").unwrap();

        let all = store.list_command_phrases().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].phrase.as_str(), all[0].how.as_str()), ("ponme mi canal", "confirmed"));
        assert_eq!(all[1].command, "otra cosa", "a phrase means one thing: the last it was taught");
        assert!(!all[1].learned_at.is_empty());

        store.forget_command_phrase("ponme mi canal").unwrap();
        assert_eq!(store.list_command_phrases().unwrap().len(), 1);
        store.rename_command_phrases("otra cosa", "la de siempre").unwrap();
        assert_eq!(store.list_command_phrases().unwrap()[0].command, "la de siempre");
        store.forget_command_phrases_of("la de siempre").unwrap();
        assert!(store.list_command_phrases().unwrap().is_empty());
    }
}
