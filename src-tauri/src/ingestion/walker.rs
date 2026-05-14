//! walker — filesystem walker for the ingestion pipeline.
//!
//! Recursively visits each configured root, yielding candidate files that
//! match the adapter's expected extensions, skipping:
//!   - Any path component in `skip_path_components` (default: "subagents")
//!   - Files modified within the last `active_threshold_secs` seconds
//!     (per PORT-REFERENCE §4: in-progress sessions risk UPSERT thrash)
//!
//! Returns a WalkReport with the candidates plus skip counts (for the
//! BackfillReport aggregate).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct WalkerConfig {
    /// Path components that, if present anywhere in a file's path, cause
    /// it to be skipped. Default: `["subagents"]`.
    pub skip_path_components: Vec<String>,
    /// Files modified within this many seconds of NOW are skipped (likely
    /// still being written by an active session). Default: 3600 (1 hour).
    pub active_threshold_secs: u64,
    /// File extensions to accept (without the leading dot). Default: `["jsonl"]`.
    /// Codex stores `.json` too — when the Codex adapter ships, that lane
    /// extends this list.
    pub extensions: Vec<String>,
}

impl Default for WalkerConfig {
    fn default() -> Self {
        Self {
            skip_path_components: vec!["subagents".to_string()],
            active_threshold_secs: 3600,
            extensions: vec!["jsonl".to_string()],
        }
    }
}

#[derive(Debug, Default)]
pub struct WalkReport {
    pub candidates: Vec<PathBuf>,
    pub skipped_active: usize,
    pub skipped_substring: usize,
}

/// Recursively walk `root` and gather matching files. Silent on IO errors —
/// individual filesystem failures don't abort the whole walk (skipped instead).
pub fn walk_candidates(root: &Path, config: &WalkerConfig) -> WalkReport {
    let mut report = WalkReport::default();
    let now = SystemTime::now();
    walk_recursive(root, config, now, &mut report);
    report
}

fn walk_recursive(
    dir: &Path,
    config: &WalkerConfig,
    now: SystemTime,
    report: &mut WalkReport,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // permission denied, doesn't exist, etc. Silent skip.
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            walk_recursive(&path, config, now, report);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        // Skip if any path component matches the skip set
        let skip = config.skip_path_components.iter().any(|skip| {
            path.components()
                .any(|c| c.as_os_str().to_string_lossy() == skip.as_str())
        });
        if skip {
            report.skipped_substring += 1;
            continue;
        }

        // Skip if extension doesn't match
        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| config.extensions.iter().any(|allowed| allowed == e))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }

        // Skip if mtime is within active_threshold_secs of NOW
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                if let Ok(age) = now.duration_since(mtime) {
                    if age.as_secs() < config.active_threshold_secs {
                        report.skipped_active += 1;
                        continue;
                    }
                }
            }
        }

        report.candidates.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn write_with_old_mtime(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        let _ = filetime::set_file_mtime(
            path,
            filetime::FileTime::from_system_time(two_hours_ago),
        );
    }

    #[test]
    fn finds_jsonl_files_in_root() {
        let tmp = tempfile::tempdir().unwrap();
        write_with_old_mtime(&tmp.path().join("a.jsonl"), "{}");
        write_with_old_mtime(&tmp.path().join("b.jsonl"), "{}");
        let report = walk_candidates(tmp.path(), &WalkerConfig::default());
        assert_eq!(report.candidates.len(), 2);
    }

    #[test]
    fn finds_jsonl_in_nested_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("-Users-foo-Projects-Bar");
        std::fs::create_dir_all(&sub).unwrap();
        write_with_old_mtime(&sub.join("sess.jsonl"), "{}");
        let report = walk_candidates(tmp.path(), &WalkerConfig::default());
        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].ends_with("sess.jsonl"));
    }

    #[test]
    fn skips_subagents_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let subagents = project.join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();

        write_with_old_mtime(&project.join("main.jsonl"), "{}");
        write_with_old_mtime(&subagents.join("agent.jsonl"), "{}");

        let report = walk_candidates(tmp.path(), &WalkerConfig::default());
        assert_eq!(report.candidates.len(), 1, "subagents file must be skipped");
        assert!(report.candidates[0].ends_with("main.jsonl"));
        assert_eq!(report.skipped_substring, 1);
    }

    #[test]
    fn skips_files_modified_within_active_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        // Fresh file (mtime = now)
        std::fs::write(tmp.path().join("active.jsonl"), "{}").unwrap();
        // Old file
        write_with_old_mtime(&tmp.path().join("old.jsonl"), "{}");

        let report = walk_candidates(tmp.path(), &WalkerConfig::default());
        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].ends_with("old.jsonl"));
        assert_eq!(report.skipped_active, 1);
    }

    #[test]
    fn ignores_non_jsonl_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_with_old_mtime(&tmp.path().join("data.jsonl"), "{}");
        write_with_old_mtime(&tmp.path().join("readme.md"), "x");
        write_with_old_mtime(&tmp.path().join("config.json"), "{}");
        let report = walk_candidates(tmp.path(), &WalkerConfig::default());
        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].ends_with("data.jsonl"));
    }

    #[test]
    fn extension_filter_is_configurable() {
        let tmp = tempfile::tempdir().unwrap();
        write_with_old_mtime(&tmp.path().join("a.jsonl"), "{}");
        write_with_old_mtime(&tmp.path().join("b.json"), "{}");
        let mut config = WalkerConfig::default();
        config.extensions = vec!["jsonl".into(), "json".into()];
        let report = walk_candidates(tmp.path(), &config);
        assert_eq!(report.candidates.len(), 2);
    }

    #[test]
    fn nonexistent_root_returns_empty_silently() {
        let report = walk_candidates(Path::new("/this/does/not/exist/anywhere"), &WalkerConfig::default());
        assert_eq!(report.candidates.len(), 0);
        assert_eq!(report.skipped_active, 0);
        assert_eq!(report.skipped_substring, 0);
    }
}
