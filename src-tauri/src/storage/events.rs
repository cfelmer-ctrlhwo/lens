//! events — UPSERT-on-content-change for agent-activity events.
//!
//! The R1A semantic: re-parsing the same source file produces the same event_id,
//! so ingestion is safe to replay. UPSERT compares content_hash against the
//! stored row and only writes when content changed.
//!
//! Crucially: `ingest_seq` is NEVER updated on UPSERT. The sequence is fixed at
//! insert time so cursor pagination stays stable when sessions get re-parsed
//! mid-scroll. The `ingested_at` timestamp DOES update — that's the signal for
//! "this row was touched recently."

use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::agent_activity::AgentActivityEvent;
use crate::storage::db::{Database, DbError};

/// Outcome of an UPSERT operation. The ingestion pipeline uses this to decide
/// whether to log a warning, update UI state, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// New event_id, no prior row existed. ingest_seq is the fresh counter value.
    Inserted { ingest_seq: i64 },
    /// Event_id existed; content_hash differs; hot columns + raw_event rewritten.
    /// ingest_seq is preserved from the original insert.
    Updated,
    /// Event_id existed and content_hash matches. No-op, no write performed.
    Unchanged,
}

/// UPSERT an event. Single-writer; safe to call from the ingestion task.
///
/// Two SQL roundtrips: SELECT for current hash, then INSERT or UPDATE. Slightly
/// less efficient than INSERT...ON CONFLICT...RETURNING, but produces clean
/// three-way outcome reporting without SQL gymnastics.
pub fn upsert_event(db: &Database, event: &AgentActivityEvent) -> Result<UpsertOutcome, DbError> {
    let raw_json =
        serde_json::to_string(event).map_err(|e| DbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?;
    let content_hash = sha256(raw_json.as_bytes());

    let conn = db.conn();
    let existing_hash: Option<Vec<u8>> = conn
        .query_row(
            "SELECT content_hash FROM events WHERE event_id = ?1",
            [&event.event_id],
            |row| row.get(0),
        )
        .optional()?;

    let ingested_at = chrono::Utc::now().to_rfc3339();
    let started_at_str = event.started_at.to_rfc3339();
    let ended_at_str = event.ended_at.map(|d| d.to_rfc3339());

    match existing_hash {
        Some(existing) if existing == content_hash => Ok(UpsertOutcome::Unchanged),

        Some(_) => {
            // Hash differs — content changed. UPDATE all mutable columns;
            // ingest_seq is intentionally preserved (pagination stability).
            conn.execute(
                "UPDATE events SET
                    content_hash = ?2,
                    tool = ?3,
                    project = ?4,
                    started_at = ?5,
                    ended_at = ?6,
                    status = ?7,
                    cost_usd_estimated = ?8,
                    model = ?9,
                    raw_event = ?10,
                    ingested_at = ?11
                 WHERE event_id = ?1",
                params![
                    event.event_id,
                    content_hash,
                    event.tool,
                    event.project,
                    started_at_str,
                    ended_at_str,
                    event.status.as_str(),
                    event.cost_usd_estimated,
                    event.model,
                    raw_json,
                    ingested_at,
                ],
            )?;
            Ok(UpsertOutcome::Updated)
        }

        None => {
            // Fresh event — INSERT with a new ingest_seq.
            let ingest_seq = db.next_ingest_seq()?;
            conn.execute(
                "INSERT INTO events (
                    event_id, ingest_seq, content_hash,
                    tool, project, started_at, ended_at,
                    status, cost_usd_estimated, model,
                    raw_event, ingested_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    event.event_id,
                    ingest_seq,
                    content_hash,
                    event.tool,
                    event.project,
                    started_at_str,
                    ended_at_str,
                    event.status.as_str(),
                    event.cost_usd_estimated,
                    event.model,
                    raw_json,
                    ingested_at,
                ],
            )?;
            Ok(UpsertOutcome::Inserted { ingest_seq })
        }
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().to_vec()
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_activity::{CostSource, EventStatus, EventType};
    use chrono::TimeZone;

    fn sample_event(id: &str, cost: Option<f64>) -> AgentActivityEvent {
        AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id: id.into(),
            tool: "claude-code".into(),
            tool_version: Some("2.1.139".into()),
            event_type: EventType::SessionCompleted,
            started_at: chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 14, 0).unwrap(),
            ended_at: Some(chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 42, 18).unwrap()),
            status: EventStatus::Success,
            session_id: Some("sess-abc".into()),
            project: Some("Paperclip-Workflow-Beta".into()),
            cwd: Some("/Users/clay/Desktop/Projects/Paperclip-Workflow-Beta".into()),
            model: Some("claude-opus-4-7".into()),
            provider: Some("anthropic".into()),
            tokens_in: Some(12480),
            tokens_out: Some(3210),
            tokens_total: Some(15690),
            cost_usd_estimated: cost,
            cost_source: Some(CostSource::LogParse),
            artifacts: Some(vec!["src/agents/engineer.ts".into()]),
            error_message: None,
            summary: Some("Refactored prompts".into()),
            tags: None,
            raw_ref: Some("~/.claude/projects/foo/sess-abc.jsonl".into()),
            extra: None,
        }
    }

    #[test]
    fn first_upsert_inserts_with_ingest_seq_1() {
        let db = Database::open_in_memory().unwrap();
        let event = sample_event("evt-1", Some(0.234));
        let outcome = upsert_event(&db, &event).unwrap();
        assert_eq!(outcome, UpsertOutcome::Inserted { ingest_seq: 1 });
    }

    #[test]
    fn second_distinct_event_gets_next_ingest_seq() {
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &sample_event("evt-1", Some(0.10))).unwrap();
        let outcome = upsert_event(&db, &sample_event("evt-2", Some(0.20))).unwrap();
        assert_eq!(outcome, UpsertOutcome::Inserted { ingest_seq: 2 });
    }

    #[test]
    fn idempotent_upsert_with_same_content_returns_unchanged() {
        let db = Database::open_in_memory().unwrap();
        let event = sample_event("evt-1", Some(0.234));
        upsert_event(&db, &event).unwrap();
        let outcome = upsert_event(&db, &event).unwrap();
        assert_eq!(outcome, UpsertOutcome::Unchanged);
    }

    #[test]
    fn changed_content_returns_updated() {
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &sample_event("evt-1", Some(0.10))).unwrap();
        let outcome = upsert_event(&db, &sample_event("evt-1", Some(0.99))).unwrap();
        assert_eq!(outcome, UpsertOutcome::Updated);
    }

    #[test]
    fn ingest_seq_preserved_on_update() {
        // R1A semantics: pagination cursors are stable. UPSERT-on-change does NOT
        // bump ingest_seq, even though content changed.
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &sample_event("evt-1", Some(0.10))).unwrap();
        upsert_event(&db, &sample_event("evt-2", Some(0.20))).unwrap();
        // Now update evt-1 — its ingest_seq must remain 1, not jump to 3.
        upsert_event(&db, &sample_event("evt-1", Some(0.99))).unwrap();

        let seq: i64 = db
            .conn()
            .query_row(
                "SELECT ingest_seq FROM events WHERE event_id = 'evt-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seq, 1, "ingest_seq must not change on UPSERT");
    }

    #[test]
    fn hot_columns_reflect_event_after_insert() {
        let db = Database::open_in_memory().unwrap();
        let event = sample_event("evt-1", Some(0.234));
        upsert_event(&db, &event).unwrap();

        let (tool, project, model, status, cost): (String, Option<String>, Option<String>, String, Option<f64>) = db
            .conn()
            .query_row(
                "SELECT tool, project, model, status, cost_usd_estimated FROM events WHERE event_id = 'evt-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(tool, "claude-code");
        assert_eq!(project.as_deref(), Some("Paperclip-Workflow-Beta"));
        assert_eq!(model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(status, "success");
        assert_eq!(cost, Some(0.234));
    }

    #[test]
    fn hot_columns_updated_on_changed_upsert() {
        let db = Database::open_in_memory().unwrap();
        upsert_event(&db, &sample_event("evt-1", Some(0.10))).unwrap();
        upsert_event(&db, &sample_event("evt-1", Some(0.99))).unwrap();
        let cost: Option<f64> = db
            .conn()
            .query_row(
                "SELECT cost_usd_estimated FROM events WHERE event_id = 'evt-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cost, Some(0.99));
    }

    #[test]
    fn raw_event_blob_round_trips_via_serde() {
        let db = Database::open_in_memory().unwrap();
        let event = sample_event("evt-1", Some(0.234));
        upsert_event(&db, &event).unwrap();

        let raw: String = db
            .conn()
            .query_row(
                "SELECT raw_event FROM events WHERE event_id = 'evt-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: AgentActivityEvent = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.event_id, "evt-1");
        assert_eq!(parsed.tool, "claude-code");
        assert_eq!(parsed.tokens_total, Some(15690));
    }

    #[test]
    fn content_hash_is_stable_across_serializations() {
        // Same struct serialized twice should yield the same hash. (serde_json
        // serializes fields in declaration order, which is deterministic.) If
        // this test ever flakes, the UPSERT-skip optimization is invalid.
        let event = sample_event("evt-1", Some(0.234));
        let json1 = serde_json::to_string(&event).unwrap();
        let json2 = serde_json::to_string(&event).unwrap();
        assert_eq!(sha256(json1.as_bytes()), sha256(json2.as_bytes()));
    }
}
