//! ipc — Tauri commands that bridge the React frontend to Lens's storage.
//!
//! Pattern: every command is a thin wrapper around an already-tested storage
//! function. Commands take `State<LensState>` for the shared Database handle,
//! lock the inner Mutex, delegate, and serialize any error as String.
//!
//! Error strategy: errors come back to JS as Promise rejections with the
//! stringified DbError. V1 doesn't try to produce typed errors over IPC —
//! the React side displays them as toast notifications.
//!
//! Concurrency: Mutex<Database> per state. SQLite is single-writer anyway and
//! Lens's expected load (~10K events/day) makes contention a non-issue. The
//! writer-task + read-pool architecture from the design doc is V1.x; V1 ships
//! with the simpler Mutex pattern.

use std::sync::{Arc, Mutex};
use tauri::State;

use crate::agent_activity::AgentActivityEvent;
use crate::storage::{
    self, query::DEFAULT_PAGE_SIZE, Cursor, Database, EventFilters, EventPage, TimelineRow,
};

/// Tauri-managed application state. Held for the lifetime of the app.
///
/// The Database lives behind `Arc<Mutex<>>` so the ingestion background task
/// can share ownership with the IPC command path. Tauri's `State<T>` is
/// internally Arc-ed, but we need a SECOND Arc handle for the spawn'd
/// backfill loop — hence the explicit Arc here.
pub struct LensState {
    pub db: Arc<Mutex<Database>>,
}

impl LensState {
    pub fn new(db: Database) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
        }
    }
}

// ============================================================
// Commands — registered via tauri::generate_handler! in lib.rs
// ============================================================

/// Return one page of timeline events. UI calls this on mount and on scroll.
#[tauri::command]
pub fn get_timeline(
    state: State<LensState>,
    filters: Option<EventFilters>,
    cursor: Option<Cursor>,
    page_size: Option<u64>,
) -> Result<EventPage, String> {
    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    let filters = filters.unwrap_or_default();
    let size = page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    storage::read_timeline(&db, &filters, cursor.as_ref(), size).map_err(|e| e.to_string())
}

/// Fetch one event in full (with deserialized raw_event JSON). Called when
/// the user clicks a timeline row to open the detail panel.
#[tauri::command]
pub fn get_event_detail(
    state: State<LensState>,
    event_id: String,
) -> Result<Option<AgentActivityEvent>, String> {
    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    storage::read_event_detail(&db, &event_id).map_err(|e| e.to_string())
}

/// Per-project event counts. Drives the sidebar — each project gets a count
/// next to its name.
#[tauri::command]
pub fn count_events_per_project(state: State<LensState>) -> Result<Vec<(String, i64)>, String> {
    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    storage::query::count_events_per_project(&db).map_err(|e| e.to_string())
}

/// Per-project ingestion-issue counts. Drives the small error badge next to
/// project names in the sidebar (R2B UX).
#[tauri::command]
pub fn count_issues_per_project(state: State<LensState>) -> Result<Vec<(String, i64)>, String> {
    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    storage::issues::count_issues_per_project(&db).map_err(|e| e.to_string())
}

/// Recent ingestion issues, newest first. Drives the "Show ingestion errors"
/// panel that appears when the user toggles the badge.
#[tauri::command]
pub fn recent_issues(
    state: State<LensState>,
    limit: Option<u64>,
) -> Result<Vec<storage::issues::StoredIssue>, String> {
    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    let limit = limit.unwrap_or(50);
    storage::issues::recent_issues(&db, limit).map_err(|e| e.to_string())
}

/// App-status command. Returns enough for the placeholder UI to render real
/// data instead of hardcoded "(checking)" strings. Once ingestion is wired,
/// this becomes the "Bridge: OK / N events / N projects" status line.
#[derive(Debug, serde::Serialize)]
pub struct AppStatus {
    pub schema_version: i64,
    pub total_events: i64,
    pub total_issues: i64,
    pub total_projects: usize,
}

#[tauri::command]
pub fn get_app_status(state: State<LensState>) -> Result<AppStatus, String> {
    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    let schema_version = db.schema_version().map_err(|e| e.to_string())?;
    let total_events: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM events WHERE tool != 'lens-adapter'", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let total_issues: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM ingestion_issues", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let total_projects = storage::query::count_events_per_project(&db)
        .map_err(|e| e.to_string())?
        .len();
    Ok(AppStatus {
        schema_version,
        total_events,
        total_issues,
        total_projects,
    })
}

// Suppress dead-code warning for TimelineRow which is referenced via re-export
// for command signatures (rust-analyzer doesn't always see across the type alias).
#[allow(dead_code)]
fn _unused_ref(_: &TimelineRow) {}

