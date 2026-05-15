// examples/smoke_real_data.rs — A/Lane-A real-data smoke test.
//
// Run: `cd src-tauri && cargo run --example smoke_real_data [N]`
//      N = how many random non-subagent files to test (default: 25).
//
// Walks ~/.claude/projects/**/*.jsonl with the production walker config
// (skip subagents/, skip mtime < 1hr), then parses every candidate via the
// real ClaudeCodeAdapter. Aggregates per-result outcomes into a report.
//
// Purpose: confirm the new JSONL parse() body (Lane A) actually works
// against Clay's real data, not just synthetic fixtures. Catches schema
// drift, control-char patterns, edge cases the unit tests miss.

use std::collections::HashMap;
use std::path::PathBuf;

use lens_lib::adapters::claude_code::{Adapter, ClaudeCodeAdapter, ParseResult};
use lens_lib::ingestion::walker::{walk_candidates, WalkerConfig};
use lens_lib::pricing::PricingTable;
use lens_lib::project_resolver::ProjectResolver;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);

    let home = std::env::var("HOME").expect("HOME must be set");
    let root = PathBuf::from(format!("{}/.claude/projects", home));
    if !root.exists() {
        eprintln!("ERROR: {} does not exist", root.display());
        std::process::exit(1);
    }

    println!("=== Lens real-data smoke test ===");
    println!("Source: {}", root.display());
    println!();

    // Walk with production config.
    let walk_config = WalkerConfig::default();
    let walk_report = walk_candidates(&root, &walk_config);
    println!("Walker output:");
    println!("  candidates found:   {}", walk_report.candidates.len());
    println!("  skipped (active):   {}", walk_report.skipped_active);
    println!("  skipped (subagent): {}", walk_report.skipped_substring);
    println!();

    let candidates = walk_report.candidates;
    let sample: Vec<&PathBuf> = candidates.iter().take(n).collect();
    println!("Sampling {} of {} candidates for parse pass...", sample.len(), candidates.len());
    println!();

    let adapter = ClaudeCodeAdapter {
        project_resolver: ProjectResolver::empty(),
        pricing: PricingTable::empty(),
    };

    let mut ok_count = 0_usize;
    let mut recoverable_count = 0_usize;
    let mut fatal_count = 0_usize;
    let mut total_tokens_in = 0_u64;
    let mut total_tokens_out = 0_u64;
    let mut total_files_with_cwd = 0_usize;
    let mut total_files_with_model = 0_usize;
    let mut total_tool_invocations = 0_u64;
    let mut total_tool_errors = 0_u64;
    let mut status_counts: HashMap<String, u64> = HashMap::new();
    let mut distinct_projects: HashMap<String, u64> = HashMap::new();
    let mut fatal_reasons: HashMap<String, u64> = HashMap::new();
    let mut recoverable_warnings: HashMap<String, u64> = HashMap::new();

    for (i, path) in sample.iter().enumerate() {
        let basename = path.file_name().unwrap().to_string_lossy();
        let short_path = path
            .strip_prefix(&root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());

        let results = adapter.parse(path);
        let result = results.into_iter().next().expect("adapter always returns >=1 result");

        match result {
            ParseResult::Ok(event) => {
                ok_count += 1;
                let status_label = format!("{:?}", event.status);
                *status_counts.entry(status_label).or_insert(0) += 1;
                if let Some(p) = &event.project {
                    *distinct_projects.entry(p.clone()).or_insert(0) += 1;
                }
                if event.cwd.is_some() {
                    total_files_with_cwd += 1;
                }
                if event.model.is_some() {
                    total_files_with_model += 1;
                }
                total_tokens_in += event.tokens_in.unwrap_or(0);
                total_tokens_out += event.tokens_out.unwrap_or(0);
                if let Some(extra) = &event.extra {
                    if let Some(n) = extra.get("tool_count_total").and_then(|v| v.as_u64()) {
                        total_tool_invocations += n;
                    }
                    if let Some(n) = extra.get("tool_errors").and_then(|v| v.as_u64()) {
                        total_tool_errors += n;
                    }
                }
                println!(
                    "{:>3}. OK    [{:>9}]  status={:?}  model={:?}  toks={}+{}  proj={:?}  | {}",
                    i + 1,
                    truncate_for_print(&event.event_id, 9),
                    event.status,
                    event.model.as_deref().unwrap_or("-"),
                    event.tokens_in.unwrap_or(0),
                    event.tokens_out.unwrap_or(0),
                    event.project.as_deref().unwrap_or("-"),
                    truncate_for_print(&short_path, 60)
                );
            }
            ParseResult::Recoverable { event, warnings } => {
                recoverable_count += 1;
                let status_label = format!("{:?}", event.status);
                *status_counts.entry(status_label).or_insert(0) += 1;
                if let Some(p) = &event.project {
                    *distinct_projects.entry(p.clone()).or_insert(0) += 1;
                }
                if event.cwd.is_some() {
                    total_files_with_cwd += 1;
                }
                if event.model.is_some() {
                    total_files_with_model += 1;
                }
                total_tokens_in += event.tokens_in.unwrap_or(0);
                total_tokens_out += event.tokens_out.unwrap_or(0);
                for warning in &warnings {
                    *recoverable_warnings.entry(warning.clone()).or_insert(0) += 1;
                }
                println!(
                    "{:>3}. WARN  [{:>9}]  status={:?}  warnings={}  | {}",
                    i + 1,
                    truncate_for_print(&event.event_id, 9),
                    event.status,
                    warnings.len(),
                    truncate_for_print(&short_path, 60)
                );
            }
            ParseResult::Fatal { reason, .. } => {
                fatal_count += 1;
                *fatal_reasons.entry(reason.clone()).or_insert(0) += 1;
                println!(
                    "{:>3}. FATAL [{:>9}]                                              | {} ({})",
                    i + 1,
                    "-",
                    truncate_for_print(&short_path, 60),
                    truncate_for_print(&reason, 50)
                );
                let _ = basename;
            }
        }
    }

    let total = sample.len();
    println!();
    println!("=== Aggregate report ===");
    println!("  Files attempted:        {}", total);
    println!(
        "  Ok:                     {}  ({:.1}%)",
        ok_count,
        100.0 * ok_count as f64 / total.max(1) as f64
    );
    println!(
        "  Recoverable:            {}  ({:.1}%)",
        recoverable_count,
        100.0 * recoverable_count as f64 / total.max(1) as f64
    );
    println!(
        "  Fatal:                  {}  ({:.1}%)",
        fatal_count,
        100.0 * fatal_count as f64 / total.max(1) as f64
    );
    println!();
    let resolvable = ok_count + recoverable_count;
    println!(
        "  cwd populated:          {}/{}  ({:.1}%)",
        total_files_with_cwd,
        resolvable,
        100.0 * total_files_with_cwd as f64 / resolvable.max(1) as f64
    );
    println!(
        "  model populated:        {}/{}  ({:.1}%)",
        total_files_with_model,
        resolvable,
        100.0 * total_files_with_model as f64 / resolvable.max(1) as f64
    );
    println!("  Total tokens in:        {}", total_tokens_in);
    println!("  Total tokens out:       {}", total_tokens_out);
    println!("  Total tool invocations: {}", total_tool_invocations);
    println!("  Total tool errors:      {}", total_tool_errors);
    println!();
    println!("  Status distribution:");
    let mut status_sorted: Vec<_> = status_counts.iter().collect();
    status_sorted.sort_by_key(|(_, &v)| std::cmp::Reverse(v));
    for (status, count) in status_sorted {
        println!("    {:<10}: {}", status, count);
    }
    println!();
    println!("  Distinct projects ({} total):", distinct_projects.len());
    let mut proj_sorted: Vec<_> = distinct_projects.iter().collect();
    proj_sorted.sort_by_key(|(_, &v)| std::cmp::Reverse(v));
    for (proj, count) in proj_sorted.iter().take(10) {
        println!("    {:<40}: {}", truncate_for_print(proj, 40), count);
    }
    if !fatal_reasons.is_empty() {
        println!();
        println!("  Fatal reasons:");
        for (reason, count) in &fatal_reasons {
            println!("    {:<60}: {}", truncate_for_print(reason, 60), count);
        }
    }
    if !recoverable_warnings.is_empty() {
        println!();
        println!("  Recoverable warning patterns:");
        let mut w_sorted: Vec<_> = recoverable_warnings.iter().collect();
        w_sorted.sort_by_key(|(_, &v)| std::cmp::Reverse(v));
        for (warning, count) in w_sorted.iter().take(5) {
            println!("    {:<60}: {}", truncate_for_print(warning, 60), count);
        }
    }

    println!();
    println!("=== Verdict ===");
    if fatal_count == 0 && recoverable_count <= total / 2 {
        println!("PASS — parser handled real data cleanly (no fatals, low warning rate).");
    } else if fatal_count <= total / 10 {
        println!("MOSTLY PASS — minor fatals or many warnings; review patterns above.");
    } else {
        println!("FAIL — fatal rate >10%. Parser bug or schema drift; investigate before shipping.");
    }
}

fn truncate_for_print(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{}…", truncated)
    }
}
