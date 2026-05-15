//! query — cursor-paginated reads for the events table.
//!
//! Pagination cursor is `ingest_seq` (not `(started_at, event_id)`) per R1A
//! revised. Stable against background-backfill insertions during user scroll.
//! Page direction is "older" — the UI loads newest events first, then scrolls
//! backwards through history. Cursor encodes "give me events with ingest_seq
//! strictly less than X."
//!
//! Filter dimensions: project, tool, status. AND'd together. Empty/None means
//! "any". Future filters (date range, model, cost threshold) follow the same
//! shape.
//!
//! Sort order: by ingest_seq DESC. The UI displays events ordered by
//! started_at; if the user wants strict chronological, sort client-side after
//! page load (cheap because pages are bounded at 200).
//!
//! V1 reads only deserialize `raw_event` JSON for event_detail queries. List
//! views project hot columns directly so we don't pay serde cost for events
//! the user is just scrolling past.

use rusqlite::{types::Value, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::agent_activity::AgentActivityEvent;
use crate::storage::db::{Database, DbError};

/// Maximum page size we'll honor regardless of caller request. Bounds IPC
/// payload size and keeps the timeline render budget predictable.
pub const MAX_PAGE_SIZE: u64 = 500;

/// Default page size when caller doesn't specify. Hits the sweet spot for
/// virtualized timeline scroll (visible viewport + readahead buffer).
pub const DEFAULT_PAGE_SIZE: u64 = 200;

/// Opaque cursor passed back to the client and returned on next page request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// Last `ingest_seq` in the previous page. Next page returns events with
    /// `ingest_seq < this`. Empty page → caller should not call again.
    pub before_ingest_seq: i64,
}

/// AND'd filter dimensions. None values mean "no constraint on this field."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilters {
    pub project: Option<String>,
    pub tool: Option<String>,
    pub status: Option<String>,
}

/// A page of events plus a cursor for the next page (None if this is the last).
#[derive(Debug, Clone, Serialize)]
pub struct EventPage {
    pub events: Vec<TimelineRow>,
    pub next_cursor: Option<Cursor>,
}

/// Compact projection for timeline list views — doesn't deserialize the full
/// raw_event JSON. Sufficient for rendering a timeline row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineRow {
    pub event_id: String,
    pub ingest_seq: i64,
    pub tool: String,
    pub project: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub cost_usd_estimated: Option<f64>,
    pub model: Option<String>,
}

/// Fetch one page of timeline events with the given filters and cursor.
///
/// Page semantics:
///   - cursor=None → newest N events
///   - cursor=Some → next N events with ingest_seq < cursor.before_ingest_seq
///   - returns next_cursor=Some only if more events exist after this page
pub fn read_timeline(
    db: &Database,
    filters: &EventFilters,
    cursor: Option<&Cursor>,
    page_size: u64,
) -> Result<EventPage, DbError> {
    let page_size = page_size.min(MAX_PAGE_SIZE).max(1);

    // Build dynamic WHERE clause. We accumulate (sql_fragment, value) pairs.
    let mut where_clauses: Vec<&str> = Vec::new();
    let mut sql_params: Vec<Value> = Vec::new();

    if let Some(c) = cursor {
        where_clauses.push("ingest_seq < ?");
        sql_params.push(Value::Integer(c.before_ingest_seq));
    }
    if let Some(p) = &filters.project {
        where_clauses.push("project = ?");
        sql_params.push(Value::Text(p.clone()));
    }
    if let Some(t) = &filters.tool {
        where_clauses.push("tool = ?");
        sql_params.push(Value::Text(t.clone()));
    }
    if let Some(s) = &filters.status {
        where_clauses.push("status = ?");
        sql_params.push(Value::Text(s.clone()));
    }
    // Always exclude lens-adapter parser_error events from the main timeline.
    // (Per R2B those live in ingestion_issues, not events — but be defensive
    // in case any leak in.)
    where_clauses.push("tool != 'lens-adapter'");

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clauses.join(" AND "))
    };
    // Fetch one extra row to know whether a "next page" exists without
    // running a separate COUNT query.
    let limit = page_size + 1;
    sql_params.push(Value::Integer(limit as i64));

    let sql = format!(
        "SELECT event_id, ingest_seq, tool, project, started_at, ended_at,
                status, cost_usd_estimated, model
         FROM events
         {}
         ORDER BY ingest_seq DESC
         LIMIT ?",
        where_sql
    );

    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(sql_params.iter()),
        |r| {
            Ok(TimelineRow {
                event_id: r.get(0)?,
                ingest_seq: r.get(1)?,
                tool: r.get(2)?,
                project: r.get(3)?,
                started_at: r.get(4)?,
                ended_at: r.get(5)?,
                status: r.get(6)?,
                cost_usd_estimated: r.get(7)?,
                model: r.get(8)?,
            })
        },
    )?;

    let mut events: Vec<TimelineRow> = Vec::with_capacity(page_size as usize);
    for row in rows {
        events.push(row?);
    }

    // Trim the one extra and produce next_cursor only if we actually got
    // more than page_size rows.
    let next_cursor = if events.len() > page_size as usize {
        events.truncate(page_size as usize);
        events.last().map(|last| Cursor { before_ingest_seq: last.ingest_seq })
    } else {
        None
    };

    Ok(EventPage { events, next_cursor })
}

