//! pricing — token-cost lookup table for AI model pricing.
//!
//! Reads `pricing.yaml`. Lookup is by (provider, model) tuple; computes USD cost
//! from input + output token counts.
//!
//! Schema of pricing.yaml:
//! ```yaml
//! schema_version: 0.1.0
//! last_updated: 2026-05-14
//! models:
//!   - provider: anthropic
//!     model: claude-opus-4-7
//!     input_per_million_usd: 15.00
//!     output_per_million_usd: 75.00
//!   - provider: local
//!     model: "*"            # wildcard — matches any model for this provider
//!     input_per_million_usd: 0.00
//!     output_per_million_usd: 0.00
//! ```
//!
//! Lookup precedence: exact (provider, model) match wins over (provider, "*")
//! wildcard. Unknown (provider, model) returns None — adapters set
//! `cost_source: "none"` and omit cost_usd_estimated.
//!
//! The `last_updated` field is exposed for UI staleness warnings (designed to
//! flash when prices are >30 days old). The struct doesn't enforce staleness;
//! that's a consumer policy.

use chrono::NaiveDate;
use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Deserialize, Clone)]
struct PricingEntry {
    provider: String,
    model: String,
    input_per_million_usd: f64,
    output_per_million_usd: f64,
}

#[derive(Debug, Deserialize)]
struct PricingConfig {
    #[serde(default)]
    #[allow(dead_code)]
    schema_version: Option<String>,
    #[serde(default)]
    last_updated: Option<NaiveDate>,
    models: Vec<PricingEntry>,
}

#[derive(Debug, Error)]
pub enum PricingError {
    #[error("IO error reading pricing.yaml at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("YAML parse error in pricing.yaml: {0}")]
    Parse(#[from] serde_yml::Error),
}

#[derive(Debug, Clone)]
pub struct PricingTable {
    entries: Vec<PricingEntry>,
    last_updated: Option<NaiveDate>,
}

impl PricingTable {
    /// Load from a pricing.yaml file path.
    pub fn load_from_path(path: &Path) -> Result<Self, PricingError> {
        let contents = std::fs::read_to_string(path).map_err(|e| PricingError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_yaml(&contents)
    }

    /// Parse from a YAML string. Used directly in tests; load_from_path delegates here.
    pub fn from_yaml(yaml: &str) -> Result<Self, PricingError> {
        let config: PricingConfig = serde_yml::from_str(yaml)?;
        Ok(PricingTable {
            entries: config.models,
            last_updated: config.last_updated,
        })
    }

    /// Empty table — useful when pricing.yaml is missing. All lookups return None.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            last_updated: None,
        }
    }

    /// Look up cost for (provider, model). Returns None if the tuple is unknown.
    ///
    /// Cost formula:
    ///   cost_usd = (tokens_in * input_per_million / 1_000_000)
    ///            + (tokens_out * output_per_million / 1_000_000)
    ///
    /// Matching precedence:
    ///   1. Exact (provider, model) match
    ///   2. (provider, "*") wildcard match for same provider
    pub fn lookup_cost(
        &self,
        provider: &str,
        model: &str,
        tokens_in: u64,
        tokens_out: u64,
    ) -> Option<f64> {
        // Pass 1: exact match
        for entry in &self.entries {
            if entry.provider == provider && entry.model == model {
                return Some(compute(entry, tokens_in, tokens_out));
            }
        }
        // Pass 2: wildcard match
        for entry in &self.entries {
            if entry.provider == provider && entry.model == "*" {
                return Some(compute(entry, tokens_in, tokens_out));
            }
        }
        None
    }

    /// Expose last_updated for the UI staleness banner (warn if >30 days stale).
    pub fn last_updated(&self) -> Option<NaiveDate> {
        self.last_updated
    }

    /// True if last_updated is more than `max_age_days` ago, OR last_updated is
    /// missing entirely (defensive: missing date = treat as stale).
    pub fn is_stale(&self, today: NaiveDate, max_age_days: i64) -> bool {
        match self.last_updated {
            Some(updated) => (today - updated).num_days() > max_age_days,
            None => true,
        }
    }
}

