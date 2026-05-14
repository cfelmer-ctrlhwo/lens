//! ingestion — wraps adapters + storage into the V1 ingestion pipeline.
//!
//! V1 implements **backfill** mode only: scan all eligible files in the
//! configured watch roots, parse each via the appropriate adapter, route
//! the results to storage (Ok/Recoverable → upsert_event; Fatal → record_issue).
//!
//! Watch mode (notify-based fs events + per-file debounce) is V1.x. The
//! `watch()` method exists with an unimplemented! body so the public API
//! is shaped correctly for callers; flip the implementation when needed.
//!
//! Lane B status: built directly in main (parallel Codex run stalled
//! without writing files). V1 polling-driven backfill is sufficient for
//! Lens's interactive use — the user re-runs Lens periodically and each
//! launch backfills new events.

pub mod pipeline;
pub mod walker;

pub use pipeline::{process_file, FileReport, PipelineError};
pub use walker::{walk_candidates, WalkerConfig};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::adapters::claude_code::Adapter;
use crate::storage::Database;

/// Public ingestion entry-point. Wraps a Database, a set of adapters, and
/// the walker config. Call `backfill()` to ingest everything matching the
/// config in one shot.
pub struct IngestionPipeline {
    db: Arc<Mutex<Database>>,
    adapters: Vec<Box<dyn Adapter + Send + Sync>>,
    config: PipelineConfig,
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Roots to scan recursively. Adapters discover their own subdirectories
    /// under these — V1 only has Claude Code, which reads `~/.claude/projects/`.
    pub watch_roots: Vec<PathBuf>,
    /// Walker config (subdirectory skip rules, mtime threshold, extension filter).
    pub walker: WalkerConfig,
}

impl PipelineConfig {
    /// Reasonable defaults for V1: watch `~/.claude/projects/` only (Claude Code
    /// adapter's source), skip subagents, skip files modified within last hour.
    pub fn defaults_for_claude_code() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        Self {
            watch_roots: vec![PathBuf::from(format!("{}/.claude/projects", home))],
            walker: WalkerConfig::default(),
        }
    }
}

/// Aggregate report for one backfill run. Surfaced to the IPC layer so the UI
/// can show "Last backfill: 1,247 events ingested, 3 issues" type messages.
#[derive(Debug, Default, Clone)]
pub struct BackfillReport {
    pub files_scanned: usize,
    pub files_skipped_active: usize,
    pub files_skipped_subagent: usize,
    pub events_inserted: usize,
    pub events_updated: usize,
    pub events_unchanged: usize,
    pub recoverable_issues: usize,
    pub fatal_issues: usize,
}

impl IngestionPipeline {
    pub fn new(
        db: Arc<Mutex<Database>>,
        adapters: Vec<Box<dyn Adapter + Send + Sync>>,
        config: PipelineConfig,
    ) -> Self {
        Self { db, adapters, config }
    }

    /// One-shot backfill: walk all configured roots, parse every eligible file,
    /// ingest into storage. Returns aggregate counts. Safe to re-run anytime
    /// (idempotent via R1A UPSERT-on-content-change).
    pub fn backfill(&self) -> Result<BackfillReport, PipelineError> {
        let mut report = BackfillReport::default();

        for root in &self.config.watch_roots {
            let walk = walker::walk_candidates(root, &self.config.walker);
            report.files_skipped_active += walk.skipped_active;
            report.files_skipped_subagent += walk.skipped_substring;
            for path in walk.candidates {
                report.files_scanned += 1;
                let db_guard = self
                    .db
                    .lock()
                    .map_err(|e| PipelineError::Internal(format!("db lock poisoned: {}", e)))?;
                let file_report = pipeline::process_file(&db_guard, &self.adapters, &path)?;
                drop(db_guard); // release lock between files

                report.events_inserted += file_report.events_inserted;
                report.events_updated += file_report.events_updated;
                report.events_unchanged += file_report.events_unchanged;
                report.recoverable_issues += file_report.recoverable_issues;
                report.fatal_issues += file_report.fatal_issues;
            }
        }

        Ok(report)
    }

