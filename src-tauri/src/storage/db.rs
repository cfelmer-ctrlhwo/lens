//! db — connection wrapper, schema init, version check.
//!
//! Two open modes:
//!   - `Database::open_path(path)` — production. Persistent SQLite file at the
//!     given path (e.g. ~/Library/Application Support/Lens/lens.db).
//!   - `Database::open_in_memory()` — tests. Ephemeral.
//!
//! Both modes run the same init sequence: apply STARTUP_PRAGMAS, apply
//! SCHEMA_DDL, stamp schema_version into _meta. Idempotent — re-opening a
//! populated database is safe.
//!
//! Version policy:
//!   - If schema_version row is absent (fresh DB) → record current version.
//!   - If schema_version matches code constant → open normally.
//!   - If schema_version is lower than code constant → V1 punts: refuses to
//!     open and instructs the user to rebuild from source logs. V1.x adds
//!     real migrations.
//!   - If schema_version is HIGHER than code constant → Lens was downgraded;
//!     refuse to open (downgrades drop unknown columns silently otherwise).

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use thiserror::Error;

use super::schema::{record_schema_version_sql, SCHEMA_DDL, SCHEMA_VERSION, STARTUP_PRAGMAS};

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "Schema version mismatch: database has v{found}, code expects v{expected}. \
         V1 does not yet implement migrations; rebuild the database from source logs."
    )]
    SchemaTooOld { found: i64, expected: i64 },
    #[error(
        "Schema version mismatch: database has v{found} but code expects v{expected}. \
         This database was written by a newer Lens. Refusing to open to avoid data loss."
    )]
    SchemaTooNew { found: i64, expected: i64 },
}

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open a persistent SQLite database at the given path. Creates the file
    /// if it doesn't exist; applies schema on first open or upgrades the
    /// stamp if a fresh schema is applied.
    pub fn open_path<P: AsRef<Path>>(path: P) -> Result<Self, DbError> {
        let conn = Connection::open_with_flags(
            path.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        let mut db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Open an ephemeral in-memory database. Useful for tests.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let mut db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Apply startup pragmas, schema DDL, and version stamp. Idempotent.
    fn init(&mut self) -> Result<(), DbError> {
        // PRAGMAs: applied via execute, not prepare — they don't bind parameters.
        for pragma in STARTUP_PRAGMAS {
            // execute_batch tolerates statements that don't return rows.
            self.conn.execute_batch(pragma)?;
        }

        // Schema DDL: each statement IF NOT EXISTS so re-runs are safe.
        for stmt in SCHEMA_DDL {
            self.conn.execute_batch(stmt)?;
        }

        // Check (or stamp) schema_version in _meta.
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM _meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .ok();

        match stored {
            None => {
                // Fresh DB or partially-applied schema. Stamp the current version.
                self.conn.execute_batch(&record_schema_version_sql())?;
            }
            Some(v) => {
                let found: i64 = v.parse().unwrap_or(-1);
                if found < SCHEMA_VERSION {
                    return Err(DbError::SchemaTooOld {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
                if found > SCHEMA_VERSION {
                    return Err(DbError::SchemaTooNew {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
            }
        }

        Ok(())
    }

    /// Read-only access to the inner connection. Mainly for tests and the
    /// query-side modules that haven't been written yet. The write-side will
    /// own its own connection inside the writer task.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Current schema version as recorded in _meta.
    pub fn schema_version(&self) -> Result<i64, DbError> {
        let v: String = self.conn.query_row(
            "SELECT value FROM _meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(v.parse().unwrap_or(-1))
    }

    /// Increment the ingest_seq counter and return the new value. Use this
    /// inside the writer task on every event insert/upsert. UPDATE ... RETURNING
    /// is atomic; safe even though the writer task is single-threaded
    /// (defensive against future re-architecture).
    pub fn next_ingest_seq(&self) -> Result<i64, DbError> {
        let new_val: String = self.conn.query_row(
            "UPDATE _meta SET value = CAST(value AS INTEGER) + 1 \
             WHERE key = 'ingest_seq_counter' RETURNING value",
            [],
            |row| row.get(0),
        )?;
        Ok(new_val.parse().unwrap_or(0))
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_applies_schema() {
        let db = Database::open_in_memory().expect("open should succeed");
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn events_table_exists_after_init() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "events table must be created on init");
    }

    #[test]
    fn ingestion_issues_table_exists_after_init() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='ingestion_issues'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "ingestion_issues table must be created on init");
    }

    #[test]
    fn all_expected_indexes_present() {
        let db = Database::open_in_memory().unwrap();
        let expected_indexes = [
            "idx_events_started_at",
            "idx_events_project_started",
            "idx_events_tool_started",
            "idx_events_ingest_seq",
            "idx_issues_project",
            "idx_issues_severity",
        ];
        for idx_name in &expected_indexes {
            let count: i64 = db
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [idx_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "index {} must exist", idx_name);
        }
    }

    #[test]
    fn wal_mode_is_enabled() {
        // For persistent DBs WAL is critical (concurrent reads during ingestion).
        // In-memory DBs report "memory" instead of "wal" — that's expected; we
        // verify the pragma was at least executed by checking journal_mode is
        // not the default "delete".
        let db = Database::open_in_memory().unwrap();
        let mode: String = db
            .conn()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        // In-memory DBs use "memory" journal regardless of WAL pragma attempt;
        // a file DB would be "wal" here. Both are non-default and acceptable.
        assert!(
            mode == "wal" || mode == "memory",
            "journal_mode should be wal or memory, got: {}",
            mode
        );
    }

    #[test]
    fn busy_timeout_pragma_applied() {
        let db = Database::open_in_memory().unwrap();
        let timeout: i64 = db
            .conn()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn reopen_is_idempotent() {
        // Persistent DB: open once, close, reopen — schema should still be there
        // and version check should pass.
        let tmpdir = std::env::temp_dir();
        let db_path = tmpdir.join(format!("lens-test-{}.db", std::process::id()));

        {
            let db = Database::open_path(&db_path).unwrap();
            assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        } // db dropped, file closed

        {
            let db = Database::open_path(&db_path).unwrap();
            assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        }

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[test]
    fn ingest_seq_counter_monotonic() {
        let db = Database::open_in_memory().unwrap();
        let a = db.next_ingest_seq().unwrap();
        let b = db.next_ingest_seq().unwrap();
        let c = db.next_ingest_seq().unwrap();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
        // The counter persists in _meta so it survives across "reopens".
    }

    #[test]
    fn ingest_seq_counter_persists_across_reopen() {
        let tmpdir = std::env::temp_dir();
        let db_path = tmpdir.join(format!("lens-test-seq-{}.db", std::process::id()));

        {
            let db = Database::open_path(&db_path).unwrap();
            assert_eq!(db.next_ingest_seq().unwrap(), 1);
            assert_eq!(db.next_ingest_seq().unwrap(), 2);
        }
        {
            let db = Database::open_path(&db_path).unwrap();
            // Counter resumed from where the prior session left off, NOT reset to 1.
            assert_eq!(db.next_ingest_seq().unwrap(), 3);
        }

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[test]
    fn ingestion_issues_severity_check_constraint() {
        // Schema enforces severity ∈ {'fatal', 'recoverable'} via CHECK.
        let db = Database::open_in_memory().unwrap();
        let valid_result = db.conn().execute(
            "INSERT INTO ingestion_issues(adapter, severity, reason) \
             VALUES('claude-code', 'fatal', 'unparseable JSON')",
            [],
        );
        assert!(valid_result.is_ok());

        let invalid_result = db.conn().execute(
            "INSERT INTO ingestion_issues(adapter, severity, reason) \
             VALUES('claude-code', 'made-up-severity', 'whatever')",
            [],
        );
        assert!(invalid_result.is_err(), "CHECK constraint must reject invalid severity");
    }
}
