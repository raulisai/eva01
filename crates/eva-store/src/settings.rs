//! A generic, typed key-value settings store, backed by a `TEXT` column
//! holding JSON. This is the "config file" from `docs/PLAN.md` §10 decision
//! 9 (no settings GUI for the MVP): every setting is `set`/`get` by a string
//! key and any `Serialize`/`Deserialize` value, so adding a new setting never
//! needs a schema migration.

use crate::error::StoreError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Stores `value` under `key`, overwriting whatever was there before.
pub fn set<T: Serialize>(conn: &Connection, key: &str, value: &T) -> Result<(), StoreError> {
    let json = serde_json::to_string(value)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, json],
    )?;
    Ok(())
}

/// Reads the value stored under `key`, or `None` if it was never set.
///
/// # Errors
/// Returns [`StoreError::Json`] if the stored value does not deserialize as `T`
/// — this can only happen if something else wrote an incompatible value under
/// the same key, since [`set`] always round-trips cleanly.
pub fn get<T: DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>, StoreError> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |row| row.get(0))
        .optional()?;

    match raw {
        Some(json) => Ok(Some(serde_json::from_str(&json)?)),
        None => Ok(None),
    }
}

/// Removes a setting. Not an error if it was never set.
pub fn remove(conn: &Connection, key: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::schema::open_in_memory;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct AgentPreferences {
        default_provider: String,
        worktree_by_default: bool,
    }

    #[test]
    fn set_then_get_round_trips_a_struct() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let prefs = AgentPreferences {
            default_provider: "codex".into(),
            worktree_by_default: true,
        };
        set(&conn, "agent_preferences", &prefs).expect("set must succeed");

        let read_back: Option<AgentPreferences> =
            get(&conn, "agent_preferences").expect("get must succeed");
        assert_eq!(read_back, Some(prefs));
    }

    #[test]
    fn get_on_a_missing_key_returns_none_not_an_error() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let read_back: Option<String> = get(&conn, "never_set").expect("get on a missing key must not error");
        assert_eq!(read_back, None);
    }

    #[test]
    fn set_overwrites_an_existing_value() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        set(&conn, "wake_word", &"Adán").expect("first set must succeed");
        set(&conn, "wake_word", &"Eva").expect("second set must succeed");

        let value: Option<String> = get(&conn, "wake_word").expect("get must succeed");
        assert_eq!(value, Some("Eva".to_string()));
    }

    #[test]
    fn remove_deletes_the_key() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        set(&conn, "wake_word", &"Adán").expect("set must succeed");
        remove(&conn, "wake_word").expect("remove must succeed");

        let value: Option<String> = get(&conn, "wake_word").expect("get must succeed");
        assert_eq!(value, None);
    }

    #[test]
    fn simple_scalar_values_round_trip_too() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        set(&conn, "paste_delay_ms", &150u64).expect("set must succeed");
        let value: Option<u64> = get(&conn, "paste_delay_ms").expect("get must succeed");
        assert_eq!(value, Some(150));
    }
}
