//! claude_code.rs — Lens adapter for Claude Code session logs.
//!
//! Purpose: Convert Claude Code's session-meta JSON files into agent-activity.v1 events.
//! Process: Walk one or more session-meta source paths (configurable), parse each file,
//!   emit events via the Adapter trait. Adapter is idempotent — re-running over the same
//!   source produces identical event_ids.
//! Connections: Reads paths from Lens config (likely ~/.claude/usage-data/session-meta/*.json;
//!   verify exact path during Week 1 spike — CLAUDE.md notes this is the orchestrator-aggregated
//!   location, NOT ~/.claude/projects/*/session-meta which is referenced in design doc).
//!   Emits AgentActivityEvent into Lens's SQLite store. Project resolution via ProjectResolver
//!   (projects.yaml). Cost calculation via PricingTable (pricing.yaml).
//!
//! STATUS: skeleton. Function signatures, error model, and data flow are committed.
//!   Bodies marked todo!() are Week 1 work. Compile-checked structure; runtime not yet wired.

use std::path::{Path, PathBuf};

// ============================================================
// Schema types (mirror agent-activity-v1.md §5)
// ============================================================

/// One agent-activity.v1 event. Round-trips to/from the JSON shape in the spec.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    SessionStarted,
    SessionCompleted,
    SessionFailed,
    ScheduledRunCompleted,
    ParserError,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Success,
    Failure,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    LogParse,
    ApiBilling,
    ToolReported,
    None,
}

// ============================================================
// Adapter trait (every tool adapter implements this)
// ============================================================

/// All adapters implement this trait. Errors are categorized fatal vs recoverable
/// per agent-activity-v1.md §8.2.
pub trait Adapter {
    /// Stable lower-kebab-case identifier emitted in the `tool` field.
    fn tool_name(&self) -> &'static str;

    /// Walk source paths and emit events. Returns a vec of ParseResult per source file.
    /// The caller (Lens ingestion pipeline) is responsible for inserting Ok+Recoverable
    /// events into storage and emitting parser_error events for Fatal cases.
    fn parse(&self, source_path: &Path) -> Vec<ParseResult>;
}

/// Result of parsing one source record.
#[derive(Debug)]
pub enum ParseResult {
    /// Parsed cleanly. Insert into storage as-is.
    Ok(AgentActivityEvent),

    /// Recoverable: missing optional fields, unknown event_type value, etc.
    /// Best-effort event was produced. Insert into storage AND log warnings.
    Recoverable {
        event: AgentActivityEvent,
        warnings: Vec<String>,
    },

    /// Fatal: cannot place this record on a timeline. Caller skips it and emits
    /// a synthetic parser_error event (see EventType::ParserError) instead.
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

        // Step 2: parse JSON to a permissive intermediate shape.
        // Per §8.2: unparseable JSON is fatal.
        let raw_session: ClaudeCodeRawSession = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return vec![ParseResult::Fatal {
                    source_path: source_path.to_path_buf(),
                    reason: format!("Unparseable JSON: {}", e),
                }];
            }
        };

        // Step 3: validate required fields per §5.1 + §8.2 fatal taxonomy.
        if raw_session.session_id.is_empty() {
            return vec![ParseResult::Fatal {
                source_path: source_path.to_path_buf(),
                reason: "Missing session_id in source".into(),
            }];
        }
        if raw_session.started_at.is_none() {
            return vec![ParseResult::Fatal {
                source_path: source_path.to_path_buf(),
                reason: "Missing started_at in source".into(),
            }];
        }

        // Step 4: build the terminal event. One session = one event in V1.
        // (V1.1+ may split into session_started + session_completed pairs.)
        vec![self.build_terminal_event(&raw_session, source_path)]
    }
}

