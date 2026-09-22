//! Errors this crate can return. Every fallible operation in `eva-store`
//! returns a `Result<_, StoreError>` — see `docs/ENGINEERING.md` #2.

use thiserror::Error;

/// Something went wrong opening or using the store.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The underlying SQLite call failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A filesystem operation (opening, renaming) failed.
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    /// A settings value could not be encoded or decoded as JSON.
    #[error("settings value is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// A record referenced by id does not exist.
    #[error("no record found for id {0}")]
    NotFound(String),

    /// The requested mutex on the connection was poisoned by a panicking
    /// holder. This should never happen because every crate in this
    /// workspace forbids `panic!` outside tests (see `docs/ENGINEERING.md`
    /// #2), but it is handled explicitly rather than unwrapped in case a
    /// dependency panics internally.
    #[error("internal database lock was poisoned")]
    LockPoisoned,
}
