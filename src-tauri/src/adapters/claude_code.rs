//! claude_code.rs — Lens adapter for Claude Code session logs.
//!
//! Purpose: Convert Claude Code's JSONL transcripts (one entry per line) into
//!   agent-activity.v1 events. Source path: `~/.claude/projects/<url-encoded-cwd>/<session-id>.jsonl`.
//! Process: For each JSONL file, parse every line into a serde_json::Value
//!   (with control-char strip retry per port reference §5), then AGGREGATE
//!   across entries into a single event (timestamps, token sums, tool counts,
//!   files modified, status inference). Per port reference §10, this maps
//!   directly to the orchestrator's `extract_session_meta(jsonl_path)` logic.
//! Connections: Reads `~/.claude/projects/**/*.jsonl`. Emits AgentActivityEvent
//!   into Lens's SQLite store via storage::upsert_event. Project resolution via
//!   ProjectResolver (projects.yaml). Cost calculation via PricingTable (pricing.yaml).
//!   Idempotent event_id via crate::event_id::derive_event_id.
//!
//! STATUS: real parser. Day 1 spike (2026-05-14) confirmed JSONL shape:
//!   top-level keys vary by .type — `user`/`assistant` have nested `message`,
//!   `attachment` has cwd at top-level, `queue-operation` and `last-prompt`
//!   are housekeeping. cwd present on ~98% of entries (GREEN tier for Claude
//!   Code alone; pooled-with-Codex falls to RED per design doc).

use crate::agent_activity::{AgentActivityEvent, CostSource, EventStatus, EventType};
use crate::event_id::derive_event_id;
use crate::pricing::PricingTable;
use crate::project_resolver::ProjectResolver;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Parser version emitted into events' `parser_version` field on
/// ingestion_issues. Bump when fixing parser bugs so old issues are
/// distinguishable from regression cases.
pub const PARSER_VERSION: &str = "0.1.0";

/// Sessions are sometimes written incrementally. If we ingest a partial
/// file, the next mtime-triggered re-parse should produce the same event_id
/// (R1A idempotency contract handles that) but a different content_hash
/// (R1A UPSERT-on-change handles that). No special handling needed here.
const SUMMARY_TRUNCATE_CHARS: usize = 300;

/// Failure-mode classifier. If tool_errors / tool_count > this ratio, the
/// status becomes Failure; below, Partial. Picked empirically from the
/// PORT-REFERENCE recommendation; revisit after first Saturday spike against
/// Clay's real failure-known sessions.
const TOOL_ERROR_FAILURE_RATIO: f64 = 0.3;

// ============================================================
// Adapter trait (every tool adapter implements this)
// ============================================================

/// All adapters implement this trait. Errors are categorized fatal vs recoverable
/// per agent-activity-v1.md §8.2.
pub trait Adapter {
    /// Stable lower-kebab-case identifier emitted in the `tool` field.
    fn tool_name(&self) -> &'static str;

    /// Walk one source file and emit zero or more ParseResults.
    /// One Claude Code session file currently yields exactly one event
    /// (success/partial/failure) or one Fatal. V1.1 may add session_started
    /// events for in-progress sessions.
    fn parse(&self, source_path: &Path) -> Vec<ParseResult>;
}

/// Result of parsing one source record. Clone is required for the ingestion
/// pipeline's test mock adapters.
#[derive(Debug, Clone)]
pub enum ParseResult {
    /// Parsed cleanly. Insert into storage as-is.
    Ok(AgentActivityEvent),

    /// Recoverable: missing optional fields, unknown values, partial data, etc.
    /// Best-effort event was produced. Insert into storage AND log warnings to
    /// ingestion_issues.
    Recoverable {
        event: AgentActivityEvent,
        warnings: Vec<String>,
    },

    /// Fatal: cannot place this record on a timeline. Caller skips it and
    /// records a parser_error issue in ingestion_issues.
    Fatal {
        source_path: PathBuf,
        reason: String,
    },
}

// ============================================================
// Claude Code adapter
// ============================================================

pub struct ClaudeCodeAdapter {
    pub project_resolver: ProjectResolver,
    pub pricing: PricingTable,
}