impl ClaudeCodeAdapter {
    /// Build a session_completed or session_failed event from a parsed raw session.
    /// Returns Recoverable when optional fields are missing but core event is well-formed.
    fn build_terminal_event(
        &self,
        raw: &ClaudeCodeRawSession,
        source_path: &Path,
    ) -> ParseResult {
        let mut warnings: Vec<String> = Vec::new();

        // Idempotent event_id: deterministic from (tool, session_id, started_at)
        let event_id = derive_event_id(
            "claude-code",
            &raw.session_id,
            &raw.started_at_str(),
        );

        // Event type + status from raw.outcome. Recoverable on unknown values.
        let (event_type, status, error_message) = match raw.outcome.as_deref() {
            Some("success") => (EventType::SessionCompleted, EventStatus::Success, None),
            Some("error") | Some("failure") => (
                EventType::SessionFailed,
                EventStatus::Failure,
                raw.error.clone(),
            ),
            Some("partial") => (EventType::SessionCompleted, EventStatus::Partial, None),
            None => {
                warnings.push("Source has no outcome field; defaulting to status=unknown".into());
                (EventType::SessionCompleted, EventStatus::Unknown, None)
            }
            Some(other) => {
                warnings.push(format!(
                    "Unknown outcome value '{}'; treating as status=unknown",
                    other
                ));
                (EventType::SessionCompleted, EventStatus::Unknown, None)
            }
        };

        // Project resolution via projects.yaml. Unmapped cwd → "Uncategorized".
        let project = raw.cwd.as_deref().map(|cwd| self.project_resolver.resolve(cwd));

        // Cost estimation via pricing.yaml. Recoverable when model or tokens are missing.
        let (cost, cost_source) = match (raw.model.as_deref(), raw.tokens_in, raw.tokens_out) {
            (Some(model), Some(ti), Some(to)) => {
                let c = self.pricing.lookup_cost("anthropic", model, ti, to);
                (c, Some(CostSource::LogParse))
            }
            _ => {
                warnings.push("Cost not computed; missing model or token counts".into());
                (None, Some(CostSource::None))
            }
        };

        let tokens_total = match (raw.tokens_in, raw.tokens_out) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };

        // Build OTel GenAI aliases per agent-activity-v1.md §3.1.
        // Populate extra.gen_ai.* for fields with direct OTel mappings so
        // OTel-native consumers can read this event without translation.
        let mut extra = serde_json::Map::new();
        extra.insert("gen_ai.conversation.id".into(), serde_json::json!(raw.session_id));
        extra.insert("gen_ai.system".into(), serde_json::json!("anthropic"));
        if let Some(m) = &raw.model {
            extra.insert("gen_ai.request.model".into(), serde_json::json!(m));
        }
        if let Some(t) = raw.tokens_in {
            extra.insert("gen_ai.usage.input_tokens".into(), serde_json::json!(t));
        }
        if let Some(t) = raw.tokens_out {
            extra.insert("gen_ai.usage.output_tokens".into(), serde_json::json!(t));
        }

