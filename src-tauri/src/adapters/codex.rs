//! codex.rs - Lens adapter for Codex CLI session logs.
//!
//! Parses `~/.codex/sessions/**/*.{jsonl,json}` into agent-activity.v1 events.

use crate::adapters::claude_code::{Adapter, ParseResult};
use crate::agent_activity::{AgentActivityEvent, CostSource, EventStatus, EventType};
use crate::event_id::derive_event_id;
use crate::pricing::PricingTable;
use crate::project_resolver::ProjectResolver;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

const SUMMARY_TRUNCATE_CHARS: usize = 300;

pub struct CodexAdapter {
    pub project_resolver: ProjectResolver,
    pub pricing: PricingTable,
}

impl Adapter for CodexAdapter {
    fn tool_name(&self) -> &'static str {
        "codex-cli"
    }

    fn parse(&self, source_path: &Path) -> Vec<ParseResult> {
        let raw = match std::fs::read_to_string(source_path) {
            Ok(s) => s,
            Err(e) => {
                return vec![ParseResult::Fatal {
                    source_path: source_path.to_path_buf(),
                    reason: format!("IO error reading source: {}", e),
                }];
            }
        };

        let mut warnings = Vec::new();
        let entries = match parse_entries(source_path, &raw, &mut warnings) {
            Ok(entries) => entries,
            Err(reason) => {
                return vec![ParseResult::Fatal {
                    source_path: source_path.to_path_buf(),
                    reason,
                }];
            }
        };

        if entries.is_empty() {
            return vec![ParseResult::Fatal {
                source_path: source_path.to_path_buf(),
                reason: "No parseable entries in Codex session file".into(),
            }];
        }

        let session_id = match source_path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return vec![ParseResult::Fatal {
                    source_path: source_path.to_path_buf(),
                    reason: "Cannot derive session_id from filename".into(),
                }];
            }
        };

        vec![self.aggregate(&entries, &session_id, source_path, warnings)]
    }
}

