//! agent_activity — the V1 event schema types.
//!
//! These types are the in-memory representation of the `agent-activity.v1` JSON
//! events defined in `docs/agent-activity-v1.md`. Producers (adapters) build
//! these structs; consumers (storage, IPC, UI) consume them.
//!
//! Serialization: serde JSON, with #[serde(skip_serializing_if = "Option::is_none")]
//! so optional fields don't pollute the wire format. The on-disk + over-IPC shape
//! matches the spec's worked examples exactly.
//!
//! OpenTelemetry GenAI aliases (§3.1 of the spec) live in the `extra` field at
//! `extra.gen_ai.*`. Adapters populate them; this struct doesn't promote them
//! to typed fields because Lens's wedge is its ergonomic field names.

use serde::{Deserialize, Serialize};

/// One agent-activity.v1 event. Round-trips to/from the JSON shape in the spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentActivityEvent {
    pub schema_version: String,
    pub event_id: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_version: Option<String>,
    pub event_type: EventType,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: EventStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd_estimated: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<CostSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    SessionStarted,
    SessionCompleted,
    SessionFailed,
    ScheduledRunCompleted,
    ParserError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Success,
    Failure,
    Partial,
    Unknown,
}

impl EventStatus {
    /// Serialize back to the canonical lower-snake-case string. Used by storage
    /// when writing the `status` hot column (typed Rust enum → SQLite TEXT).
    pub fn as_str(&self) -> &'static str {
        match self {
            EventStatus::Success => "success",
            EventStatus::Failure => "failure",
            EventStatus::Partial => "partial",
            EventStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    LogParse,
    ApiBilling,
    ToolReported,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn round_trip_minimal_event() {
        let event = AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id: "01HXNKQ3M7K9V8T2P4R5Z6Y7W8".into(),
            tool: "claude-code".into(),
            tool_version: Some("2.1.139".into()),
            event_type: EventType::SessionCompleted,
            started_at: chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 14, 0).unwrap(),
            ended_at: Some(chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 42, 18).unwrap()),
            status: EventStatus::Success,
            session_id: Some("sess-abc123".into()),
            project: Some("Paperclip-Workflow-Beta".into()),
            cwd: Some("/Users/cfelmer/Desktop/Projects/Paperclip-Workflow-Beta".into()),
            model: Some("claude-opus-4-7".into()),
            provider: Some("anthropic".into()),
            tokens_in: Some(12480),
            tokens_out: Some(3210),
            tokens_total: Some(15690),
            cost_usd_estimated: Some(0.234),
            cost_source: Some(CostSource::LogParse),
            artifacts: Some(vec!["src/agents/engineer.ts".into()]),
            error_message: None,
            summary: Some("Refactored engineer prompts".into()),
            tags: None,
            raw_ref: Some("~/.claude/projects/Paperclip-Workflow-Beta/sess-abc123.jsonl".into()),
            extra: None,
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: AgentActivityEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.event_id, event.event_id);
        assert_eq!(parsed.tool, event.tool);
        assert_eq!(parsed.status, EventStatus::Success);
    }

    #[test]
    fn event_status_as_str_matches_serde_name() {
        // Storage writes status as TEXT via `as_str()`. The serde rename must
        // match the column value or filter queries will silently miss rows.
        let cases = [
            (EventStatus::Success, "success"),
            (EventStatus::Failure, "failure"),
            (EventStatus::Partial, "partial"),
            (EventStatus::Unknown, "unknown"),
        ];
        for (status, expected) in cases {
            assert_eq!(status.as_str(), expected);
            // And verify serde renames to the same value (so JSON consumers agree)
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", expected));
        }
    }

    #[test]
    fn optional_fields_skipped_in_serialization() {
        // Verify that None fields don't bloat the wire format.
        let event = AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id: "01H".into(),
            tool: "test-tool".into(),
            tool_version: None,
            event_type: EventType::SessionStarted,
            started_at: chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            ended_at: None,
            status: EventStatus::Unknown,
            session_id: None,
            project: None,
            cwd: None,
            model: None,
            provider: None,
            tokens_in: None,
            tokens_out: None,
            tokens_total: None,
            cost_usd_estimated: None,
            cost_source: None,
            artifacts: None,
            error_message: None,
            summary: None,
            tags: None,
            raw_ref: None,
            extra: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("tool_version"));
        assert!(!json.contains("ended_at"));
        assert!(!json.contains("session_id"));
        assert!(!json.contains("\"project\":null"));
    }
}