/// Fetch the full AgentActivityEvent for a single event_id. Deserializes the
/// raw_event JSON blob. Used by the event-detail panel on click.
pub fn read_event_detail(
    db: &Database,
    event_id: &str,
) -> Result<Option<AgentActivityEvent>, DbError> {
    let raw: Option<String> = db
        .conn()
        .query_row(
            "SELECT raw_event FROM events WHERE event_id = ?1",
            [event_id],
            |r| r.get(0),
        )
        .optional()?;

    match raw {
        None => Ok(None),
        Some(json) => {
            let event: AgentActivityEvent = serde_json::from_str(&json).map_err(|e| {
                DbError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))
            })?;
            Ok(Some(event))
        }
    }
}

/// Per-project event counts. Drives the sidebar count badges next to project
/// names. Independent of the ingestion_issues count (which has its own query).
pub fn count_events_per_project(db: &Database) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = db.conn().prepare(
        "SELECT COALESCE(project, 'Uncategorized') AS project, COUNT(*) AS event_count
         FROM events
         WHERE tool != 'lens-adapter'
         GROUP BY COALESCE(project, 'Uncategorized')
         ORDER BY event_count DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_activity::{CostSource, EventStatus, EventType};
    use crate::storage::events::upsert_event;
    use chrono::TimeZone;

    fn event_with(id: &str, project: &str, tool: &str, status: EventStatus) -> AgentActivityEvent {
        AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id: id.into(),
            tool: tool.into(),
            tool_version: None,
            event_type: EventType::SessionCompleted,
            started_at: chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 14, 0).unwrap(),
            ended_at: None,
            status,
            session_id: None,
            project: Some(project.into()),
            cwd: None,
            model: Some("claude-opus-4-7".into()),
            provider: None,
            tokens_in: None,
            tokens_out: None,
            tokens_total: None,
            cost_usd_estimated: Some(0.234),
            cost_source: Some(CostSource::LogParse),
            artifacts: None,
            error_message: None,
            summary: None,
            tags: None,
            raw_ref: None,
            extra: None,
        }
    }

    fn seed_events(db: &Database, n: usize) {
        for i in 0..n {
            let project = if i % 2 == 0 { "Lens" } else { "Understdy" };
            let evt = event_with(&format!("evt-{:03}", i), project, "claude-code", EventStatus::Success);
            upsert_event(db, &evt).unwrap();
        }
    }

    #[test]
    fn empty_db_returns_empty_page_no_cursor() {
        let db = Database::open_in_memory().unwrap();
        let page = read_timeline(&db, &EventFilters::default(), None, 20).unwrap();
        assert!(page.events.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn page_size_capped_at_max() {
        let db = Database::open_in_memory().unwrap();
        seed_events(&db, 5);
        let page = read_timeline(&db, &EventFilters::default(), None, MAX_PAGE_SIZE * 2).unwrap();
        assert!(page.events.len() <= MAX_PAGE_SIZE as usize);
    }

    #[test]
    fn newest_events_returned_first_by_ingest_seq() {
        // ingest_seq is monotonic on insert, so the LAST inserted event has
        // the highest ingest_seq and should come first.
        let db = Database::open_in_memory().unwrap();
        seed_events(&db, 5);
        let page = read_timeline(&db, &EventFilters::default(), None, 10).unwrap();
        assert_eq!(page.events[0].event_id, "evt-004");
        assert_eq!(page.events[4].event_id, "evt-000");
    }

    #[test]
    fn cursor_paginates_correctly_without_skip_or_dupe() {
        let db = Database::open_in_memory().unwrap();
        seed_events(&db, 10);

        // Page 1: 4 events, cursor returned
        let page1 = read_timeline(&db, &EventFilters::default(), None, 4).unwrap();
        assert_eq!(page1.events.len(), 4);
        assert!(page1.next_cursor.is_some());

        // Page 2: next 4 events using the cursor
        let cursor = page1.next_cursor.as_ref().unwrap();
        let page2 = read_timeline(&db, &EventFilters::default(), Some(cursor), 4).unwrap();
        assert_eq!(page2.events.len(), 4);

        // Page 3: last 2 events, no further cursor
        let cursor = page2.next_cursor.as_ref().unwrap();
        let page3 = read_timeline(&db, &EventFilters::default(), Some(cursor), 4).unwrap();
        assert_eq!(page3.events.len(), 2);
        assert!(page3.next_cursor.is_none());

        // Sanity: no duplicates across pages
        let mut all_ids: Vec<String> = page1.events.iter().map(|e| e.event_id.clone()).collect();
        all_ids.extend(page2.events.iter().map(|e| e.event_id.clone()));
        all_ids.extend(page3.events.iter().map(|e| e.event_id.clone()));
        let mut sorted = all_ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all_ids.len(), "no duplicate events across pages");
        assert_eq!(all_ids.len(), 10);
    }

    #[test]
    fn filter_by_project() {
        let db = Database::open_in_memory().unwrap();
        seed_events(&db, 10);
        let filters = EventFilters {
            project: Some("Lens".into()),
            ..Default::default()
        };
        let page = read_timeline(&db, &filters, None, 100).unwrap();
        assert_eq!(page.events.len(), 5);
        assert!(page.events.iter().all(|e| e.project.as_deref() == Some("Lens")));
    }

    #[test]
    fn filter_by_tool() {
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &event_with("evt-cc-1", "P", "claude-code", EventStatus::Success)).unwrap();
        upsert_event(&db, &event_with("evt-cx-1", "P", "codex-cli", EventStatus::Success)).unwrap();
        upsert_event(&db, &event_with("evt-cc-2", "P", "claude-code", EventStatus::Success)).unwrap();

        let filters = EventFilters {
            tool: Some("codex-cli".into()),
            ..Default::default()
        };
        let page = read_timeline(&db, &filters, None, 100).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].tool, "codex-cli");
    }

    #[test]
    fn filter_by_status() {
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &event_with("evt-s", "P", "claude-code", EventStatus::Success)).unwrap();
        upsert_event(&db, &event_with("evt-f", "P", "claude-code", EventStatus::Failure)).unwrap();
        upsert_event(&db, &event_with("evt-p", "P", "claude-code", EventStatus::Partial)).unwrap();

        let filters = EventFilters {
            status: Some("failure".into()),
            ..Default::default()
        };
        let page = read_timeline(&db, &filters, None, 100).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].status, "failure");
    }

    #[test]
    fn lens_adapter_events_excluded_from_timeline() {
        // R2B safety net: even if a parser_error somehow lands in events,
        // the timeline view never shows it.
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &event_with("evt-real", "P", "claude-code", EventStatus::Success)).unwrap();
        upsert_event(&db, &event_with("evt-leak", "P", "lens-adapter", EventStatus::Failure)).unwrap();
        let page = read_timeline(&db, &EventFilters::default(), None, 100).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].tool, "claude-code");
    }

    #[test]
    fn read_event_detail_returns_full_event() {
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &event_with("evt-1", "Lens", "claude-code", EventStatus::Success)).unwrap();

        let event = read_event_detail(&db, "evt-1").unwrap().unwrap();
        assert_eq!(event.event_id, "evt-1");
        assert_eq!(event.project.as_deref(), Some("Lens"));
        assert_eq!(event.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn read_event_detail_returns_none_for_unknown_id() {
        let db = Database::open_in_memory().unwrap();
        let result = read_event_detail(&db, "evt-nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn count_events_per_project_groups_correctly() {
        let db = Database::open_in_memory().unwrap();
        seed_events(&db, 10); // 5 Lens + 5 Understdy
        upsert_event(&db, &event_with("evt-x", "Lens", "claude-code", EventStatus::Success)).unwrap();
        let counts = count_events_per_project(&db).unwrap();
        // Lens has 6, Understdy has 5
        assert_eq!(counts[0], ("Lens".to_string(), 6));
        assert_eq!(counts[1], ("Understdy".to_string(), 5));
    }
}
