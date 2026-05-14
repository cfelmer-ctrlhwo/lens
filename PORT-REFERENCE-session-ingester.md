# Port Reference — session_ingester.py → claude_code.rs

> **Purpose:** Direct port reference from Clay's working Python session-meta parser to the Rust Claude Code adapter for Lens. Source: `~/.claude/orchestrator/session_ingester.py` (~750 lines). Target: `lens-starter/src/adapters/claude_code.rs`. This doc captures the actual shape of Claude Code's JSONL transcripts and the parsing decisions that took the orchestrator months to get right.
> **Process:** Read before Saturday's Day 1 spike. Port the deterministic parts (extract_session_meta + helpers) literally. Skip the Ollama-driven facets generation — that's V2+ territory. Update the skeleton's working hypothesis to match the real source shape.
> **Connections:** Source: `~/.claude/orchestrator/session_ingester.py`. Target: `lens-starter/src/adapters/claude_code.rs`. Consumed by: Lens ingestion pipeline. Informed by: design doc §Causal Stitching + §Constraints + agent-activity-v1.md §5.

Status: REFERENCE
Last updated: 2026-05-14
Original Python: 3154 lines (validate.py = shallow mtime aggregator, NOT the parser. session_ingester.py = real parser)

---

## 1. The biggest correction: source is JSONL, not JSON

The adapter skeleton currently does `serde_json::from_str(&raw)` on a single JSON file. **This is wrong for Claude Code.** Real Claude Code transcripts are JSONL (newline-delimited JSON) at:

```
~/.claude/projects/<url-encoded-project-path>/<session-id>.jsonl
```

Example real path: `~/.claude/projects/-Users-cfelmer-Desktop-Projects-Paperclip-Workflow-Beta/sess-abc123.jsonl`

The `~/.claude/usage-data/session-meta/*.json` path referenced in CLAUDE.md is the orchestrator's OUTPUT after parsing, not the source. Lens parses raw JSONL from `~/.claude/projects/`, same as the orchestrator does.

**Fix the adapter:** read the file line by line, parse each line as a JSON entry, accumulate entries, then derive a session-meta dict by aggregating across entries. NOT a 1:1 JSON→struct mapping.

## 2. Source JSONL entry shape (per line)

Each line in the JSONL is one entry. Entries have a `type` discriminator:

| type | What it represents | Key fields |
|---|---|---|
| `user` | A human prompt or a tool_result echo back | `timestamp`, `message.content`, `cwd` (only on first), `isSidechain`, `isMeta`, `isCompactSummary` |
| `assistant` | A Claude turn | `timestamp`, `message.id`, `message.content` (array of blocks), `message.usage.{input_tokens,output_tokens}` |

`message.content` for assistant is an array of typed blocks: `text`, `tool_use`, `thinking`. `message.content` for user can be a plain string OR an array containing `tool_result` blocks (from prior tool_use), `text` blocks (when the human typed something), etc.

**The session_id is the filename stem, not a field in the JSONL.** Same for project_path — derived from the parent directory's URL-encoded name (or from `cwd` on the first user message, preferred).

## 3. URL-encoded project paths

Claude Code encodes `/Users/cfelmer/Desktop/Projects/Foo` as directory name `-Users-cfelmer-Desktop-Projects-Foo`. The orchestrator's fallback when `cwd` isn't readable:

```python
project_path = "/" + project_dir.replace("-", "/", 1).lstrip("/")
```

**Wrong-ish:** this single-replace approach mangles paths with legitimate hyphens (e.g., `Paperclip-Workflow-Beta` becomes `Paperclip/Workflow-Beta` after one replace then unaffected). Lens should prefer `cwd` from the first user message ALWAYS, and treat the URL-encoded directory name only as a fingerprint, not a reliable path.

## 4. Filtering rules (port these exactly)

Before parsing, exclude:
- Files inside `<session-id>/subagents/` subdirectories. Subagent transcripts are separate sessions, not noise to merge in.
- Files modified within the last 3600s (`ACTIVE_SESSION_THRESHOLD_SECS`). These are still being written; parsing them risks UPSERT thrash. Wait until they go quiet.

Within a file, when counting human user messages, exclude entries where:
- `isSidechain == true` (parallel subagent invocations)
- `isMeta == true` (system housekeeping)
- `isCompactSummary == true` (auto-compaction markers)
- `message.content` is a list containing any `tool_result` block (those are tool-output echoes, not human prompts)

## 5. Resilience patterns (port all five)

