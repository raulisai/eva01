//! Opening the database file, the integrity check + quarantine-and-recreate
//! fallback, and the table migrations.
//!
//! The integrity check is the concrete implementation of the "corrupted
//! store" row in the graceful-degradation matrix in `docs/PLAN.md` §3.3
//! point 5: a damaged SQLite file must never crash startup — it gets moved
//! aside and a fresh one takes its place.

use crate::error::StoreError;
use rusqlite::Connection;
use std::path::Path;

/// The schema version this build of `eva-store` expects. Bumped whenever
/// [`migrate`] gains a new step. Stored in SQLite's own `PRAGMA user_version`,
/// so no extra table is needed to track it.
const SCHEMA_VERSION: i64 = 3;

/// Opens (or creates) the database at `path`, verifying its integrity first.
///
/// If the file exists but fails `PRAGMA integrity_check`, it is renamed aside
/// with a timestamp suffix (so nothing is silently destroyed — the operator
/// can inspect or recover it later) and a fresh, empty database takes its
/// place, exactly as documented in `docs/PLAN.md` §3.3. Enables WAL mode,
/// which is both faster for this access pattern and more crash-resistant
/// than the default rollback journal.
pub fn open_checked(path: &Path) -> Result<Connection, StoreError> {
    if path.exists() {
        if let Err(reason) = check_integrity(path) {
            tracing::warn!(
                path = %path.display(),
                %reason,
                "la base de datos no pasó el chequeo de integridad; se aparta y se crea una nueva"
            );
            quarantine(path)?;
        }
    }

    let conn = Connection::open(path)?;
    configure_and_migrate(&conn)?;
    Ok(conn)
}

/// Opens a private, in-memory database. Used by tests and by any caller that
/// explicitly wants no persistence (e.g. a one-off `eva intent` CLI run).
pub fn open_in_memory() -> Result<Connection, StoreError> {
    let conn = Connection::open_in_memory()?;
    configure_and_migrate(&conn)?;
    Ok(conn)
}

/// Runs `PRAGMA integrity_check` and returns `Err` with SQLite's own report
/// if it finds anything other than a single `"ok"` row.
fn check_integrity(path: &Path) -> Result<(), String> {
    let conn = match Connection::open(path) {
        Ok(c) => c,
        Err(e) => return Err(format!("no se pudo abrir el archivo: {e}")),
    };

    let report: Result<String, rusqlite::Error> = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));

    match report {
        Ok(text) if text == "ok" => Ok(()),
        Ok(text) => Err(text),
        Err(e) => Err(format!("PRAGMA integrity_check falló: {e}")),
    }
}

/// Renames the file (and its WAL/SHM siblings, if present) aside with a
/// timestamp suffix, so a fresh database can be created at the original path.
fn quarantine(path: &Path) -> Result<(), StoreError> {
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let quarantined = path.with_extension(format!("corrupt-{timestamp}.sqlite3"));
    std::fs::rename(path, &quarantined)?;

    for suffix in ["-wal", "-shm"] {
        let sidecar = append_to_file_name(path, suffix);
        if sidecar.exists() {
            let quarantined_sidecar = append_to_file_name(&quarantined, suffix);
            std::fs::rename(&sidecar, &quarantined_sidecar)?;
        }
    }

    Ok(())
}

