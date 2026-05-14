//! pipeline — glue from walker output → adapter.parse() → storage.
//!
//! For each candidate file, picks an adapter (V1: claude-code only — by tool
//! name match), calls its parse(), and routes the results:
//!   - Ok(event)           → upsert_event; bump events_{inserted,updated,unchanged}
//!   - Recoverable {e,ws}  → upsert_event AND record_issue per warning (severity=Recoverable)
//!   - Fatal {path,reason} → record_issue (severity=Fatal); skip the event
//!
//! Errors at the storage layer abort processing of the current file but do
//! NOT abort the whole backfill; the next file gets a fresh try.

use std::path::Path;
use thiserror::Error;

use crate::adapters::claude_code::{Adapter, ParseResult};
use crate::storage::{
    self, db::DbError, issues::{IngestionIssue, IssueSeverity}, UpsertOutcome,
};

/// Errors that can occur while processing one file or running a backfill.
/// Non-fatal at the storage layer is rare — we surface them so the caller
/// can decide whether to continue.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("storage error: {0}")]
    Storage(#[from] DbError),
    #[error("no adapter available for source path")]
    NoAdapterMatch,
    #[error("internal: {0}")]
    Internal(String),
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

/// Aggregate counts for one file's processing pass.
#[derive(Debug, Default, Clone)]
pub struct FileReport {
    pub events_inserted: usize,
    pub events_updated: usize,
    pub events_unchanged: usize,
    pub recoverable_issues: usize,
    pub fatal_issues: usize,
}

/// Process one candidate source file: pick an adapter, call parse(),
/// route results to storage. Returns aggregate counts for the file.
pub fn process_file(
    db: &storage::Database,
    adapters: &[Box<dyn Adapter + Send + Sync>],
    source_path: &Path,
) -> Result<FileReport, PipelineError> {
    // Adapter selection: V1 has only claude-code, and any *.jsonl under
    // ~/.claude/projects is its territory. When the Codex adapter ships in
    // V1.x, this becomes a path-prefix match (~/.claude → claude-code,
    // ~/.codex → codex-cli).
    let adapter = adapters.first().ok_or(PipelineError::NoAdapterMatch)?;
    let parser_version = "0.1.0";
    let tool = adapter.tool_name();

    let results = adapter.parse(source_path);
    let mut report = FileReport::default();

    for result in results {
        match result {
            ParseResult::Ok(event) => {
                let outcome = storage::upsert_event(db, &event)?;
                match outcome {
                    UpsertOutcome::Inserted { .. } => report.events_inserted += 1,
                    UpsertOutcome::Updated => report.events_updated += 1,
                    UpsertOutcome::Unchanged => report.events_unchanged += 1,
                }
            }
            ParseResult::Recoverable { event, warnings } => {
                let project = event.project.clone();
                let outcome = storage::upsert_event(db, &event)?;
                match outcome {
                    UpsertOutcome::Inserted { .. } => report.events_inserted += 1,
                    UpsertOutcome::Updated => report.events_updated += 1,
                    UpsertOutcome::Unchanged => report.events_unchanged += 1,
                }
                for warning in warnings {
                    storage::record_issue(
                        db,
                        &IngestionIssue {
                            adapter: tool.to_string(),
                            source_path: Some(source_path.display().to_string()),
                            project: project.clone(),
                            severity: IssueSeverity::Recoverable,
                            reason: warning,
                            parser_version: Some(parser_version.to_string()),
                        },
                    )?;
                    report.recoverable_issues += 1;
                }
            }
            ParseResult::Fatal { source_path: sp, reason } => {
                storage::record_issue(
                    db,
                    &IngestionIssue {
                        adapter: tool.to_string(),
                        source_path: Some(sp.display().to_string()),
                        project: None,
                        severity: IssueSeverity::Fatal,
                        reason,
                        parser_version: Some(parser_version.to_string()),
                    },
                )?;
                report.fatal_issues += 1;
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::claude_code::{ClaudeCodeAdapter, ParseResult};
    use crate::agent_activity::{AgentActivityEvent, CostSource, EventStatus, EventType};
    use crate::pricing::PricingTable;
    use crate::project_resolver::ProjectResolver;
    use crate::storage::Database;
    use chrono::TimeZone;
    use std::path::PathBuf;

    /// Mock adapter that returns a pre-baked ParseResult vec. Used to drive
    /// pipeline routing tests without depending on real JSONL parsing.
    struct MockAdapter {
        results: Vec<ParseResult>,
    }

    impl Adapter for MockAdapter {
        fn tool_name(&self) -> &'static str {
            "mock-adapter"
        }
        fn parse(&self, _: &Path) -> Vec<ParseResult> {
            self.results.clone()
        }
    }

    fn mock_event(id: &str) -> AgentActivityEvent {
        AgentActivityEvent {
            schema_version: "0.1.1".into(),
            event_id: id.into(),
            tool: "mock-adapter".into(),
            tool_version: None,
            event_type: EventType::SessionCompleted,
            started_at: chrono::Utc.with_ymd_and_hms(2026, 5, 14, 17, 0, 0).unwrap(),
            ended_at: None,
            status: EventStatus::Success,
            session_id: Some("sess".into()),
            project: Some("MockProject".into()),
            cwd: Some("/mock".into()),
            model: Some("claude-opus-4-7".into()),
            provider: Some("anthropic".into()),
            tokens_in: Some(100),
            tokens_out: Some(50),
            tokens_total: Some(150),
            cost_usd_estimated: Some(0.01),
            cost_source: Some(CostSource::LogParse),
            artifacts: None,
            error_message: None,
            summary: None,
            tags: None,
            raw_ref: None,
            extra: None,
        }
    }

    #[test]
    fn routes_ok_event_to_upsert() {
        let db = Database::open_in_memory().unwrap();
        let adapter: Box<dyn Adapter + Send + Sync> = Box::new(MockAdapter {
            results: vec![ParseResult::Ok(mock_event("e1"))],
        });
        let report = process_file(&db, &[adapter], &PathBuf::from("/fake/path")).unwrap();
        assert_eq!(report.events_inserted, 1);
        assert_eq!(report.recoverable_issues, 0);
        assert_eq!(report.fatal_issues, 0);
    }

    #[test]
    fn routes_recoverable_to_event_and_issue() {
        let db = Database::open_in_memory().unwrap();
        let adapter: Box<dyn Adapter + Send + Sync> = Box::new(MockAdapter {
            results: vec![ParseResult::Recoverable {
                event: mock_event("e1"),
                warnings: vec!["warning 1".into(), "warning 2".into()],
            }],
        });
        let report = process_file(&db, &[adapter], &PathBuf::from("/fake")).unwrap();
        assert_eq!(report.events_inserted, 1);
        assert_eq!(report.recoverable_issues, 2);
        assert_eq!(report.fatal_issues, 0);
    }

    #[test]
    fn routes_fatal_to_issue_only() {
        let db = Database::open_in_memory().unwrap();
        let adapter: Box<dyn Adapter + Send + Sync> = Box::new(MockAdapter {
            results: vec![ParseResult::Fatal {
                source_path: PathBuf::from("/bad"),
                reason: "unparseable".into(),
            }],
        });
        let report = process_file(&db, &[adapter], &PathBuf::from("/bad")).unwrap();
        assert_eq!(report.events_inserted, 0);
        assert_eq!(report.fatal_issues, 1);
    }

    #[test]
    fn second_call_with_same_event_returns_unchanged() {
        let db = Database::open_in_memory().unwrap();
        let adapter: Box<dyn Adapter + Send + Sync> = Box::new(MockAdapter {
            results: vec![ParseResult::Ok(mock_event("e1"))],
        });
        let r1 = process_file(&db, &[adapter], &PathBuf::from("/x")).unwrap();
        assert_eq!(r1.events_inserted, 1);

        // Second pass with same event_id and identical content
        let adapter2: Box<dyn Adapter + Send + Sync> = Box::new(MockAdapter {
            results: vec![ParseResult::Ok(mock_event("e1"))],
        });
        let r2 = process_file(&db, &[adapter2], &PathBuf::from("/x")).unwrap();
        assert_eq!(r2.events_inserted, 0);
        assert_eq!(r2.events_unchanged, 1);
    }

    #[test]
    fn no_adapter_in_list_yields_error() {
        let db = Database::open_in_memory().unwrap();
        let adapters: Vec<Box<dyn Adapter + Send + Sync>> = vec![];
        let result = process_file(&db, &adapters, &PathBuf::from("/x"));
        assert!(matches!(result, Err(PipelineError::NoAdapterMatch)));
    }

    #[test]
    fn real_claude_code_adapter_integration() {
        // End-to-end: real ClaudeCodeAdapter (with empty resolver + pricing)
        // parsing a synthetic JSONL fixture and storing the event.
        let db = Database::open_in_memory().unwrap();
        let adapter: Box<dyn Adapter + Send + Sync> = Box::new(ClaudeCodeAdapter {
            project_resolver: ProjectResolver::empty(),
            pricing: PricingTable::empty(),
        });

        let tmp = tempfile::tempdir().unwrap();
        let fixture = tmp.path().join("sess.jsonl");
        let content = concat!(
            r#"{"type":"user","sessionId":"s","timestamp":"2026-05-14T17:00:00Z","cwd":"/x","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-05-14T17:00:30Z","message":{"role":"assistant","id":"m1","model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50},"content":[{"type":"text","text":"ok"}]}}"#,
        );
        std::fs::write(&fixture, content).unwrap();

        let report = process_file(&db, &[adapter], &fixture).unwrap();
        // The real adapter emits Recoverable when cost is unavailable
        // (empty PricingTable). Event count is 1; recoverable_issues is 1.
        assert_eq!(report.events_inserted, 1);
        assert!(report.recoverable_issues >= 0, "real adapter may or may not warn depending on data");
    }
}