```python
# Pattern 1: errors="replace" on file open — survives bad encoding
with open(jsonl_path, "r", errors="replace") as f:

# Pattern 2: per-line try/except — one bad line never aborts the file
for line in f:
    try:
        entries.append(json.loads(line))
    except json.JSONDecodeError:
        # Pattern 3: retry after stripping control chars
        clean = re.sub(r'[\x00-\x08\x0b\x0c\x0e-\x1f]', '', line)
        try:
            entries.append(json.loads(clean))
        except (json.JSONDecodeError, Exception):
            continue  # Pattern 4: skip and keep going

# Pattern 5: graceful None return — caller emits parser_error event
if not entries or not all_timestamps:
    return None
```

Rust port (sketch):

```rust
let file = match std::fs::File::open(source_path) {
    Ok(f) => f,
    Err(e) => return vec![ParseResult::Fatal { ... }],
};
let reader = std::io::BufReader::new(file);
let mut entries: Vec<serde_json::Value> = Vec::new();
for line_result in reader.lines() {
    let Ok(line) = line_result else { continue };
    let line = line.trim();
    if line.is_empty() { continue; }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(entry) => entries.push(entry),
        Err(_) => {
            // Retry after stripping control chars
            let clean: String = line.chars()
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
                .collect();
            if let Ok(entry) = serde_json::from_str(&clean) {
                entries.push(entry);
            }
            // else: skip this line, keep going
        }
    }
}
if entries.is_empty() {
    return vec![ParseResult::Fatal {
        source_path: source_path.to_path_buf(),
        reason: "No parseable entries".into(),
    }];
}
```

## 6. Deduplication (mandatory — Claude Code emits duplicates)

Claude Code can write the same `tool_use` block or assistant message twice across the JSONL (network retries, partial flushes). Without dedupe, token counts and tool counts inflate. Two dedupe sets:

```rust
let mut seen_tool_block_ids: HashSet<String> = HashSet::new();
let mut seen_msg_ids: HashSet<String> = HashSet::new();
```

Dedupe tool_use blocks by `block.id`. Dedupe token counts by `message.id`. If id is empty, count it (no way to dedupe).

## 7. The fields the orchestrator derives (and where they map to agent-activity.v1)

Output dict from `extract_session_meta` (Python) → mapping to `AgentActivityEvent` (Rust):

| Orchestrator field | Source | Lens field | Note |
|---|---|---|---|
| `session_id` | filename stem | `session_id` | Direct. |
| `project_path` | `cwd` from first user msg, fallback to URL-decoded dir | `cwd` | Project name is then resolved via projects.yaml. |
| `start_time` | min(timestamp across all entries) | `started_at` | Already ISO 8601 with Z. |
| (no equivalent) | max(timestamp across all entries) | `ended_at` | Add as derived. |
| `duration_minutes` | (max - min) timestamps | (derive in UI) | Don't store as separate field. |
| `input_tokens` | sum of `message.usage.input_tokens` (deduped by msg id) | `tokens_in` | Direct. |
| `output_tokens` | sum of `message.usage.output_tokens` (deduped by msg id) | `tokens_out` | Direct. |
| `tool_counts` | counter over deduped tool_use blocks | `extra.tool_counts` | Lens-specific; not in agent-activity.v1 core. Goes in extra. |
| `tool_errors` + `tool_error_categories` | count tool_result blocks where `is_error: true` | derive `status` from this | If tool_errors > 0 AND no successful recovery, status=failure. Else success. **THIS IS A V1 DESIGN QUESTION — discuss Saturday.** |
| `files_modified` | distinct file_path from Edit/Write/NotebookEdit tool_use blocks | `artifacts` | Direct, but spec says artifacts is a list, not count. |
| `lines_added` / `lines_removed` | count `\n` in old_string/new_string/content | `extra.lines_added`, `extra.lines_removed` | Lens-specific; goes in extra. |
| `git_commits` / `git_pushes` | scan Bash commands containing "git commit" / "git push" | `extra.git_commits`, `extra.git_pushes` | Lens-specific. |
| `first_prompt` | content of first non-sidechain non-meta user message (300 chars truncated) | `summary` | Direct, but consider shorter truncation. |
| `uses_task_agent`, `uses_mcp`, `uses_web_search`, `uses_web_fetch` | boolean from tool_counts keys | `extra.uses_*` | Lens-specific. |
| `user_response_times` | gaps between consecutive user message timestamps (1s-86400s range) | `extra.user_response_times` | Lens-specific. |
| `message_hours` | hour-of-day for each user message | `extra.message_hours` | Lens-specific; useful for "when does Clay work" analytics. |

**No `model` field is extracted by the orchestrator.** The model identifier doesn't seem to be in Claude Code's JSONL transcripts directly. **OPEN QUESTION FOR SATURDAY:** verify whether model is in the JSONL (perhaps in `message.model` or `metadata.model`) and update the parser accordingly. If model is absent, cost cannot be computed reliably — `cost_source: "none"` for all Claude Code events.

