//! issues — write + query for the ingestion_issues table.
//!
//! Per R2B: parser errors live in a separate table from events. Timeline queries
//! never accidentally include parser_error rows; the sidebar badge queries this
//! table independently and joins on project.
//!
//! Two severity levels mirror the adapter's ParseResult enum:
//!   - Fatal: the record could not be placed on a timeline at all
//!   - Recoverable: a best-effort event was emitted, but with warnings worth
//!     surfacing (unknown event_type values, missing optional fields, etc.)

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::storage::db::{Database, DbError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Fatal,
    Recoverable,
}

impl IssueSeverity {
    fn as_str(&self) -> &'static str {
        match self {
            IssueSeverity::Fatal => "fatal",
            IssueSeverity::Recoverable => "recoverable",
        }
    }
}

/// Input for record_issue. Adapter ingestion code constructs these and hands
/// off to the writer task.
#[derive(Debug, Clone)]
pub struct IngestionIssue {
    /// Which adapter emitted the issue (claude-code, codex-cli, etc.) — for filtering.
    pub adapter: String,
    /// The source file path that failed to parse. Useful for "click to see" in UI.
    pub source_path: Option<String>,
    /// Resolved project name (or None if cwd wasn't extractable from the bad record).
    pub project: Option<String>,
    pub severity: IssueSeverity,
    /// Human-readable explanation. Stays terse; full diagnostic detail
    /// belongs in logs, not the UI badge.
    pub reason: String,
    /// Version of the adapter that produced the issue. Helps when a parser
    /// update changes which records are considered fatal.
    pub parser_version: Option<String>,
}

/// One row out of ingestion_issues, with the DB-assigned id + timestamp.
#[derive(Debug, Clone, Serialize)]
pub struct StoredIssue {
    pub issue_id: i64,
    pub occurred_at: String,
    pub adapter: String,
    pub source_path: Option<String>,
    pub project: Option<String>,
    pub severity: IssueSeverity,
    pub reason: String,
    pub parser_version: Option<String>,
}