impl CodexAdapter {
    fn aggregate(
        &self,
        entries: &[Value],
        session_id: &str,
        source_path: &Path,
        mut warnings: Vec<String>,
    ) -> ParseResult {
        let mut timestamps = Vec::new();
        for entry in entries {
            if let Some(ts) = entry_timestamp(entry) {
                if let Ok(dt) = parse_iso_timestamp(ts) {
                    timestamps.push(dt);
                }
            }
        }

        if timestamps.is_empty() {
            return ParseResult::Fatal {
                source_path: source_path.to_path_buf(),
                reason: "No parseable timestamps in any entry".into(),
            };
        }
        let started_at = *timestamps.iter().min().unwrap();
        let ended_at = *timestamps.iter().max().unwrap();

        let cwd = entries.iter().find_map(|entry| {
            payload(entry)
                .and_then(|p| p.get("cwd"))
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        });
        let project = Some(
            cwd.as_deref()
                .map(|c| self.project_resolver.resolve(c))
                .unwrap_or_else(|| self.project_resolver.fallback().to_string()),
        );

        let model = entries.iter().find_map(|entry| {
            let p = payload(entry)?;
            p.get("model")
                .and_then(|m| m.as_str())
                .or_else(|| p.get("config")?.get("model")?.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        });

        let mut seen_payload_ids: HashSet<String> = HashSet::new();
        let mut tokens_in = 0;
        let mut tokens_out = 0;
        for entry in entries {
            let p = match payload(entry) {
                Some(p) => p,
                None => continue,
            };
            if !p.get("usage").is_some_and(|u| u.is_object()) {
                continue;
            }
            if let Some(id) = p.get("id").and_then(|i| i.as_str()).filter(|s| !s.is_empty()) {
                if !seen_payload_ids.insert(id.to_string()) {
                    continue;
                }
            }
            if let Some(usage) = p.get("usage") {
                tokens_in += usage
                    .get("input_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
                tokens_out += usage
                    .get("output_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
            }
        }

        let first_prompt = entries
            .iter()
            .find_map(|entry| {
                let p = payload(entry)?;
                if p.get("role").and_then(|r| r.as_str()) == Some("user") {
                    extract_text(p).map(|s| truncate(&s, SUMMARY_TRUNCATE_CHARS))
                } else {
                    None
                }
            });

        let error_message = entries.iter().find_map(|entry| {
            let p = payload(entry)?;
            payload_error_text(p)
        });
        let has_error_event = entries.iter().any(|entry| {
            payload(entry)
                .and_then(|p| p.get("event_type"))
                .and_then(|e| e.as_str())
                == Some("error")
        });
        let status = if error_message.is_some() || has_error_event {
            EventStatus::Failure
        } else {
            EventStatus::Success
        };
        let event_type = match status {
            EventStatus::Failure => EventType::SessionFailed,
            _ => EventType::SessionCompleted,
        };

        let (cost, cost_source) = match (&model, tokens_in, tokens_out) {
            (Some(m), ti, to) if ti > 0 || to > 0 => (
                self.pricing.lookup_cost("openai", m, ti, to),
                Some(CostSource::LogParse),
            ),
            (None, _, _) => {
                warnings.push("Cost not computed: no model identified in any Codex entry".into());
                (None, Some(CostSource::None))
            }
            _ => {
                warnings.push("Cost not computed: zero tokens reported".into());
                (None, Some(CostSource::None))
            }
        };

        let event_id = derive_event_id("codex-cli", session_id, started_at);
        let tokens_total = if tokens_in + tokens_out > 0 {
            Some(tokens_in + tokens_out)
        } else {
            None
        };

        let mut extra = serde_json::Map::new();
        extra.insert(
            "gen_ai.conversation.id".into(),
            Value::String(session_id.to_string()),
        );
        extra.insert("gen_ai.system".into(), Value::String("openai".into()));
        if let Some(m) = &model {
            extra.insert("gen_ai.request.model".into(), Value::String(m.clone()));
        }
        if tokens_in > 0 {
            extra.insert(
                "gen_ai.usage.input_tokens".into(),
                Value::Number(tokens_in.into()),
            );
        }
        if tokens_out > 0 {
            extra.insert(
                "gen_ai.usage.output_tokens".into(),
                Value::Number(tokens_out.into()),
            );
        }

        let event = AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id,
            tool: "codex-cli".into(),
            tool_version: None,
            event_type,
            started_at,
            ended_at: Some(ended_at),
            status,
            session_id: Some(session_id.to_string()),
            project,
            cwd,
            model,
            provider: Some("openai".into()),
            tokens_in: if tokens_in > 0 { Some(tokens_in) } else { None },
            tokens_out: if tokens_out > 0 { Some(tokens_out) } else { None },
            tokens_total,
            cost_usd_estimated: cost,
            cost_source,
            artifacts: None,
            error_message,
            summary: first_prompt,
            tags: None,
            raw_ref: Some(source_path.display().to_string()),
            extra: Some(Value::Object(extra)),
        };

        if warnings.is_empty() {
            ParseResult::Ok(event)
        } else {
            ParseResult::Recoverable { event, warnings }
        }
    }
}

fn parse_entries(
    source_path: &Path,
    raw: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<Value>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    match source_path.extension().and_then(|e| e.to_str()) {
        Some("jsonl") => Ok(parse_jsonl_with_recovery(raw)),
        Some("json") => parse_json_file(raw, warnings),
        Some(other) => Err(format!("Unsupported Codex session extension: {}", other)),
        None => Err("Unsupported Codex session file without extension".into()),
    }
}

fn parse_json_file(raw: &str, warnings: &mut Vec<String>) -> Result<Vec<Value>, String> {
    let root = serde_json::from_str::<Value>(raw)
        .map_err(|e| format!("JSON parse error in Codex session file: {}", e))?;
    match root {
        Value::Object(mut obj) => match obj.remove("entries") {
            Some(Value::Array(entries)) => Ok(entries),
            Some(other) => {
                warnings.push(format!(
                    "Expected JSON .entries to be an array, got {}",
                    value_kind(&other)
                ));
                Ok(vec![Value::Object(obj)])
            }
            None => {
                warnings.push("JSON session did not contain entries array; treating root as one entry".into());
                Ok(vec![Value::Object(obj)])
            }
        },
        Value::Array(entries) => Ok(entries),
        other => {
            warnings.push(format!(
                "Unexpected JSON session root {}; treating root as one entry",
                value_kind(&other)
            ));
            Ok(vec![other])
        }
    }
}

fn parse_jsonl_with_recovery(raw: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => entries.push(value),
            Err(_) => {
                let cleaned: String = line
                    .chars()
                    .filter(|c| !is_problematic_control(*c))
                    .collect();
                if let Ok(value) = serde_json::from_str::<Value>(&cleaned) {
                    entries.push(value);
                }
            }
        }
    }
    entries
}

