//! project_resolver — map an absolute cwd path to a user-defined project name.
//!
//! Reads `projects.yaml` (path is configurable; default is the repo root). First
//! prefix-match wins. Tilde is expanded, trailing slashes normalized. Path-boundary
//! safety is enforced: prefix `/foo` matches `/foo` and `/foo/bar` but NOT `/foobar`.
//! Unmapped cwd → the user-defined `fallback` value (default `"Uncategorized"`).
//!
//! Format of projects.yaml:
//! ```yaml
//! schema_version: 0.1.0
//! projects:
//!   - name: Paperclip-Workflow-Beta
//!     cwd_prefix: ~/Desktop/Projects/Paperclip-Workflow-Beta
//!   - name: orchestrator
//!     cwd_prefix: ~/.claude/orchestrator
//! fallback: Uncategorized
//! ```
//!
//! Symlink resolution is intentionally NOT done here — adapters often see cwd
//! values that don't exist on disk (deleted projects, network drives,
//! sessions captured on another machine). The resolver works on the string
//! representation. If real-disk normalization is needed later, do it at the
//! adapter layer before calling resolve().

use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

const DEFAULT_FALLBACK: &str = "Uncategorized";

#[derive(Debug, Deserialize, Clone)]
struct ProjectMapping {
    name: String,
    cwd_prefix: String,
}

#[derive(Debug, Deserialize)]
struct ProjectsConfig {
    #[serde(default)]
    #[allow(dead_code)]
    schema_version: Option<String>,
    projects: Vec<ProjectMapping>,
    #[serde(default = "default_fallback")]
    fallback: String,
}

fn default_fallback() -> String {
    DEFAULT_FALLBACK.to_string()
}