**No `outcome` / `error` / `status` top-level fields.** Status must be derived from `tool_errors` count and possibly the final assistant message content. **OPEN QUESTION FOR SATURDAY:** define the status-inference rule. Suggested rule: `tool_errors == 0` → success; `tool_errors > 0` AND `tool_errors / tool_count > 0.3` → failure; else → partial.

## 8. The status-derivation problem deserves its own paragraph

The skeleton currently assumes `raw.outcome` is a top-level field that Claude Code emits. **It isn't.** The orchestrator infers status from `tool_errors` count. Lens should do the same, BUT with R1A's UPSERT semantics, the status can change as the file is rewritten (e.g., a session that looks successful after entry 50 may have its final assistant message at entry 100 saying "I couldn't complete this"). UPSERT handles this correctly: re-parse, recompute status, overwrite event.

**Status inference recipe (port + improve):**

```rust
let status = if tool_errors == 0 {
    EventStatus::Success
} else if tool_errors as f64 / tool_count.max(1) as f64 > 0.3 {
    EventStatus::Failure
} else {
    EventStatus::Partial
};
```

This is heuristic. Saturday's spike should verify against 5-10 real sessions Clay knows the outcome of.

## 9. What to skip (V2+ territory)

- `generate_facets()` and everything Ollama-driven. V1 is deterministic only.
- The `goal_category` and `outcome` classifiers (LLM-based). V1.x.
- `_generate_agent_facets()`, `_minimal_facets()`, `_sanitize_facets()`. V1.x.

## 10. Functions to port literally (named, with Python line numbers)

| Python function | Line | Rust target |
|---|---|---|
| `extract_session_meta(jsonl_path)` | 82-368 | `ClaudeCodeAdapter::parse()` (rewrite from current `serde_json::from_str` to JSONL streaming + aggregation) |
| `_parse_ts(ts)` | 669-670 | `parse_iso_timestamp(s: &str)` helper |
| `_categorize_error(err_text)` | 673-681 | `categorize_tool_error(err: &str)` helper |
| `find_candidate_jsonl_files(days_back)` | 684-716 | `ClaudeCodeAdapter::list_source_files()` — walks `~/.claude/projects/**/*.jsonl`, filters subagents and active sessions |

Skip: `ingest_session()`, `_session_meta_path()`, `_facets_path()`, all Ollama code, all atomic-write code (Lens uses SQLite UPSERT instead).

## 11. Test fixtures Saturday

Once the parser works, validate it against 5-10 real sessions Clay knows the outcome of. Pick sessions from `~/.claude/projects/-Users-cfelmer-*` directories. Cross-reference with what Clay remembers about that session (success / failure / partial). Adjust the status-inference rule based on what matches reality.

Cross-validate against the orchestrator's existing parsed output at `~/.claude/usage-data/session-meta/*.json` (850 files exist). Lens's Rust output should match the orchestrator's Python output on `input_tokens`, `output_tokens`, `tool_counts`, `git_commits`, `files_modified`, `start_time`. Per the Reviewer Concern from the design doc: **"Rust output must match Python on a known fixture set before Week 1 closes."**

## 12. Updates needed to the existing skeleton

Once the spike confirms the JSONL shape, update `lens-starter/src/adapters/claude_code.rs`:

1. Change file format from JSON to JSONL (BufReader::lines + per-line parse).
2. Replace `ClaudeCodeRawSession` struct entirely. Real raw shape is a `Vec<serde_json::Value>` (one entry per JSONL line). The "session meta" is what you DERIVE from that vec, not what you DESERIALIZE.
3. Drop the `outcome` field assumption from `build_terminal_event`. Replace with status-inference from `tool_errors` count.
4. Add the dedupe sets (`seen_tool_block_ids`, `seen_msg_ids`) before iterating.
5. Add the subagents / active-session filters to the source-discovery step.
6. Populate `extra.tool_counts`, `extra.lines_added`, `extra.lines_removed`, `extra.git_commits`, `extra.git_pushes`, `extra.uses_*` for the Lens-specific derived fields the orchestrator already computes — they're useful even if not in agent-activity.v1 core.

Estimated effort for the rewrite: 4-6 hours of Rust work with `cc+gstack`. Saves 2-3 days of "discover the JSONL shape by trial and error" that Saturday would otherwise hit.

## 13. One more thing — the orchestrator already has dedupe logic for the wider Claude Code ecosystem

Read `session_ingester.py` lines 186-241 closely. The dedupe logic for tool_use blocks AND message tokens IS the reason Clay's existing aggregates are correct. Without it, Lens will overcount by 20-40% on long sessions. This isn't a nice-to-have; it's the difference between "the dashboard shows real numbers" and "the dashboard shows inflated numbers that don't match the billing page."

**This single port — extract_session_meta + dedupe + status inference — is worth 1-2 weeks of Lens build time saved.** The orchestrator already crashed against this surface area and won. Don't redo the discovery.