    /// Watch mode (V1.x). Currently unimplemented; backfill is the V1 way.
    /// Designed signature for when this lands: notify-rs watcher + per-file
    /// debounce on fs events, calls process_file when debounce fires.
    pub async fn watch(&self) -> Result<(), PipelineError> {
        Err(PipelineError::NotImplemented(
            "watch mode is V1.x; use backfill() in V1".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::claude_code::{ClaudeCodeAdapter, ParseResult};
    use crate::pricing::PricingTable;
    use crate::project_resolver::ProjectResolver;
    use std::sync::{Arc, Mutex};

    fn make_pipeline_with_root(root: PathBuf) -> IngestionPipeline {
        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter {
            project_resolver: ProjectResolver::empty(),
            pricing: PricingTable::empty(),
        };
        let config = PipelineConfig {
            watch_roots: vec![root],
            walker: WalkerConfig::default(),
        };
        IngestionPipeline::new(
            Arc::new(Mutex::new(db)),
            vec![Box::new(adapter)],
            config,
        )
    }

    #[test]
    fn pipeline_constructs_with_defaults() {
        let cfg = PipelineConfig::defaults_for_claude_code();
        assert_eq!(cfg.watch_roots.len(), 1);
        assert!(cfg.watch_roots[0].to_string_lossy().contains(".claude/projects"));
    }

    #[test]
    fn backfill_on_empty_root_returns_zero_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = make_pipeline_with_root(tmp.path().to_path_buf());
        let report = pipeline.backfill().unwrap();
        assert_eq!(report.files_scanned, 0);
        assert_eq!(report.events_inserted, 0);
        assert_eq!(report.fatal_issues, 0);
    }

    #[test]
    fn backfill_processes_a_real_jsonl_file() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a Claude Code-shaped fixture file with a valid session
        let fixture_dir = tmp.path().join("-Users-test-Projects-Demo");
        std::fs::create_dir_all(&fixture_dir).unwrap();
        let fixture_path = fixture_dir.join("sess-abc.jsonl");
        let content = concat!(
            r#"{"type":"user","sessionId":"s","timestamp":"2026-05-14T17:00:00Z","cwd":"/x","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-05-14T17:00:30Z","message":{"role":"assistant","id":"m1","model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50},"content":[{"type":"text","text":"ok"}]}}"#,
        );
        std::fs::write(&fixture_path, content).unwrap();
        // Backdate mtime so the active-file filter doesn't skip it
        set_old_mtime(&fixture_path);

        let pipeline = make_pipeline_with_root(tmp.path().to_path_buf());
        let report = pipeline.backfill().unwrap();
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.events_inserted, 1);
    }

    #[test]
    fn backfill_is_idempotent_via_upsert() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture_path = tmp.path().join("sess-id.jsonl");
        let content = concat!(
            r#"{"type":"user","sessionId":"s","timestamp":"2026-05-14T17:00:00Z","cwd":"/x","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-05-14T17:00:30Z","message":{"role":"assistant","id":"m1","model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50},"content":[{"type":"text","text":"ok"}]}}"#,
        );
        std::fs::write(&fixture_path, content).unwrap();
        set_old_mtime(&fixture_path);

        let pipeline = make_pipeline_with_root(tmp.path().to_path_buf());
        let r1 = pipeline.backfill().unwrap();
        let r2 = pipeline.backfill().unwrap();
        assert_eq!(r1.events_inserted, 1);
        assert_eq!(r2.events_unchanged, 1, "second backfill must be a no-op");
        assert_eq!(r2.events_inserted, 0);
    }

    #[test]
    fn backfill_records_fatal_issue_for_garbage_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture_path = tmp.path().join("bad.jsonl");
        std::fs::write(&fixture_path, "this is not json\nstill not json\n").unwrap();
        set_old_mtime(&fixture_path);

        let pipeline = make_pipeline_with_root(tmp.path().to_path_buf());
        let report = pipeline.backfill().unwrap();
        assert_eq!(report.fatal_issues, 1);
        assert_eq!(report.events_inserted, 0);
    }

    #[test]
    fn watch_mode_returns_not_implemented_in_v1() {
        let pipeline = make_pipeline_with_root(std::path::PathBuf::from("/tmp"));
        let result = futures_block_on(pipeline.watch());
        assert!(matches!(result, Err(PipelineError::NotImplemented(_))));
    }

    /// Helper: backdate a file's mtime to bypass the active-session filter
    /// (default 3600s). Sets to ~2 hours ago.
    fn set_old_mtime(path: &std::path::Path) {
        let two_hours_ago = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(7200))
            .unwrap();
        let _ = filetime::set_file_mtime(path, filetime::FileTime::from_system_time(two_hours_ago));
        // If filetime isn't available, the test may still pass on systems where
        // mtime is set to creation time (which is "now") but that's a problem
        // for the active-file filter. The conditional dependency below.
    }

    /// Block on a future without pulling in full tokio runtime for these
    /// synchronous tests.
    fn futures_block_on<F: std::future::Future>(future: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(future)
    }
}