#[derive(Debug, Error)]
pub enum ProjectResolverError {
    #[error("IO error reading projects.yaml at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("YAML parse error in projects.yaml: {0}")]
    Parse(#[from] serde_yml::Error),
}

#[derive(Debug, Clone)]
pub struct ProjectResolver {
    /// (normalized_prefix, project_name) tuples in load order. First match wins.
    mappings: Vec<(String, String)>,
    fallback: String,
}

impl ProjectResolver {
    /// Load from a projects.yaml file on disk.
    pub fn load_from_path(path: &Path) -> Result<Self, ProjectResolverError> {
        let contents = std::fs::read_to_string(path).map_err(|e| ProjectResolverError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_yaml(&contents)
    }

    /// Parse from a YAML string. Used directly in tests; load_from_path delegates here.
    pub fn from_yaml(yaml: &str) -> Result<Self, ProjectResolverError> {
        let config: ProjectsConfig = serde_yml::from_str(yaml)?;
        let mappings = config
            .projects
            .into_iter()
            .map(|m| (normalize_path(&m.cwd_prefix), m.name))
            .collect();
        Ok(ProjectResolver {
            mappings,
            fallback: config.fallback,
        })
    }

    /// Empty resolver — useful when projects.yaml is missing. Everything goes
    /// to fallback ("Uncategorized"). Adapters should still function.
    pub fn empty() -> Self {
        Self {
            mappings: Vec::new(),
            fallback: DEFAULT_FALLBACK.to_string(),
        }
    }

    /// Map cwd → project name. First prefix-match wins; unmapped → fallback.
    pub fn resolve(&self, cwd: &str) -> String {
        let normalized = normalize_path(cwd);
        for (prefix, name) in &self.mappings {
            if path_starts_with(&normalized, prefix) {
                return name.clone();
            }
        }
        self.fallback.clone()
    }

    /// Read-only access to the fallback value, mainly for debug UI.
    pub fn fallback(&self) -> &str {
        &self.fallback
    }
}

/// Normalize a path string: expand tilde, strip trailing slash. Does NOT touch
/// the filesystem — pure string transformation, safe for paths that don't exist.
fn normalize_path(p: &str) -> String {
    let expanded = expand_tilde(p);
    expanded.trim_end_matches('/').to_string()
}

/// Tilde expansion. `~` → $HOME; `~/foo` → $HOME/foo. Other forms (`~user/foo`)
/// are NOT expanded — too rare to bother with in V1; revisit if real logs hit it.
fn expand_tilde(p: &str) -> String {
    if p == "~" {
        return home_dir();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        let home = home_dir();
        return format!("{}/{}", home.trim_end_matches('/'), rest);
    }
    p.to_string()
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

/// Path-boundary-safe starts_with. Prevents `/foobar` from matching prefix `/foo`.
/// Returns true iff `candidate` is equal to `prefix` or begins with `prefix` + '/'.
fn path_starts_with(candidate: &str, prefix: &str) -> bool {
    if !candidate.starts_with(prefix) {
        return false;
    }
    let rest = &candidate[prefix.len()..];
    rest.is_empty() || rest.starts_with('/')
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
schema_version: 0.1.0
projects:
  - name: Paperclip-Workflow-Beta
    cwd_prefix: /Users/clay/Desktop/Projects/Paperclip-Workflow-Beta
  - name: orchestrator
    cwd_prefix: /Users/clay/.claude/orchestrator
  - name: ClaudeCode-workspace
    cwd_prefix: /Users/clay/Desktop/Projects/Claude Code
fallback: Uncategorized
"#;

    fn fixture() -> ProjectResolver {
        ProjectResolver::from_yaml(SAMPLE_YAML).expect("fixture YAML must parse")
    }

    #[test]
    fn exact_match_returns_project_name() {
        let r = fixture();
        assert_eq!(
            r.resolve("/Users/clay/Desktop/Projects/Paperclip-Workflow-Beta"),
            "Paperclip-Workflow-Beta"
        );
    }

    #[test]
    fn nested_subdir_match_returns_project_name() {
        let r = fixture();
        assert_eq!(
            r.resolve("/Users/clay/Desktop/Projects/Paperclip-Workflow-Beta/src/agents"),
            "Paperclip-Workflow-Beta"
        );
    }

    #[test]
    fn unmapped_cwd_returns_fallback() {
        let r = fixture();
        assert_eq!(
            r.resolve("/Users/clay/Some/Unmapped/Path"),
            "Uncategorized"
        );
    }

    #[test]
    fn trailing_slash_normalized() {
        // Both the prefix in YAML and the cwd input may have trailing slashes;
        // normalization makes them comparable.
        let r = fixture();
        assert_eq!(
            r.resolve("/Users/clay/.claude/orchestrator/"),
            "orchestrator"
        );
    }

    #[test]
    fn path_boundary_safety_no_substring_match() {
        // Critical: prefix /Users/clay/.claude/orchestrator must NOT match
        // /Users/clay/.claude/orchestrator-backup. This is the regression test
        // for the classic naive startsWith bug.
        let r = fixture();
        assert_eq!(
            r.resolve("/Users/clay/.claude/orchestrator-backup"),
            "Uncategorized",
            "prefix must match on path boundary, not raw substring"
        );
    }

    #[test]
    fn first_match_wins() {
        // When two prefixes both match a cwd (one nests the other), the first
        // in YAML order wins. Document the rule so users can order their YAML
        // most-specific first.
        let yaml = r#"
projects:
  - name: Inner
    cwd_prefix: /a/b/c
  - name: Outer
    cwd_prefix: /a
fallback: Uncategorized
"#;
        let r = ProjectResolver::from_yaml(yaml).unwrap();
        assert_eq!(r.resolve("/a/b/c/file"), "Inner");
        assert_eq!(r.resolve("/a/b/other"), "Outer");
        assert_eq!(r.resolve("/a/other"), "Outer");
    }

    #[test]
    fn tilde_in_prefix_is_expanded() {
        let yaml = r#"
projects:
  - name: MyProject
    cwd_prefix: ~/Code/myproject
fallback: Uncategorized
"#;
        let r = ProjectResolver::from_yaml(yaml).unwrap();
        let home = home_dir();
        let cwd = format!("{}/Code/myproject", home.trim_end_matches('/'));
        assert_eq!(r.resolve(&cwd), "MyProject");
    }

    #[test]
    fn tilde_in_cwd_is_expanded() {
        // Symmetric: if the cwd from a session log somehow has a tilde (rare but
        // some tools log raw shell expansions), it should still resolve.
        let yaml = r#"
projects:
  - name: MyProject
    cwd_prefix: ~/Code/myproject
fallback: Uncategorized
"#;
        let r = ProjectResolver::from_yaml(yaml).unwrap();
        assert_eq!(r.resolve("~/Code/myproject"), "MyProject");
        assert_eq!(r.resolve("~/Code/myproject/src"), "MyProject");
    }

    #[test]
    fn empty_resolver_always_returns_uncategorized() {
        let r = ProjectResolver::empty();
        assert_eq!(r.resolve("/anywhere"), "Uncategorized");
        assert_eq!(r.resolve(""), "Uncategorized");
    }

    #[test]
    fn malformed_yaml_returns_parse_error() {
        let result = ProjectResolver::from_yaml("this is not: yaml: [it has: nested colons");
        assert!(
            result.is_err(),
            "malformed YAML must return Err, not silently succeed"
        );
    }

    #[test]
    fn missing_fallback_uses_default() {
        let yaml = r#"
projects:
  - name: One
    cwd_prefix: /one
"#;
        let r = ProjectResolver::from_yaml(yaml).unwrap();
        assert_eq!(r.fallback(), "Uncategorized");
        assert_eq!(r.resolve("/unmapped"), "Uncategorized");
    }

    #[test]
    fn loads_from_real_projects_yaml_at_repo_root() {
        // Smoke-test against the actual projects.yaml shipped with Lens.
        // Verifies the parser handles real-world YAML, not just curated fixtures.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("projects.yaml");
        let r = ProjectResolver::load_from_path(&path)
            .expect("real projects.yaml at repo root must parse");
        // At least one mapping should be defined (the user has projects).
        // We don't assert on specific values to avoid making the test brittle
        // when the user adds/removes projects.
        assert_eq!(r.fallback(), "Uncategorized");
    }
}