impl Adapter for ClaudeCodeAdapter {
    fn tool_name(&self) -> &'static str {
        "claude-code"
    }

    fn parse(&self, source_path: &Path) -> Vec<ParseResult> {
        // Step 1: read file. IO errors are fatal.
        let raw = match std::fs::read_to_string(source_path) {
            Ok(s) => s,
            Err(e) => {
                return vec![ParseResult::Fatal {
                    source_path: source_path.to_path_buf(),
                    reason: format!("IO error reading source: {}", e),
                }];
            }
        };

        // Step 2: parse JSONL line-by-line with control-char retry.
        let entries = parse_jsonl_with_recovery(&raw);
        if entries.is_empty() {
            return vec![ParseResult::Fatal {
                source_path: source_path.to_path_buf(),
                reason: "No parseable entries in JSONL file".into(),
            }];
        }

        // Step 3: derive session_id from filename stem.
        let session_id = match source_path
            .file_stem()
            .and_then(|s| s.to_str())
        {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return vec![ParseResult::Fatal {
                    source_path: source_path.to_path_buf(),
                    reason: "Cannot derive session_id from filename".into(),
                }];
            }
        };

        // Step 4: aggregate entries into a single AgentActivityEvent.
        vec![self.aggregate(&entries, &session_id, source_path)]
    }
}

