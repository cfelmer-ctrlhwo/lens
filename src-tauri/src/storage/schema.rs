//! schema — DDL statements for Lens's SQLite store.
//!
//! Schema version 1. Bumped only on breaking changes (renames, drops, retypes).
//! Additive changes to agent-activity.v1 land in the `raw_event` JSON blob and
//! do NOT trigger a schema bump. See storage/db.rs for the version check on open.

/// Current schema version. Bumped only on breaking changes.
pub const SCHEMA_VERSION: i64 = 1;

/// Pragmas applied on every connection open. These configure SQLite to be
/// suitable for an interactive desktop dashboard: WAL for concurrent reads
/// during ingestion, NORMAL synchronous (fast, durable under power loss
/// modulo last few transactions), short busy_timeout so writer queue absorbs
/// contention without hangs.
pub const STARTUP_PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA busy_timeout = 5000",
    "PRAGMA foreign_keys = ON",
];

/// Schema DDL. Applied in order on first open or when schema_version is missing.
/// All statements use IF NOT EXISTS so partial applications don't break re-runs.
pub const SCHEMA_DDL: &[&str] = &[
    // Meta table — tracks schema version + arbitrary key/value config for the store.
    // Created first so we can record the version even if subsequent DDL fails.
    r#"
    CREATE TABLE IF NOT EXISTS _meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
    "#,

    // Events table — agent-activity.v1 events. Hot columns indexed; full
    // payload preserved in raw_event for forward-compat with v0.1.x additive
    // schema changes.
    //
    // ingest_seq is monotonic-on-insert; pagination cursors use this for
    // stable scrolling against a moving dataset (R1A).
    //
    // content_hash holds SHA-256 of raw_event (or of the canonical input
    // tuple — adapter's choice). Used by UPSERT to detect mutated source
    // files and skip no-op writes.
    r#"
    CREATE TABLE IF NOT EXISTS events (
        event_id              TEXT    PRIMARY KEY,
        ingest_seq            INTEGER NOT NULL,
        content_hash          BLOB    NOT NULL,

        -- Hot columns (indexed for fast filter + sort)
        tool                  TEXT    NOT NULL,
        project               TEXT,
        started_at            TEXT    NOT NULL,
        ended_at              TEXT,
        status                TEXT    NOT NULL,
        cost_usd_estimated    REAL,
        model                 TEXT,

        -- Full agent-activity.v1 payload as JSON. Read at event-detail time.
        raw_event             TEXT    NOT NULL,

        -- Lens-internal timestamp: when this row was last UPSERTed.
        ingested_at           TEXT    NOT NULL
    )
    "#,

    // Monotonic ingest_seq counter. SQLite's AUTOINCREMENT doesn't fit a
    // hybrid PK design where event_id is the natural primary key but we still
    // need a monotonic sequence. So we maintain a counter row in _meta and
    // increment via UPDATE ... RETURNING in the application layer.
    //
    // Seed with 0; the first event will be ingest_seq=1 after increment.
    r#"
    INSERT OR IGNORE INTO _meta(key, value) VALUES ('ingest_seq_counter', '0')
    "#,

    // Indexes for common query patterns: chronological timeline, per-project
    // timeline, per-tool filter, and the all-important ingest_seq cursor.
    "CREATE INDEX IF NOT EXISTS idx_events_started_at ON events(started_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_events_project_started ON events(project, started_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_events_tool_started ON events(tool, started_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_events_ingest_seq ON events(ingest_seq DESC)",

    // Ingestion issues table (R2B). Separate from events so timeline queries
    // never accidentally include parser_error rows. Sidebar badge queries this
    // independently and joins on project.
    r#"
    CREATE TABLE IF NOT EXISTS ingestion_issues (
        issue_id        INTEGER PRIMARY KEY AUTOINCREMENT,
        occurred_at     TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
        adapter         TEXT    NOT NULL,
        source_path     TEXT,
        project         TEXT,
        severity        TEXT    NOT NULL CHECK (severity IN ('fatal', 'recoverable')),
        reason          TEXT    NOT NULL,
        parser_version  TEXT
    )
    "#,
    "CREATE INDEX IF NOT EXISTS idx_issues_project ON ingestion_issues(project, occurred_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_issues_severity ON ingestion_issues(severity, occurred_at DESC)",
];

/// Stamp the schema version into _meta. Called once after DDL applies cleanly.
pub fn record_schema_version_sql() -> String {
    format!(
        "INSERT OR REPLACE INTO _meta(key, value, updated_at) \
         VALUES ('schema_version', '{}', CURRENT_TIMESTAMP)",
        SCHEMA_VERSION
    )
}