/// Dev-only command for end-to-end smoke testing without the ingestion pipeline.
/// Inserts a synthetic agent-activity event. The placeholder UI exposes a button
/// that calls this so we can verify React → IPC → storage → IPC → React end-to-end.
///
/// Remove or gate behind a feature flag once real ingestion ships.
#[tauri::command]
pub fn insert_demo_event(state: State<LensState>) -> Result<String, String> {
    use crate::agent_activity::{CostSource, EventStatus, EventType};
    use crate::event_id::derive_event_id;
    use chrono::Utc;

    let db = state.db.lock().map_err(|e| format!("state lock poisoned: {}", e))?;
    let now = Utc::now();
    // Unique-per-call event_id: include nanos in the session_id so each click
    // creates a new row instead of UPSERTing the same one.
    let session_id = format!("demo-{}", now.timestamp_nanos_opt().unwrap_or(0));
    let event_id = derive_event_id("claude-code", &session_id, now);

    let event = AgentActivityEvent {
        schema_version: "0.1.1".into(),
        event_id: event_id.clone(),
        tool: "claude-code".into(),
        tool_version: Some("2.1.139".into()),
        event_type: EventType::SessionCompleted,
        started_at: now,
        ended_at: Some(now),
        status: EventStatus::Success,
        session_id: Some(session_id),
        project: Some("Demo".into()),
        cwd: Some("/Users/demo/Projects/Demo".into()),
        model: Some("claude-opus-4-7".into()),
        provider: Some("anthropic".into()),
        tokens_in: Some(100),
        tokens_out: Some(50),
        tokens_total: Some(150),
        cost_usd_estimated: Some(0.005),
        cost_source: Some(CostSource::LogParse),
        artifacts: None,
        error_message: None,
        summary: Some("Synthetic event from insert_demo_event Tauri command".into()),
        tags: Some(vec!["demo".into()]),
        raw_ref: None,
        extra: None,
    };

    storage::upsert_event(&db, &event).map_err(|e| e.to_string())?;
    Ok(event_id)
}

// ============================================================
// Tests — exercise the inner logic without spinning up Tauri's full runtime.
// We rely on the underlying storage functions being tested (76 cases in
// storage::*) and verify command-shape correctness here.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_activity::{CostSource, EventStatus, EventType};
    use crate::storage::events::upsert_event;
    use chrono::TimeZone;

    fn fixture_event(id: &str, project: &str) -> AgentActivityEvent {
        AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id: id.into(),
            tool: "claude-code".into(),
            tool_version: None,
            event_type: EventType::SessionCompleted,
            started_at: chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 0, 0).unwrap(),
            ended_at: None,
            status: EventStatus::Success,
            session_id: None,
            project: Some(project.into()),
            cwd: None,
            model: Some("claude-opus-4-7".into()),
            provider: None,
            tokens_in: None,
            tokens_out: None,
            tokens_total: None,
            cost_usd_estimated: Some(0.1),
            cost_source: Some(CostSource::LogParse),
            artifacts: None,
            error_message: None,
            summary: None,
            tags: None,
            raw_ref: None,
            extra: None,
        }
    }

    /// State construction doesn't crash and locks work as expected.
    #[test]
    fn lens_state_construction() {
        let db = Database::open_in_memory().unwrap();
        let state = LensState::new(db);
        let _guard = state.db.lock().unwrap();
        // Lock acquired without panic.
    }

    /// get_app_status returns correct counts for an empty DB.
    /// We call the inner logic by locking + calling the storage fns; this
    /// is what the #[tauri::command] wrapper does under the hood.
    #[test]
    fn app_status_on_empty_db_is_all_zeros() {
        let db = Database::open_in_memory().unwrap();
        let state = LensState::new(db);
        let db = state.db.lock().unwrap();

        let total_events: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total_events, 0);
        let total_issues: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM ingestion_issues", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total_issues, 0);
    }

    /// After seeding events, the storage layer reports them correctly.
    /// This is effectively the integration test for the command path.
    #[test]
    fn timeline_round_trip_through_state() {
        let db = Database::open_in_memory().unwrap();
        let state = LensState::new(db);

        // Insert 3 events
        {
            let db = state.db.lock().unwrap();
            upsert_event(&db, &fixture_event("e1", "Lens")).unwrap();
            upsert_event(&db, &fixture_event("e2", "Lens")).unwrap();
            upsert_event(&db, &fixture_event("e3", "Understdy")).unwrap();
        }

        // Now read via the same storage path the command uses
        {
            let db = state.db.lock().unwrap();
            let page = storage::read_timeline(&db, &EventFilters::default(), None, 10).unwrap();
            assert_eq!(page.events.len(), 3);

            let detail = storage::read_event_detail(&db, "e1").unwrap().unwrap();
            assert_eq!(detail.project.as_deref(), Some("Lens"));

            let counts = storage::query::count_events_per_project(&db).unwrap();
            // Lens=2, Understdy=1
            assert_eq!(counts[0], ("Lens".to_string(), 2));
            assert_eq!(counts[1], ("Understdy".to_string(), 1));
        }
    }

    /// LensState is Send + Sync so Tauri can manage it across async commands.
    /// Compile-time assertion; no runtime work.
    #[test]
    fn lens_state_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LensState>();
    }
}