        let event = AgentActivityEvent {
            schema_version: "0.1.1".into(), // R4A: OTel alignment table added
            event_id,
            tool: "claude-code".into(),
            tool_version: raw.tool_version.clone(),
            event_type,
            started_at: raw.started_at.expect("validated above"),
            ended_at: raw.ended_at,
            status,
            session_id: Some(raw.session_id.clone()),
            project,
            cwd: raw.cwd.clone(),
            model: raw.model.clone(),
            // R2.1 fix: hardcode provider statically per adapter. Don't infer from model presence.
            provider: Some("anthropic".into()),
            tokens_in: raw.tokens_in,
            tokens_out: raw.tokens_out,
            tokens_total,
            cost_usd_estimated: cost,
            cost_source,
            artifacts: raw.artifacts.clone(),
            error_message,
            summary: raw.summary.clone(),
            tags: None,
            raw_ref: Some(source_path.display().to_string()),
            extra: if extra.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(extra))
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
// Raw source shape (permissive intermediate)
//
// Claude Code's actual session-meta JSON shape needs to be verified during
// the Week 1 spike. This struct is the working hypothesis based on what
// fields appear to exist in current builds. If the real shape differs,
// update this struct and the build_terminal_event mapping above.
// ============================================================

#[derive(serde::Deserialize)]
struct ClaudeCodeRawSession {
    session_id: String,
    #[serde(default)]
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    ended_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tokens_in: Option<u64>,
    #[serde(default)]
    tokens_out: Option<u64>,
    #[serde(default)]
    tool_version: Option<String>,
    #[serde(default)]
    artifacts: Option<Vec<String>>,
    #[serde(default)]
    summary: Option<String>,
}

impl ClaudeCodeRawSession {
    fn started_at_str(&self) -> String {
        self.started_at.map(|d| d.to_rfc3339()).unwrap_or_default()
    }
}

// ============================================================
// Helpers — interfaces only; bodies are Week 1 work
// ============================================================

/// Derive an idempotent event_id from (tool, session_id, started_at).
/// Reparsing the same source MUST yield the same event_id.
///
/// Week 1 decision: ULID vs UUIDv7. Both are sortable and time-prefixed.
/// ULID has wider Rust ecosystem support today; UUIDv7 is the emerging
/// standard. Pick one in the spike, commit, document in the schema spec
/// changelog.
///
/// Likely implementation: hash(tool || session_id || started_at) →
/// 128-bit ID with time-prefix from started_at.
fn derive_event_id(tool: &str, session_id: &str, started_at: &str) -> String {
    let _ = (tool, session_id, started_at);
    todo!("Week 1: pick ULID or UUIDv7, implement deterministic derivation")
}

/// Reads projects.yaml, applies prefix-match resolution. Unmapped cwds
/// return the fallback value (default "Uncategorized").
pub struct ProjectResolver {
    // mappings: Vec<(PathBuf, String)>,
    // fallback: String,
}

impl ProjectResolver {
    pub fn resolve(&self, cwd: &str) -> String {
        let _ = cwd;
        todo!("Week 1: load projects.yaml, normalize cwd (tilde + symlinks), first-prefix-match")
    }
}

/// Reads pricing.yaml, computes USD cost from token counts.
pub struct PricingTable {
    // entries: Vec<PricingEntry>,
}

impl PricingTable {
    pub fn lookup_cost(
        &self,
        provider: &str,
        model: &str,
        tokens_in: u64,
        tokens_out: u64,
    ) -> Option<f64> {
        let _ = (provider, model, tokens_in, tokens_out);
        todo!("Week 1: lookup (provider, model) in pricing.yaml, apply input/output rates")
    }
}

// ============================================================
// Tests — skeleton; real fixtures and assertions land Week 1
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_session_completed_fixture() {
        let fixture = PathBuf::from("tests/fixtures/claude-code-session-example.json");
        // let adapter = ClaudeCodeAdapter { project_resolver: /* test */, pricing: /* test */ };
        // let results = adapter.parse(&fixture);
        // assert_eq!(results.len(), 1);
        // assert!(matches!(results[0], ParseResult::Ok(_)));
        let _ = fixture;
    }

    #[test]
    fn fatal_on_unparseable_json() {
        // Feed malformed JSON; expect ParseResult::Fatal with reason containing "Unparseable JSON"
    }

    #[test]
    fn recoverable_on_missing_optional_fields() {
        // Valid JSON missing model/tokens; expect ParseResult::Recoverable with warnings
    }

    #[test]
    fn idempotent_event_id_across_reparses() {
        // Parse same fixture twice, assert event_ids match
    }

    #[test]
    fn unknown_outcome_value_recoverable_not_fatal() {
        // Source has outcome: "something-weird"; expect Recoverable with status=unknown
    }

    #[test]
    fn missing_session_id_is_fatal() {
        // Source missing session_id; expect ParseResult::Fatal
    }
}