impl ClaudeCodeAdapter {
    /// Aggregate JSONL entries into a single agent-activity event.
    /// This is the heart of the adapter — direct port of the orchestrator's
    /// `extract_session_meta` per PORT-REFERENCE §10.
    fn aggregate(
        &self,
        entries: &[Value],
        session_id: &str,
        source_path: &Path,
    ) -> ParseResult {
        let mut warnings: Vec<String> = Vec::new();

        // --- Timestamps: min over all entries with parseable .timestamp ---
        let mut all_timestamps: Vec<DateTime<Utc>> = Vec::new();
        for e in entries {
            if let Some(ts_str) = e.get("timestamp").and_then(|t| t.as_str()) {
                if let Ok(dt) = parse_iso_timestamp(ts_str) {
                    all_timestamps.push(dt);
                }
            }
        }
        if all_timestamps.is_empty() {
            return ParseResult::Fatal {
                source_path: source_path.to_path_buf(),
                reason: "No parseable timestamps in any entry".into(),
            };
        }
        let started_at = *all_timestamps.iter().min().unwrap();
        let ended_at = *all_timestamps.iter().max().unwrap();

        // --- cwd: first entry that has one (any type) ---
        // Per Day 1 spike: cwd lives at top-level of entries (not nested in message).
        // Day 1 finding: ~98% of Claude Code entries have cwd; falls back to None.
        let cwd = entries
            .iter()
            .find_map(|e| e.get("cwd").and_then(|c| c.as_str()).map(String::from));
        let project = cwd
            .as_deref()
            .map(|c| self.project_resolver.resolve(c))
            .or_else(|| Some(self.project_resolver.fallback().to_string()));

        // --- gitBranch: top-level on most entries (bonus signal, used as tag) ---
        let git_branch = entries.iter().find_map(|e| {
            e.get("gitBranch")
                .and_then(|b| b.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        });

        // --- First user message: filter sidechain/meta/compact/tool_result-only ---
        let first_user_msg = entries.iter().find(|e| {
            let is_user = e.get("type").and_then(|t| t.as_str()) == Some("user");
            if !is_user {
                return false;
            }
            if get_bool(e, "isSidechain") || get_bool(e, "isMeta") || get_bool(e, "isCompactSummary") {
                return false;
            }
            user_message_has_text_content(e)
        });

        let first_prompt = first_user_msg
            .and_then(extract_user_message_text)
            .map(|s| truncate(&s, SUMMARY_TRUNCATE_CHARS));

        // --- Token counts: dedupe by message.id, model from first assistant ---
        let mut seen_msg_ids: HashSet<String> = HashSet::new();
        let mut tokens_in: u64 = 0;
        let mut tokens_out: u64 = 0;
        let mut model: Option<String> = None;
        for e in entries {
            if e.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let msg = match e.get("message") {
                Some(m) => m,
                None => continue,
            };
            // Dedupe by message.id (Claude Code re-emits the same msg on retry)
            let msg_id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if !msg_id.is_empty() && !seen_msg_ids.insert(msg_id.to_string()) {
                continue;
            }
            if model.is_none() {
                model = msg.get("model").and_then(|m| m.as_str()).map(String::from);
            }
            if let Some(usage) = msg.get("usage") {
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

        // --- Tool counts + files modified + git ops + tool errors ---
        // Dedupe tool_use blocks by block.id; tool_result is_error counts errors.
        let mut seen_tool_ids: HashSet<String> = HashSet::new();
        let mut tool_counts: HashMap<String, u64> = HashMap::new();
        let mut tool_errors: u64 = 0;
        let mut files_modified: HashSet<String> = HashSet::new();
        let mut lines_added: u64 = 0;
        let mut lines_removed: u64 = 0;
        let mut git_commits: u64 = 0;
        let mut git_pushes: u64 = 0;

        for e in entries {
            let etype = e.get("type").and_then(|t| t.as_str());
            match etype {
                Some("assistant") => {
                    let blocks = match e
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        Some(b) => b,
                        None => continue,
                    };
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                            continue;
                        }
                        let bid = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        if !bid.is_empty() && !seen_tool_ids.insert(bid.to_string()) {
                            continue;
                        }
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                        *tool_counts.entry(name.to_string()).or_insert(0) += 1;

                        // Files modified — from Edit / Write / NotebookEdit
                        if matches!(name, "Edit" | "Write" | "NotebookEdit") {
                            let input = block.get("input");
                            if let Some(fp) = input
                                .and_then(|i| i.get("file_path"))
                                .and_then(|f| f.as_str())
                            {
                                files_modified.insert(fp.to_string());
                            }
                            match name {
                                "Edit" => {
                                    let old = input
                                        .and_then(|i| i.get("old_string"))
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("");
                                    let new = input
                                        .and_then(|i| i.get("new_string"))
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("");
                                    lines_removed += count_lines(old);
                                    lines_added += count_lines(new);
                                }
                                "Write" => {
                                    let content = input
                                        .and_then(|i| i.get("content"))
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("");
                                    lines_added += count_lines(content);
                                }
                                _ => {}
                            }
                        }

                        // Git ops — heuristic match on Bash commands
                        if name == "Bash" {
                            if let Some(cmd) = block
                                .get("input")
                                .and_then(|i| i.get("command"))
                                .and_then(|c| c.as_str())
                            {
                                let lower = cmd.to_lowercase();
                                if lower.contains("git commit") {
                                    git_commits += 1;
                                }
                                if lower.contains("git push") && !lower.contains("--force") {
                                    git_pushes += 1;
                                }
                            }
                        }
                    }
                }
                Some("user") => {
                    // tool_result blocks carry is_error from prior tool_use outcomes
                    let blocks = match e
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        Some(b) => b,
                        None => continue,
                    };
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                            continue;
                        }
                        if get_bool(block, "is_error") {
                            tool_errors += 1;
                        }
                    }
                }
                _ => {} // attachment, queue-operation, last-prompt: housekeeping; skip
            }
        }

        // --- Status inference (PORT-REFERENCE §8) ---
        // No top-level `outcome` field exists in real Claude Code JSONL. We derive
        // from tool_errors count. Threshold picked empirically; the Saturday spike
        // against known-outcome sessions should validate / adjust.
        let tool_count_total: u64 = tool_counts.values().sum();
        let status = if tool_errors == 0 {
            EventStatus::Success
        } else if tool_count_total > 0
            && (tool_errors as f64 / tool_count_total as f64) > TOOL_ERROR_FAILURE_RATIO
        {
            EventStatus::Failure
        } else {
            EventStatus::Partial
        };
        let event_type = match status {
            EventStatus::Failure => EventType::SessionFailed,
            _ => EventType::SessionCompleted,
        };

        // --- Cost ---
        let (cost, cost_source) = match (&model, tokens_in, tokens_out) {
            (Some(m), ti, to) if ti > 0 || to > 0 => (
                self.pricing.lookup_cost("anthropic", m, ti, to),
                Some(CostSource::LogParse),
            ),
            (None, _, _) => {
                warnings.push("Cost not computed: no model identified in any assistant entry".into());
                (None, Some(CostSource::None))
            }
            _ => {
                warnings.push("Cost not computed: zero tokens reported".into());
                (None, Some(CostSource::None))
            }
        };

        // --- event_id ---
        let event_id = derive_event_id("claude-code", session_id, started_at);

        // --- OTel aliases + Lens-specific stats packed into `extra` ---
        let mut extra = serde_json::Map::new();
        extra.insert(
            "gen_ai.conversation.id".into(),
            Value::String(session_id.to_string()),
        );
        extra.insert("gen_ai.system".into(), Value::String("anthropic".into()));
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
        // Lens-specific (not in agent-activity.v1 core; lives in extra namespace)
        if !tool_counts.is_empty() {
            extra.insert("tool_counts".into(), serde_json::json!(tool_counts));
        }
        if git_commits > 0 {
            extra.insert("git_commits".into(), Value::Number(git_commits.into()));
        }
        if git_pushes > 0 {
            extra.insert("git_pushes".into(), Value::Number(git_pushes.into()));
        }
        if lines_added > 0 {
            extra.insert("lines_added".into(), Value::Number(lines_added.into()));
        }
        if lines_removed > 0 {
            extra.insert("lines_removed".into(), Value::Number(lines_removed.into()));
        }
        if tool_errors > 0 {
            extra.insert("tool_errors".into(), Value::Number(tool_errors.into()));
        }
        if let Some(branch) = &git_branch {
            extra.insert("git_branch".into(), Value::String(branch.clone()));
        }
        // Always include tool_count_total for status-inference traceability
        extra.insert(
            "tool_count_total".into(),
            Value::Number(tool_count_total.into()),
        );

        let tokens_total = if tokens_in + tokens_out > 0 {
            Some(tokens_in + tokens_out)
        } else {
            None
        };
        let mut artifacts: Vec<String> = files_modified.into_iter().collect();
        artifacts.sort(); // determinism for tests + content_hash stability

        let event = AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id,
            tool: "claude-code".into(),
            tool_version: None, // Not in JSONL header
            event_type,
            started_at,
            ended_at: Some(ended_at),
            status,
            session_id: Some(session_id.to_string()),
            project,
            cwd,
            model,
            provider: Some("anthropic".into()),
            tokens_in: if tokens_in > 0 { Some(tokens_in) } else { None },
            tokens_out: if tokens_out > 0 { Some(tokens_out) } else { None },
            tokens_total,
            cost_usd_estimated: cost,
            cost_source,
            artifacts: if artifacts.is_empty() {
                None
            } else {
                Some(artifacts)
            },
            error_message: None, // Filled in V1.1 from the last failing tool_result if any
            summary: first_prompt,
            tags: git_branch.map(|b| vec![format!("branch:{}", b)]),
            raw_ref: Some(source_path.display().to_string()),
            extra: if extra.is_empty() {
                None
            } else {
                Some(Value::Object(extra))
            },
        };

        if warnings.is_empty() {
            ParseResult::Ok(event)
        } else {
            ParseResult::Recoverable { event, warnings }
        }
    }
}

