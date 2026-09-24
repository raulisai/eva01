//! How the user says the apps they have: a mishearing or a habit ("spotifi")
//! confirmed once as an app, remembered so the next one needs no question.

use crate::error::StoreError;
use rusqlite::{params, Connection};

/// Remembers that `heard` means `app`, replacing what it meant before.
pub fn learn(conn: &Connection, heard: &str, app: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO app_aliases (heard, app) VALUES (?1, ?2) ON CONFLICT(heard) DO UPDATE SET app = excluded.app",
        params![heard, app],
    )?;
    Ok(())
}

/// Forgets what `heard` meant. Not an error if it meant nothing.
pub fn forget(conn: &Connection, heard: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM app_aliases WHERE heard = ?1", params![heard])?;
    Ok(())
}

/// Every `(heard, app)` remembered, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = conn.prepare("SELECT heard, app FROM app_aliases ORDER BY rowid ASC")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use crate::Store;

    #[test]
    fn what_was_learned_is_listed_replaced_and_forgotten() {
        let store = Store::open_in_memory().unwrap();
        store.learn_app_alias("spotifi", "Spotify").unwrap();
        store.learn_app_alias("brave", "Brave Browser").unwrap();
        store.learn_app_alias("spotifi", "Spotify Lite").unwrap();
        assert_eq!(
            store.list_app_aliases().unwrap(),
            vec![
                ("spotifi".to_string(), "Spotify Lite".to_string()),
                ("brave".to_string(), "Brave Browser".to_string())
            ]
        );
        store.forget_app_alias("spotifi").unwrap();
        assert_eq!(store.list_app_aliases().unwrap().len(), 1);
    }
}