fn append_to_file_name(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

/// Sets pragmas and creates every table this crate owns, if they don't
/// already exist. Idempotent: safe to call on every startup.
fn configure_and_migrate(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;

    let current_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current_version < 1 {
        migrate_to_v1(conn)?;
    }
    if current_version < 2 {
        migrate_to_v2(conn)?;
    }
    if current_version < 3 {
        migrate_to_v3(conn)?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn migrate_to_v1(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS transcripts (
            id            TEXT PRIMARY KEY,
            created_at    TEXT NOT NULL,
            raw           TEXT NOT NULL,
            pre_formatted TEXT NOT NULL,
            formatted     TEXT NOT NULL,
            marked_bad    INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS custom_words (
            word TEXT PRIMARY KEY
        );

        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS audit_log (
            id             TEXT PRIMARY KEY,
            created_at     TEXT NOT NULL,
            transcript_id  TEXT,
            intent_json    TEXT NOT NULL,
            decision       TEXT NOT NULL,
            result_summary TEXT,
            FOREIGN KEY (transcript_id) REFERENCES transcripts(id)
        );

        CREATE INDEX IF NOT EXISTS idx_transcripts_created_at ON transcripts(created_at);
        CREATE INDEX IF NOT EXISTS idx_audit_log_created_at ON audit_log(created_at);
        ",
    )?;
    Ok(())
}

/// The "Adán, continúa" session table from `docs/PLAN.md` fase 7: one row
/// per project directory, holding the most recent agent session run there,
/// so a bare "continúa" knows which provider and session id to resume
/// without the user having to name either.
fn migrate_to_v2(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS agent_sessions (
            project_dir TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            session_id  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

/// Two additions for background agent tasks (`docs/PLAN.md` fase 6/7):
/// `agent_sessions.work_dir` — the directory the session actually ran in
/// (a worktree, when one was used), because both CLIs key their sessions to
/// the working directory and "continúa" must resume from the same one — and
/// `agent_tasks`, EVA's own record of every dispatched task, which is the
/// panel's data source for Codex (whose CLI has no `agents --json` of its
/// own) and the way a task interrupted by a worker crash stays visible.
fn migrate_to_v3(conn: &Connection) -> Result<(), StoreError> {
    let has_work_dir: bool =
        conn.prepare("SELECT 1 FROM pragma_table_info('agent_sessions') WHERE name = 'work_dir'")?.exists([])?;
    if !has_work_dir {
        conn.execute_batch("ALTER TABLE agent_sessions ADD COLUMN work_dir TEXT;")?;
    }
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS agent_tasks (
            id          TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            prompt      TEXT NOT NULL,
            project_dir TEXT NOT NULL,
            work_dir    TEXT,
            branch      TEXT,
            started_at  TEXT NOT NULL,
            finished_at TEXT,
            success     INTEGER,
            summary     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_agent_tasks_started_at ON agent_tasks(started_at);
        ",
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn opens_a_fresh_file_and_creates_the_schema() {
        let dir = tempfile::tempdir().expect("tempdir creation cannot fail in CI sandboxes we run in");
        let path = dir.path().join("eva.sqlite3");
        let conn = open_checked(&path).expect("opening a fresh path must succeed");

        let table_count: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |row| row.get(0))
            .expect("querying sqlite_master must succeed");
        assert!(table_count >= 4, "expected at least the 4 tables this crate owns");
    }

    #[test]
    fn reopening_a_healthy_file_does_not_touch_existing_data() {
        let dir = tempfile::tempdir().expect("tempdir creation cannot fail in CI sandboxes we run in");
        let path = dir.path().join("eva.sqlite3");

        {
            let conn = open_checked(&path).expect("first open must succeed");
            conn.execute("INSERT INTO settings (key, value) VALUES ('marker', '\"still here\"')", [])
                .expect("insert must succeed");
        }

        let conn = open_checked(&path).expect("second open must succeed");
        let value: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'marker'", [], |row| row.get(0))
            .expect("the row inserted before reopening must still be there");
        assert_eq!(value, "\"still here\"");
    }

    #[test]
    fn a_corrupted_file_is_quarantined_and_replaced_with_a_fresh_one() {
        let dir = tempfile::tempdir().expect("tempdir creation cannot fail in CI sandboxes we run in");
        let path = dir.path().join("eva.sqlite3");

        // Write bytes that are not a valid SQLite file at all.
        {
            let mut f = std::fs::File::create(&path).expect("creating the garbage file must succeed");
            f.write_all(b"this is not a sqlite database, just garbage bytes")
                .expect("writing garbage bytes must succeed");
        }

        let conn = open_checked(&path).expect("open_checked must recover instead of erroring");

        // The original path now holds a fresh, healthy, empty database.
        let table_count: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |row| row.get(0))
            .expect("querying the fresh database must succeed");
        assert!(table_count >= 4);

        // And the garbage was preserved next to it, not silently deleted.
        let quarantined_files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("reading the temp dir must succeed")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(quarantined_files.len(), 1, "the corrupt file must be quarantined, not deleted");
    }

    #[test]
    fn a_v2_database_is_upgraded_in_place_keeping_its_sessions() {
        // Exactly what a user who ran the previous release has on disk: the
        // v2 `agent_sessions` table without `work_dir`, with a row in it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("eva.sqlite3");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE agent_sessions (project_dir TEXT PRIMARY KEY, provider_id TEXT NOT NULL,
                     session_id TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO agent_sessions VALUES ('/repos/iam', 'codex',
                     '11111111-1111-1111-1111-111111111111', '2026-09-22T00:00:00+00:00');
                 PRAGMA user_version = 2;",
            )
            .expect("seed a v2 database");
        }

        let conn = open_checked(&path).expect("upgrade must succeed");

        let work_dir: Option<String> = conn
            .query_row("SELECT work_dir FROM agent_sessions WHERE project_dir = '/repos/iam'", [], |r| r.get(0))
            .expect("the old row must survive and gain a work_dir column");
        assert_eq!(work_dir, None);
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn in_memory_store_gets_the_same_schema() {
        let conn = open_in_memory().expect("in-memory open must succeed");
        let table_count: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |row| row.get(0))
            .expect("querying sqlite_master must succeed");
        assert!(table_count >= 4);
    }
}