fn is_problematic_control(c: char) -> bool {
    matches!(c as u32, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F)
}

fn entry_timestamp(entry: &Value) -> Option<&str> {
    entry
        .get("timestamp")
        .and_then(|t| t.as_str())
        .or_else(|| payload(entry)?.get("timestamp")?.as_str())
}

fn payload(entry: &Value) -> Option<&Value> {
    entry.get("payload").or(Some(entry))
}

fn parse_iso_timestamp(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc))
}

fn extract_text(value: &Value) -> Option<String> {
    for key in ["content", "text", "prompt"] {
        if let Some(text) = value.get(key).and_then(value_to_text) {
            return Some(text);
        }
    }
    value.get("message").and_then(value_to_text)
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Array(arr) => arr.iter().find_map(|item| {
            item.get("text")
                .and_then(|t| t.as_str())
                .or_else(|| item.get("content").and_then(|t| t.as_str()))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }),
        Value::Object(obj) => obj
            .get("content")
            .and_then(value_to_text)
            .or_else(|| obj.get("text").and_then(value_to_text)),
        _ => None,
    }
}

fn payload_error_text(payload: &Value) -> Option<String> {
    payload
        .get("error")
        .and_then(value_to_text)
        .or_else(|| {
            if payload.get("event_type").and_then(|e| e.as_str()) == Some("error") {
                extract_text(payload)
            } else {
                None
            }
        })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("...");
        out
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_activity::{EventStatus, EventType};
    use std::path::{Path, PathBuf};

    fn test_adapter() -> CodexAdapter {
        CodexAdapter {
            project_resolver: ProjectResolver::empty(),
            pricing: PricingTable::empty(),
        }
    }

    fn write_fixture(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn event_from(result: &ParseResult) -> &crate::agent_activity::AgentActivityEvent {
        match result {
            ParseResult::Ok(event) | ParseResult::Recoverable { event, .. } => event,
            r => panic!("expected event result, got: {:?}", r),
        }
    }

    fn valid_jsonl() -> String {
        [
            r#"{"timestamp":"2026-05-14T17:00:00Z","payload":{"id":"init","cwd":"/Users/clay/Projects/lens","model":"gpt-5.1-codex","event_type":"session_init"}}"#,
            r#"{"timestamp":"2026-05-14T17:00:03Z","payload":{"id":"u1","role":"user","content":"implement codex ingestion"}}"#,
            r#"{"timestamp":"2026-05-14T17:00:10Z","payload":{"id":"a1","role":"assistant","usage":{"input_tokens":1000,"output_tokens":250}}}"#,
            r#"{"timestamp":"2026-05-14T17:00:20Z","payload":{"id":"a2","role":"assistant","usage":{"input_tokens":500,"output_tokens":100}}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn fatal_on_io_error() {
        let adapter = test_adapter();
        let results = adapter.parse(Path::new("/nonexistent/codex-session.jsonl"));
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ParseResult::Fatal { reason, .. } if reason.contains("IO error")));
    }

    #[test]
    fn fatal_on_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "empty.jsonl", "");
        let results = test_adapter().parse(&path);
        assert!(matches!(&results[0], ParseResult::Fatal { reason, .. } if reason.contains("No parseable entries")));
    }

    #[test]
    fn fatal_on_all_unparseable() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "garbage.jsonl", "not json\nstill not json\n");
        let results = test_adapter().parse(&path);
        assert!(matches!(&results[0], ParseResult::Fatal { reason, .. } if reason.contains("No parseable entries")));
    }

    #[test]
    fn ok_on_valid_jsonl_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "session-abc.jsonl", &valid_jsonl());
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.tool, "codex-cli");
        assert_eq!(event.session_id.as_deref(), Some("session-abc"));
        assert_eq!(event.status, EventStatus::Success);
        assert_eq!(event.event_type, EventType::SessionCompleted);
        assert_eq!(event.cwd.as_deref(), Some("/Users/clay/Projects/lens"));
        assert_eq!(event.project.as_deref(), Some("Uncategorized"));
        assert_eq!(event.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(event.provider.as_deref(), Some("openai"));
        assert_eq!(event.tokens_in, Some(1500));
        assert_eq!(event.tokens_out, Some(350));
        assert_eq!(event.tokens_total, Some(1850));
        assert_eq!(event.summary.as_deref(), Some("implement codex ingestion"));
        assert_eq!(
            event.extra.as_ref().and_then(|e| e.get("gen_ai.system")).and_then(|v| v.as_str()),
            Some("openai")
        );
        assert_eq!(
            event
                .extra
                .as_ref()
                .and_then(|e| e.get("gen_ai.request.model"))
                .and_then(|v| v.as_str()),
            Some("gpt-5.1-codex")
        );
    }

    #[test]
    fn ok_on_valid_single_json_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "session-json.json",
            r#"{"entries":[
                {"timestamp":"2026-05-14T18:00:00Z","payload":{"id":"init","config":{"model":"gpt-5.1-codex"}}},
                {"timestamp":"2026-05-14T18:00:02Z","payload":{"id":"u1","role":"user","text":"summarize this repo"}},
                {"timestamp":"2026-05-14T18:00:04Z","payload":{"id":"a1","usage":{"input_tokens":25,"output_tokens":75}}}
            ]}"#,
        );
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.session_id.as_deref(), Some("session-json"));
        assert_eq!(event.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(event.tokens_in, Some(25));
        assert_eq!(event.tokens_out, Some(75));
        assert_eq!(event.summary.as_deref(), Some("summarize this repo"));
    }

    #[test]
    fn dedupe_by_payload_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "dedupe.jsonl",
            &[
                r#"{"timestamp":"2026-05-14T17:00:00Z","payload":{"id":"u1","role":"user","content":"hi"}}"#,
                r#"{"timestamp":"2026-05-14T17:00:01Z","payload":{"id":"same","model":"gpt-5.1-codex","usage":{"input_tokens":10,"output_tokens":20}}}"#,
                r#"{"timestamp":"2026-05-14T17:00:02Z","payload":{"id":"same","model":"gpt-5.1-codex","usage":{"input_tokens":10,"output_tokens":20}}}"#,
            ]
            .join("\n"),
        );
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.tokens_in, Some(10));
        assert_eq!(event.tokens_out, Some(20));
    }

    #[test]
    fn cwd_extracted_from_nested_payload_not_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "cwd.jsonl",
            r#"{"timestamp":"2026-05-14T17:00:00Z","cwd":"/wrong","payload":{"id":"u1","role":"user","cwd":"/right","content":"hi"}}"#,
        );
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.cwd.as_deref(), Some("/right"));
    }

    #[test]
    fn missing_cwd_still_produces_uncategorized_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "missing-cwd.jsonl",
            r#"{"timestamp":"2026-05-14T17:00:00Z","payload":{"id":"u1","role":"user","content":"hi"}}"#,
        );
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.cwd, None);
        assert_eq!(event.project.as_deref(), Some("Uncategorized"));
    }

    #[test]
    fn status_failure_when_payload_error_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "error.jsonl",
            &[
                r#"{"timestamp":"2026-05-14T17:00:00Z","payload":{"id":"u1","role":"user","content":"hi"}}"#,
                r#"{"timestamp":"2026-05-14T17:00:01Z","payload":{"id":"e1","error":"rate limit"}}"#,
            ]
            .join("\n"),
        );
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.status, EventStatus::Failure);
        assert_eq!(event.event_type, EventType::SessionFailed);
        assert_eq!(event.error_message.as_deref(), Some("rate limit"));
    }

    #[test]
    fn status_failure_when_payload_event_type_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "event-type-error.jsonl",
            r#"{"timestamp":"2026-05-14T17:00:00Z","payload":{"id":"e1","event_type":"error","message":"boom"}}"#,
        );
        let results = test_adapter().parse(&path);
        let event = event_from(&results[0]);
        assert_eq!(event.status, EventStatus::Failure);
        assert_eq!(event.error_message.as_deref(), Some("boom"));
    }

    #[test]
    fn idempotent_event_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "same-id.jsonl", &valid_jsonl());
        let adapter = test_adapter();
        let first = adapter.parse(&path);
        let second = adapter.parse(&path);
        assert_eq!(event_from(&first[0]).event_id, event_from(&second[0]).event_id);
    }
}