fn compute(entry: &PricingEntry, tokens_in: u64, tokens_out: u64) -> f64 {
    let per_million = 1_000_000.0_f64;
    let in_cost = (tokens_in as f64) * entry.input_per_million_usd / per_million;
    let out_cost = (tokens_out as f64) * entry.output_per_million_usd / per_million;
    in_cost + out_cost
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
schema_version: 0.1.0
last_updated: 2026-05-14
models:
  - provider: anthropic
    model: claude-opus-4-7
    input_per_million_usd: 15.00
    output_per_million_usd: 75.00
  - provider: openai
    model: gpt-5.4
    input_per_million_usd: 1.25
    output_per_million_usd: 10.00
  - provider: local
    model: "*"
    input_per_million_usd: 0.0
    output_per_million_usd: 0.0
"#;

    fn fixture() -> PricingTable {
        PricingTable::from_yaml(SAMPLE_YAML).expect("fixture YAML must parse")
    }

    #[test]
    fn exact_match_computes_cost() {
        let t = fixture();
        // 12480 in * 15/M + 3210 out * 75/M
        // = 0.18720 + 0.24075 = 0.42795
        let cost = t.lookup_cost("anthropic", "claude-opus-4-7", 12480, 3210).unwrap();
        let expected = 12480.0 * 15.0 / 1_000_000.0 + 3210.0 * 75.0 / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-9, "got {}, expected {}", cost, expected);
    }

    #[test]
    fn unknown_provider_model_returns_none() {
        let t = fixture();
        assert!(t.lookup_cost("xai", "grok-99", 1000, 500).is_none());
        assert!(t.lookup_cost("anthropic", "unknown-model", 1000, 500).is_none());
    }

    #[test]
    fn wildcard_matches_any_model_for_provider() {
        let t = fixture();
        // local/* wildcard matches any local model
        let cost = t.lookup_cost("local", "llama-3.3-70b", 5000, 2000).unwrap();
        assert_eq!(cost, 0.0, "local always-zero by wildcard");
        let cost = t.lookup_cost("local", "qwen2.5-coder-3b", 1000, 500).unwrap();
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn exact_match_takes_precedence_over_wildcard() {
        let yaml = r#"
models:
  - provider: openai
    model: gpt-5.4
    input_per_million_usd: 1.25
    output_per_million_usd: 10.00
  - provider: openai
    model: "*"
    input_per_million_usd: 99.99
    output_per_million_usd: 999.99
"#;
        let t = PricingTable::from_yaml(yaml).unwrap();
        // gpt-5.4 hits the exact entry, not the bogus wildcard
        let cost = t.lookup_cost("openai", "gpt-5.4", 1000, 1000).unwrap();
        let expected = 1000.0 * 1.25 / 1_000_000.0 + 1000.0 * 10.0 / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-9);
        // Different openai model hits the wildcard
        let cost = t.lookup_cost("openai", "gpt-99", 1000, 1000).unwrap();
        let expected = 1000.0 * 99.99 / 1_000_000.0 + 1000.0 * 999.99 / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn zero_tokens_returns_zero_cost() {
        let t = fixture();
        let cost = t.lookup_cost("anthropic", "claude-opus-4-7", 0, 0).unwrap();
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn large_token_counts_dont_overflow() {
        // Cumulative-over-a-year worst case: ~10M tokens. We allow much higher.
        let t = fixture();
        let cost = t
            .lookup_cost("anthropic", "claude-opus-4-7", 1_000_000_000, 1_000_000_000)
            .unwrap();
        // 1B input * 15/M + 1B output * 75/M = 15,000 + 75,000 = 90,000 USD
        assert!((cost - 90_000.0).abs() < 0.01, "got {}", cost);
    }

    #[test]
    fn empty_table_returns_none_for_all() {
        let t = PricingTable::empty();
        assert!(t.lookup_cost("anthropic", "claude-opus-4-7", 1000, 500).is_none());
        assert!(t.last_updated().is_none());
    }

    #[test]
    fn malformed_yaml_returns_parse_error() {
        let result = PricingTable::from_yaml("not yaml: [malformed");
        assert!(result.is_err());
    }

    #[test]
    fn last_updated_exposed_and_staleness_check() {
        let t = fixture();
        let updated = t.last_updated().unwrap();
        assert_eq!(updated, NaiveDate::from_ymd_opt(2026, 5, 14).unwrap());

        // Same-day: not stale
        assert!(!t.is_stale(NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(), 30));
        // 25 days later: still not stale (under 30-day threshold)
        assert!(!t.is_stale(NaiveDate::from_ymd_opt(2026, 6, 8).unwrap(), 30));
        // 60 days later: stale
        assert!(t.is_stale(NaiveDate::from_ymd_opt(2026, 7, 13).unwrap(), 30));
    }

    #[test]
    fn missing_last_updated_treated_as_stale() {
        let yaml = "models: []";
        let t = PricingTable::from_yaml(yaml).unwrap();
        assert!(t.last_updated().is_none());
        // Defensive: no date = stale by default
        assert!(t.is_stale(NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(), 30));
    }

    #[test]
    fn loads_from_real_pricing_yaml_at_repo_root() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("pricing.yaml");
        let t = PricingTable::load_from_path(&path)
            .expect("real pricing.yaml at repo root must parse");
        // Should have at least Anthropic + OpenAI entries (verifying the file is real-shape).
        let claude_cost = t.lookup_cost("anthropic", "claude-opus-4-7", 1000, 1000);
        assert!(claude_cost.is_some(), "real pricing.yaml must price claude-opus-4-7");
        let gpt_cost = t.lookup_cost("openai", "gpt-5.4", 1000, 1000);
        assert!(gpt_cost.is_some(), "real pricing.yaml must price gpt-5.4");
    }
}