/// Insert a new ingestion issue. Returns the auto-assigned issue_id.
pub fn record_issue(db: &Database, issue: &IngestionIssue) -> Result<i64, DbError> {
    db.conn().execute(
        "INSERT INTO ingestion_issues (adapter, source_path, project, severity, reason, parser_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            issue.adapter,
            issue.source_path,
            issue.project,
            issue.severity.as_str(),
            issue.reason,
            issue.parser_version,
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// Count issues per project. Drives the sidebar badge in the UI.
/// Treats NULL project values as 'Uncategorized' so they aggregate cleanly.
pub fn count_issues_per_project(db: &Database) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = db.conn().prepare(
        "SELECT COALESCE(project, 'Uncategorized') AS project, COUNT(*) AS issue_count
         FROM ingestion_issues
         GROUP BY COALESCE(project, 'Uncategorized')
         ORDER BY issue_count DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Recent issues, newest-first. Drives the "Show ingestion errors" panel.
pub fn recent_issues(db: &Database, limit: u64) -> Result<Vec<StoredIssue>, DbError> {
    let mut stmt = db.conn().prepare(
        "SELECT issue_id, occurred_at, adapter, source_path, project, severity, reason, parser_version
         FROM ingestion_issues
         ORDER BY occurred_at DESC, issue_id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        let severity_str: String = r.get(5)?;
        let severity = match severity_str.as_str() {
            "fatal" => IssueSeverity::Fatal,
            "recoverable" => IssueSeverity::Recoverable,
            // CHECK constraint guarantees this branch is unreachable, but
            // be defensive against schema drift.
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    format!("unknown severity '{}'", other).into(),
                ));
            }
        };
        Ok(StoredIssue {
            issue_id: r.get(0)?,
            occurred_at: r.get(1)?,
            adapter: r.get(2)?,
            source_path: r.get(3)?,
            project: r.get(4)?,
            severity,
            reason: r.get(6)?,
            parser_version: r.get(7)?,
        })
    })?;
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

    fn sample_fatal(adapter: &str, project: Option<&str>) -> IngestionIssue {
        IngestionIssue {
            adapter: adapter.into(),
            source_path: Some(format!("~/.{}/projects/test.jsonl", adapter)),
            project: project.map(|s| s.to_string()),
            severity: IssueSeverity::Fatal,
            reason: "Unparseable JSON at line 42".into(),
            parser_version: Some("0.1.0".into()),
        }
    }

    #[test]
    fn record_issue_returns_monotonic_id() {
        let db = Database::open_in_memory().unwrap();
        let id1 = record_issue(&db, &sample_fatal("claude-code", Some("Lens"))).unwrap();
        let id2 = record_issue(&db, &sample_fatal("claude-code", Some("Lens"))).unwrap();
        assert!(id2 > id1, "issue_ids must be monotonic");
    }

    #[test]
    fn recoverable_severity_accepted() {
        let db = Database::open_in_memory().unwrap();
        let issue = IngestionIssue {
            severity: IssueSeverity::Recoverable,
            reason: "Unknown event_type value 'foo'; treated as status=unknown".into(),
            ..sample_fatal("codex-cli", Some("Understdy"))
        };
        let id = record_issue(&db, &issue).unwrap();
        assert!(id >= 1);
    }

    #[test]
    fn count_issues_per_project_groups_correctly() {
        let db = Database::open_in_memory().unwrap();
        record_issue(&db, &sample_fatal("claude-code", Some("Lens"))).unwrap();
        record_issue(&db, &sample_fatal("claude-code", Some("Lens"))).unwrap();
        record_issue(&db, &sample_fatal("codex-cli", Some("Understdy"))).unwrap();
        record_issue(&db, &sample_fatal("codex-cli", None)).unwrap();

        let counts = count_issues_per_project(&db).unwrap();
        // Order: by COUNT DESC, so Lens (2) should be first
        assert_eq!(counts[0], ("Lens".to_string(), 2));
        // Understdy and Uncategorized both have 1; either order is fine
        let one_each: Vec<_> = counts[1..].iter().collect();
        assert_eq!(one_each.len(), 2);
        assert!(one_each.iter().any(|(p, c)| p == "Understdy" && *c == 1));
        assert!(one_each.iter().any(|(p, c)| p == "Uncategorized" && *c == 1));
    }

    #[test]
    fn null_project_becomes_uncategorized_in_count() {
        let db = Database::open_in_memory().unwrap();
        record_issue(&db, &sample_fatal("claude-code", None)).unwrap();
        let counts = count_issues_per_project(&db).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].0, "Uncategorized");
    }

    #[test]
    fn recent_issues_returns_newest_first_with_limit() {
        let db = Database::open_in_memory().unwrap();
        for i in 0..5 {
            let mut issue = sample_fatal("claude-code", Some("Lens"));
            issue.reason = format!("Issue number {}", i);
            record_issue(&db, &issue).unwrap();
        }
        let recent = recent_issues(&db, 3).unwrap();
        assert_eq!(recent.len(), 3);
        // Highest issue_id (5) comes first when timestamps tie
        assert_eq!(recent[0].issue_id, 5);
        assert_eq!(recent[1].issue_id, 4);
        assert_eq!(recent[2].issue_id, 3);
    }

    #[test]
    fn recent_issues_round_trips_severity() {
        let db = Database::open_in_memory().unwrap();
        record_issue(&db, &sample_fatal("claude-code", Some("Lens"))).unwrap();
        let issue = IngestionIssue {
            severity: IssueSeverity::Recoverable,
            ..sample_fatal("codex-cli", Some("Understdy"))
        };
        record_issue(&db, &issue).unwrap();

        let recent = recent_issues(&db, 10).unwrap();
        assert_eq!(recent.len(), 2);
        // Whichever order, both severities must round-trip correctly
        let severities: Vec<_> = recent.iter().map(|i| &i.severity).collect();
        assert!(severities.contains(&&IssueSeverity::Fatal));
        assert!(severities.contains(&&IssueSeverity::Recoverable));
    }

    #[test]
    fn recent_issues_preserves_source_path_and_parser_version() {
        let db = Database::open_in_memory().unwrap();
        record_issue(&db, &sample_fatal("claude-code", Some("Lens"))).unwrap();
        let recent = recent_issues(&db, 1).unwrap();
        let issue = &recent[0];
        assert_eq!(issue.adapter, "claude-code");
        assert!(issue.source_path.is_some());
        assert_eq!(issue.parser_version.as_deref(), Some("0.1.0"));
        assert!(issue.reason.contains("Unparseable"));
    }
}
