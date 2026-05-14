//! storage — SQLite persistence layer for Lens.
//!
//! Schema (R1.2A hybrid): hot fields as indexed columns, full event payload as
//! `raw_event` JSON TEXT. Adding a v0.1.x schema field lands in JSON blob with
//! no SQL migration; only breaking changes require migrations.
//!
//! Concurrency (R1.4): WAL mode + single-writer task pattern. All writes flow
//! through a tokio::sync::mpsc channel into one writer task; readers use their
//! own connections. Connection-per-thread, not connection pool.
//!
//! Idempotency (R1A): UPSERT-on-content-change. Re-parsing the same source
//! produces the same event_id (per crate::event_id); we compare content_hash
//! against the stored row and only write if it differs.
//!
//! Pagination (R1A revised): `ingest_seq` autoincrement column drives stable
//! cursor pagination against the moving dataset. Naive `(started_at, event_id)`
//! cursors would skip/dupe rows when background backfill inserts events
//! mid-scroll.
//!
//! Parser errors (R2B): separate `ingestion_issues` table. Keeps timeline
//! queries clean and FTS5 indexes unpolluted; UI sidebar badge joins on project.

pub mod db;
pub mod events;
pub mod issues;
pub mod query;
pub mod schema;

pub use db::Database;
pub use events::{upsert_event, UpsertOutcome};
pub use issues::{record_issue, IngestionIssue, IssueSeverity};
pub use query::{read_event_detail, read_timeline, Cursor, EventFilters, EventPage, TimelineRow};