// ============================================================
// JSONL parsing with control-char strip retry (PORT-REFERENCE §5)
// ============================================================

/// Parse a JSONL blob into a Vec<Value>, tolerating control-char contamination
/// (which DOES appear in real Claude Code logs per Day 1 spike). Lines that
/// fail to parse even after stripping control chars are silently dropped —
/// caller's responsibility to record the file as having recoverable issues.
fn parse_jsonl_with_recovery(raw: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => entries.push(v),
            Err(_) => {
                // Retry after stripping ASCII control characters (0x00-0x08,
                // 0x0B, 0x0C, 0x0E-0x1F). Tab/newline/CR are valid in JSON
                // strings and stay.
                let cleaned: String = line
                    .chars()
                    .filter(|c| !is_problematic_control(*c))
                    .collect();
                if let Ok(v) = serde_json::from_str::<Value>(&cleaned) {
                    entries.push(v);
                }
                // else: silently drop this line
            }
        }
    }
    entries
}

fn is_problematic_control(c: char) -> bool {
    matches!(c as u32,
        0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F
    )
}

// ============================================================
// Small helpers
// ============================================================

fn get_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn parse_iso_timestamp(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc))
}

/// Count "lines" as `\n` occurrences + 1 if string is non-empty. Matches the
/// orchestrator's Python logic precisely so a regression test against the
/// Python output stays meaningful.
fn count_lines(s: &str) -> u64 {
    if s.is_empty() {
        0
    } else {
        (s.matches('\n').count() as u64) + 1
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// True if this `type: "user"` entry has at least one text content block and
/// no tool_result blocks. (tool_result-only entries are echoes of prior
/// tool_use outcomes, not human prompts.)
fn user_message_has_text_content(entry: &Value) -> bool {
    let content = match entry.get("message").and_then(|m| m.get("content")) {
        Some(c) => c,
        None => return false,
    };
    match content {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(arr) => {
            let has_tool_result = arr.iter().any(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
            });
            let has_text = arr
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"));
            has_text && !has_tool_result
        }
        _ => false,
    }
}

fn extract_user_message_text(entry: &Value) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    match content {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Array(arr) => arr.iter().find_map(|b| {
            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                b.get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        }),
        _ => None,
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::PricingTable;
    use crate::project_resolver::ProjectResolver;

    fn test_adapter() -> ClaudeCodeAdapter {
        ClaudeCodeAdapter {
            project_resolver: ProjectResolver::empty(),
            pricing: PricingTable::empty(),
        }
    }

    /// Build a minimal valid Claude Code JSONL fixture as a string.
    /// Includes one queue-operation, one user message, one assistant message
    /// with token usage and a tool_use, and one tool_result. Mirrors real-shape
    /// findings from the Day 1 spike.
    fn fixture_jsonl() -> String {
        let lines = [
            r#"{"type":"queue-operation","operation":"enqueue","sessionId":"s1","timestamp":"2026-05-14T17:14:00Z","content":{}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:14:05Z","cwd":"/Users/clay/Projects/lens","gitBranch":"main","isSidechain":false,"isMeta":false,"message":{"role":"user","content":"refactor the auth flow"}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:14:30Z","cwd":"/Users/clay/Projects/lens","message":{"role":"assistant","id":"msg_abc1","model":"claude-opus-4-7","usage":{"input_tokens":1200,"output_tokens":350},"content":[{"type":"text","text":"I'll start with auth.ts"},{"type":"tool_use","id":"tool_001","name":"Edit","input":{"file_path":"/Users/clay/Projects/lens/src/auth.ts","old_string":"const x = 1;","new_string":"const x = 1;\nconst y = 2;"}}]}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:14:45Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool_001","content":"ok","is_error":false}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:15:00Z","cwd":"/Users/clay/Projects/lens","message":{"role":"assistant","id":"msg_abc2","model":"claude-opus-4-7","usage":{"input_tokens":1300,"output_tokens":80},"content":[{"type":"tool_use","id":"tool_002","name":"Bash","input":{"command":"git commit -am 'auth refactor'"}}]}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:15:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool_002","content":"[main abc123] auth refactor","is_error":false}]}}"#,
        ];
        lines.join("\n")
    }

    fn write_fixture(jsonl: &str, name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lens-cc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.jsonl", name));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    #[test]
    fn fatal_on_io_error() {
        let adapter = test_adapter();
        let results = adapter.parse(Path::new("/nonexistent/file.jsonl"));
        assert_eq!(results.len(), 1);
        match &results[0] {
            ParseResult::Fatal { reason, .. } => {
                assert!(reason.contains("IO error"), "got: {}", reason);
            }
            r => panic!("expected Fatal, got: {:?}", r),
        }
    }

    #[test]
    fn fatal_on_empty_file() {
        let path = write_fixture("", "empty");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        assert!(matches!(&results[0], ParseResult::Fatal { reason, .. } if reason.contains("No parseable entries")));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fatal_on_all_unparseable_lines() {
        let path = write_fixture("not json\nalso not json\n", "garbage");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        assert!(matches!(&results[0], ParseResult::Fatal { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ok_on_valid_fixture_with_full_aggregation() {
        let path = write_fixture(&fixture_jsonl(), "valid");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) => e,
            r => panic!("expected Ok, got: {:?}", r),
        };
        assert_eq!(event.tool, "claude-code");
        assert_eq!(event.status, EventStatus::Success); // zero tool_errors
        assert_eq!(event.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(event.tokens_in, Some(2500)); // 1200 + 1300
        assert_eq!(event.tokens_out, Some(430)); // 350 + 80
        assert_eq!(event.cwd.as_deref(), Some("/Users/clay/Projects/lens"));
        assert_eq!(event.event_type, EventType::SessionCompleted);
        assert_eq!(event.summary.as_deref(), Some("refactor the auth flow"));
        // Started/ended span all timestamps
        assert!(event.started_at < event.ended_at.unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dedupes_assistant_token_counts_by_message_id() {
        // Same message.id appearing twice should only count once.
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:30Z","message":{"role":"assistant","id":"dup1","model":"claude-opus-4-7","usage":{"input_tokens":500,"output_tokens":200},"content":[{"type":"text","text":"first"}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:35Z","message":{"role":"assistant","id":"dup1","model":"claude-opus-4-7","usage":{"input_tokens":500,"output_tokens":200},"content":[{"type":"text","text":"first (retry)"}]}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "dedup_msg");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("expected Ok/Recoverable, got: {:?}", r),
        };
        assert_eq!(event.tokens_in, Some(500), "msg.id dedup must collapse retries");
        assert_eq!(event.tokens_out, Some(200));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dedupes_tool_use_by_block_id() {
        // Same tool_use block.id appearing twice should only count once.
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{"role":"user","content":"x"}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:10Z","message":{"role":"assistant","id":"m1","model":"claude-opus-4-7","content":[{"type":"tool_use","id":"t-dup","name":"Edit","input":{"file_path":"/a/x","old_string":"a","new_string":"b"}}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:11Z","message":{"role":"assistant","id":"m2","model":"claude-opus-4-7","content":[{"type":"tool_use","id":"t-dup","name":"Edit","input":{"file_path":"/a/x","old_string":"a","new_string":"b"}}]}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "dedup_tool");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        // One files_modified entry (set dedup is automatic), AND extra.tool_counts.Edit = 1
        let artifacts = event.artifacts.as_ref().unwrap();
        assert_eq!(artifacts.len(), 1);
        let edit_count = event
            .extra
            .as_ref()
            .and_then(|e| e.get("tool_counts"))
            .and_then(|tc| tc.get("Edit"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        assert_eq!(edit_count, 1, "tool_use block dedup by id must collapse");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn status_failure_when_majority_tool_errors() {
        // 3 tool_use blocks; 2 errors. Ratio 2/3 > 0.3 → Failure.
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{"role":"user","content":"do it"}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:01Z","message":{"role":"assistant","id":"m1","model":"claude-opus-4-7","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"fail"}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:03Z","message":{"role":"assistant","id":"m2","model":"claude-opus-4-7","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"pwd"}}]}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:04Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"fail"}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:05Z","message":{"role":"assistant","id":"m3","model":"claude-opus-4-7","content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"echo"}}]}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "fail_status");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        assert_eq!(event.status, EventStatus::Failure);
        assert_eq!(event.event_type, EventType::SessionFailed);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn status_partial_when_minority_tool_errors() {
        // 5 tool_use blocks; 1 error. Ratio 0.2 <= 0.3 → Partial.
        let mut lines = vec![
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{"role":"user","content":"x"}}"#.to_string(),
        ];
        for i in 1..=5 {
            lines.push(format!(
                r#"{{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:{:02}Z","message":{{"role":"assistant","id":"m{}","model":"claude-opus-4-7","content":[{{"type":"tool_use","id":"t{}","name":"Bash","input":{{"command":"echo {}"}}}}]}}}}"#,
                i, i, i, i
            ));
        }
        lines.push(
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"fail"}]}}"#.to_string(),
        );
        let path = write_fixture(&lines.join("\n"), "partial_status");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        assert_eq!(event.status, EventStatus::Partial);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn aggregates_files_modified_lines_and_git_ops() {
        let path = write_fixture(&fixture_jsonl(), "agg");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        // Edit on /Users/clay/Projects/lens/src/auth.ts → 1 file modified
        assert_eq!(event.artifacts.as_ref().unwrap().len(), 1);
        // git commit ran once
        let git_commits = event
            .extra
            .as_ref()
            .and_then(|e| e.get("git_commits"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        assert_eq!(git_commits, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn control_chars_in_string_dont_kill_parsing() {
        // Real Claude Code logs have lines with raw control chars in strings.
        // strict JSON rejects these; we strip and retry.
        let lines = format!(
            r#"{{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{{"role":"user","content":"helloworld"}}}}
{{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:01Z","message":{{"role":"assistant","id":"m1","model":"claude-opus-4-7","usage":{{"input_tokens":100,"output_tokens":50}},"content":[{{"type":"text","text":"reply"}}]}}}}"#
        );
        // Inject a raw control char (0x01) into the literal — Rust string
        // escapes don't allow it directly; build via bytes.
        let mut bytes = lines.into_bytes();
        // Find a safe position to inject (inside the "hello..world" content) — just
        // append a line with control chars instead for simplicity.
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{\"type\":\"user\",\"sessionId\":\"s1\",\"timestamp\":\"2026-05-14T17:00:02Z\",\"message\":{\"role\":\"user\",\"content\":\"trailing\x01ctrl\"}}");
        let raw = String::from_utf8(bytes).unwrap();
        let path = write_fixture(&raw, "ctrlchars");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        // Should produce SOME event — the control-char line gets retried, and
        // even if it fails the OTHER lines parse cleanly.
        assert!(
            matches!(&results[0], ParseResult::Ok(_) | ParseResult::Recoverable { .. }),
            "control chars must not break the file, got: {:?}",
            results[0]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_idempotent_same_event_id_across_reparses() {
        let path = write_fixture(&fixture_jsonl(), "idempotent");
        let adapter = test_adapter();
        let r1 = adapter.parse(&path);
        let r2 = adapter.parse(&path);
        let id1 = match &r1[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => &e.event_id,
            _ => panic!(),
        };
        let id2 = match &r2[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => &e.event_id,
            _ => panic!(),
        };
        assert_eq!(id1, id2, "event_id must be deterministic (R1A contract)");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn otel_aliases_populated_in_extra() {
        let path = write_fixture(&fixture_jsonl(), "otel");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        let extra = event.extra.as_ref().unwrap();
        assert_eq!(
            extra.get("gen_ai.system").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            extra.get("gen_ai.request.model").and_then(|v| v.as_str()),
            Some("claude-opus-4-7")
        );
        assert_eq!(
            extra.get("gen_ai.usage.input_tokens").and_then(|v| v.as_u64()),
            Some(2500)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sidechain_user_messages_excluded_from_first_prompt() {
        // First non-sidechain user is the SECOND user line; first one is sidechain.
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","isSidechain":true,"message":{"role":"user","content":"sidechain subagent question"}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:01Z","cwd":"/a","message":{"role":"user","content":"real user prompt"}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-14T17:00:02Z","message":{"role":"assistant","id":"m1","model":"claude-opus-4-7","usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"text","text":"ok"}]}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "sidechain");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        assert_eq!(event.summary.as_deref(), Some("real user prompt"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tool_result_only_user_entries_excluded_from_first_prompt() {
        // First "user" entry is a tool_result echo, not a real prompt.
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"r"}]}}"#,
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:01Z","cwd":"/a","message":{"role":"user","content":"actual prompt here"}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "tool_result_first");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        let event = match &results[0] {
            ParseResult::Ok(e) | ParseResult::Recoverable { event: e, .. } => e,
            r => panic!("got: {:?}", r),
        };
        assert_eq!(event.summary.as_deref(), Some("actual prompt here"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cost_warnings_when_no_model_or_zero_tokens() {
        // No assistant entry → no model, no tokens. Must produce Recoverable with warning.
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-14T17:00:00Z","cwd":"/a","message":{"role":"user","content":"x"}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "no_model");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        match &results[0] {
            ParseResult::Recoverable { warnings, .. } => {
                assert!(warnings.iter().any(|w| w.contains("Cost not computed")));
            }
            r => panic!("expected Recoverable, got: {:?}", r),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fatal_when_no_parseable_timestamps() {
        // All entries lack .timestamp.
        let lines = [
            r#"{"type":"user","sessionId":"s1","cwd":"/a","message":{"role":"user","content":"x"}}"#,
            r#"{"type":"assistant","sessionId":"s1","message":{"role":"assistant","id":"m1","model":"x"}}"#,
        ];
        let path = write_fixture(&lines.join("\n"), "no_ts");
        let adapter = test_adapter();
        let results = adapter.parse(&path);
        assert!(matches!(&results[0], ParseResult::Fatal { reason, .. } if reason.contains("timestamps")));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn count_lines_matches_python_orchestrator_semantics() {
        // Direct port of count behavior: "" → 0, "a" → 1, "a\nb" → 2, "a\n" → 2.
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("a\nb\nc"), 3);
        // Trailing newline contributes one to the count (matching Python's logic
        // which is `s.count('\n') + (1 if s else 0)`)
        assert_eq!(count_lines("a\n"), 2);
    }

    #[test]
    fn truncate_caps_at_max_chars() {
        assert_eq!(truncate("short", 100), "short");
        assert_eq!(truncate("a".repeat(500).as_str(), 10), format!("{}…", "a".repeat(10)));
    }
}
