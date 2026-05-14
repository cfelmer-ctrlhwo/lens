# agent-activity.v1 — Cross-Tool AI Activity Event Schema

> **Purpose:** Normalized event format for representing AI tool activity (sessions, scheduled runs, failures, parser errors) across vendors, so a single surface can aggregate runs from Claude Code, Codex, Grok, Perplexity, Paperclip, local models, and any other AI tool emitting structured logs.
> **Process:** Producers (tool adapters, future native emitters) emit JSON events conforming to this schema. Consumers (Lens, future dashboards) ingest events, normalize storage, and render unified views. Append-only. Version 0.1.0 is DRAFT — additive changes allowed; promotes to 1.0.0 (STABLE) once ≥2 independent adapters have operated against it for one continuous week without rewrite.
> **Connections:** Lens dashboard (consumer reference implementation) · Claude Code adapter (`~/.claude/projects/*/session-meta/*.json`) · Codex CLI adapter (`~/.codex/sessions/*.json`) · Future emitters from cooperating AI tools · Design doc at `~/.gstack/projects/ClaudeCode/cfelmer-unknown-design-20260514-134014.md`

Status: DRAFT
Schema version: 0.1.0
Last updated: 2026-05-14
Maintainer: cfelmer

---

## 1. Why this schema exists

Every AI tool emits activity in its own dialect: Claude Code writes JSON session-meta files, Codex writes JSON transcripts, Grok keeps SQLite, Paperclip emits filesystem markers, scheduled jobs leave plist + log trails. There is no shared format for "an AI run happened, here's what it did, what it cost, what it produced."

`agent-activity.v1` is that shared format. The goal is a stable, vendor-neutral event shape that:

1. Lens (and any future dashboard) can ingest from multiple sources without per-tool special-casing in the storage layer.
2. AI tools can natively emit (V1.1+) instead of being scraped, removing dependence on adapter code.
3. Power users can pipe through standard tooling (`jq`, SQLite, DuckDB, Polars) without learning a custom format.

The schema is intentionally minimal in V1. Five event types, ~25 fields, no streaming protocol, no cryptographic signing, no episode primitive. Those grow in later versions only if real adoption demands them.

## 2. Core concepts

| Concept | Definition |
|---|---|
| **Event** | Atomic record of one activity from one tool. The unit of this schema. |
| **Session** | Tool-internal grouping. One Claude Code conversation, one Codex run, one Grok thread. Producers emit one or more events per session. |
| **Episode** | Consumer-derived grouping across tools and projects. NOT a producer concept. Computed by consumers (e.g., Lens) via heuristics like shared cwd within a time window. |
| **Project** | User-facing grouping of related activity, typically resolved from cwd via a user-supplied mapping. Defaults to `"Uncategorized"` when unresolved. |
| **Tool** | The producer of the event. Lower-kebab-case identifier, stable across releases. |
| **Artifact** | File path mentioned in tool-use events during the activity (Edit/Write/Bash output). Filesystem watching is NOT part of V1. |
| **Adapter** | Code that converts a tool's native logs into agent-activity.v1 events. Lives in the consumer (e.g., Lens) in V1; may be natively emitted by cooperating tools in V1.1+. |

## 3. Versioning policy

- Schema follows semver.
- V1 schema is `0.x` — additive changes (new optional fields, new event_types, new enum values) allowed without bump. Breaking changes (renaming, removing, retyping required fields) require minor bump (`0.1` → `0.2`).
- V1.1 promotes to `1.0.0` (STABLE) once ≥2 independent adapters have operated against it for one continuous week without schema rewrite.
- Post-1.0, breaking changes require major bump (`1.0` → `2.0`) with migration guidance committed alongside the bump.
- **Consumers MUST ignore unknown fields.**
- **Consumers MUST NOT fail on additive changes.**
- Consumers SHOULD log-but-process events with `schema_version` higher than the consumer's supported range; fail only if a required field is missing.

"Internal-stable" (used in the design doc) = schema_version 0.x, in active use, additive changes allowed, no breaking changes without minor bump, not yet promoted to public 1.0.0 spec.

## 3.1 OpenTelemetry GenAI alignment

OpenTelemetry maintains [Semantic Conventions for Generative AI](https://opentelemetry.io/docs/specs/semconv/gen-ai/), currently in Development status, covering spans, events, and metrics for AI operations. `agent-activity.v1` is **deliberately compatible-by-aliasing** with this convention rather than renaming to match it:

- Lens field names are kept short for developer ergonomics (`tokens_in` beats `gen_ai.usage.input_tokens` in `jq` queries and SQL filters).
- OTel field names are preserved as aliases in `extra.gen_ai.*` so OTel-native tooling can consume Lens events without translation.
- Because OTel GenAI conventions are still in Development, this approach insulates Lens's stable schema from upstream field-name churn while preserving interop.

**Alignment table (Lens → OTel GenAI):**

| Lens field | OTel attribute | Alias path | Notes |
|---|---|---|---|
| `session_id` | `gen_ai.conversation.id` | `extra.gen_ai.conversation.id` | Same semantics: tool-internal session/thread identifier. |
| `provider` | `gen_ai.system` | `extra.gen_ai.system` | Values match: `anthropic`, `openai`, `xai`, `google`, `local`. |
| `model` | `gen_ai.request.model` | `extra.gen_ai.request.model` | Direct mapping. |
| `tokens_in` | `gen_ai.usage.input_tokens` | `extra.gen_ai.usage.input_tokens` | Direct mapping. |
| `tokens_out` | `gen_ai.usage.output_tokens` | `extra.gen_ai.usage.output_tokens` | Direct mapping. |
| `event_type` | `gen_ai.operation.name` | — | Structural divergence: Lens uses session-level event types (session_started/completed/failed), OTel uses operation-level (chat/text_completion/embeddings). Not a 1:1 alias. |
| `status` | (no direct OTel attr) | — | Lens-specific. |
| `cost_usd_estimated` | (not in OTel spec) | — | Lens-specific; OTel does not standardize cost. |
| `cwd` | (not in OTel spec) | — | Lens-specific; OTel does not capture working directory. |
| `project` | (not in OTel spec) | — | Lens-specific; consumer-derived from cwd. |

**Structural divergence (intentional):** OTel models AI activity as a span tree per request (one span per LLM call, nested under operation spans). Lens models it as one event per session (whole conversations rolled up). For Lens's wedge ("show me my daily AI activity across tools"), event-per-session is operationally right; OTel's span tree is the right model for production tracing of a single AI service. The two formats are complementary, not redundant.

**Producer guidance:** adapters SHOULD populate `extra.gen_ai.*` aliases for any Lens field with a direct OTel mapping (rows 1-5 above). This is the minimum-effort interop guarantee. Adapters MAY omit aliases for fields with no OTel equivalent (rows 6-10).

**Consumer guidance:** Lens-native consumers query Lens field names directly. OTel-native tooling (Langtrace, Logfire, others) can consume `extra.gen_ai.*` aliases without translation.

## 4. Event types (V1)

| event_type | When emitted |
|---|---|
| `session_started` | A tool session began. `ended_at` MUST be omitted. |
| `session_completed` | A tool session finished. `status` is typically `success` or `partial`. `ended_at` REQUIRED. |
| `session_failed` | A tool session ended with an error. `status: failure`. `error_message` SHOULD be set. `ended_at` REQUIRED. |
| `scheduled_run_completed` | A cron / launchd / orchestrator-triggered run finished. `ended_at` REQUIRED. |
| `parser_error` | An adapter could not parse a source record. Emitted by Lens itself (tool: `lens-adapter`), not by the source tool. `status: failure`. Captures source path and reason in `extra`. |

V1.1+ may add: `tool_use` (per-call granularity), `artifact_created` (filesystem-watched), `cost_alert`, `goal_completed` (`/goal` integration).

## 5. Event payload

A single event is a JSON object. Encoding: UTF-8. Newline-delimited JSON (NDJSON) is the recommended on-disk format.

### 5.1 Required fields

| Field | Type | Notes |
|---|---|---|
| `schema_version` | string (semver) | Version this event conforms to. V1 producers emit `"0.1.0"`. |
| `event_id` | string (ULID) | Globally unique, sortable, time-prefixed. ULID strongly preferred over UUIDv4 for natural ordering. |
| `tool` | string | Tool identifier. Lower-kebab-case. Stable across releases. See §6 for the V1 reserved list. |
| `event_type` | string (enum) | One of §4. |
| `started_at` | string (ISO 8601, UTC, `Z` suffix) | When the activity began. |
| `status` | string (enum) | One of: `success`, `failure`, `partial`, `unknown`. |

### 5.2 Conditionally required

| Field | Type | When required |
|---|---|---|
| `ended_at` | string (ISO 8601, UTC) | REQUIRED for terminal events (`session_completed`, `session_failed`, `scheduled_run_completed`). MUST NOT be set for `session_started`. |
| `error_message` | string | SHOULD be set for `status: failure`. Short, human-readable. |

### 5.3 Recommended fields

| Field | Type | Notes |
|---|---|---|
| `tool_version` | string | Helps debug parser drift across tool releases. |
| `session_id` | string | Tool-internal session/conversation identifier. Correlates multiple events from the same run. |
| `project` | string | Project name. Resolved by adapter via cwd→project mapping. Defaults to `"Uncategorized"`. |
| `cwd` | string | Absolute path where the activity ran (tilde-expanded, symlinks resolved). Drives project attribution. |
| `model` | string | Model identifier. E.g., `claude-opus-4-7`, `gpt-5.4`, `grok-3.5`, `llama-3.3-70b`. |
| `provider` | string | One of: `anthropic`, `openai`, `xai`, `google`, `local`, `other`. |
| `tokens_in` | integer | Input/prompt token count. |
| `tokens_out` | integer | Output/completion token count. |
| `cost_usd_estimated` | number | Estimated cost in USD. |
| `cost_source` | string (enum) | How cost was computed. One of: `log_parse`, `api_billing`, `tool_reported`, `none`. |
| `summary` | string | One-line description of what the activity did. Tool-emitted summaries preferred over consumer-derived. |
| `raw_ref` | string | Path to the raw source data the adapter parsed. Useful for "click to see original log." Tilde paths allowed. |

### 5.4 Optional fields

| Field | Type | Notes |
|---|---|---|
| `tokens_total` | integer | Sum of in + out. May be set when only one of in/out is known. |
| `artifacts` | array of strings | File paths mentioned in tool-use events. V1: just paths, no metadata. |
| `tags` | array of strings | Free-form user-defined tags. |
| `parent_event_id` | string (ULID) | For nested or causally-linked events. Leave empty in V1. V1.1+ may use heavily. |
| `extra` | object | Tool-specific extension namespace. Consumers SHOULD ignore unless they understand the tool dialect. See §5.5. |

### 5.5 Producer extension namespace (`extra`)

The `extra` field is an opaque object for tool-specific data that doesn't fit the core schema. Examples:
- A Claude Code adapter might emit `extra.session_subtype: "skill"` for skill-invocation sessions.
- A Codex adapter might emit `extra.reasoning_effort: "high"`.
- A parser_error event uses `extra.adapter`, `extra.source_path`, `extra.parser_version`, `extra.reason`.

Forward-compat rule: producers MAY add new `extra.*` fields without a schema version bump. Consumers MUST ignore unknown `extra` contents unless they understand the specific tool's dialect.

## 6. Reserved tool identifiers (V1)

Stable identifiers for known producers. Adapters and native emitters SHOULD use these exact strings.

| tool | Source |
|---|---|
| `claude-code` | Anthropic Claude Code CLI |
| `codex-cli` | OpenAI Codex CLI |
| `grok-desktop` | xAI Grok Desktop |
| `perplexity-pc` | Perplexity Personal Computer |
| `paperclip` | Paperclip.ing native emitter |
| `ollama` | Local Ollama runtime |
| `orchestrator` | User's own scheduled-task system (cron, launchd, custom) |
| `lens-adapter` | Lens itself (used for `parser_error` events) |

Tools not on this list MAY use any other lower-kebab-case identifier. Reserved identifiers will be added in a registry doc post-1.0.

## 7. Examples

### 7.1 session_completed — Claude Code, success

```json
{
  "schema_version": "0.1.0",
  "event_id": "01HXNKQ3M7K9V8T2P4R5Z6Y7W8",
  "tool": "claude-code",
  "tool_version": "2.1.139",
  "event_type": "session_completed",
  "started_at": "2026-05-14T17:14:00Z",
  "ended_at": "2026-05-14T17:42:18Z",
  "status": "success",
  "session_id": "sess-abc123",
  "project": "Paperclip-Workflow-Beta",
  "cwd": "/Users/cfelmer/Desktop/Projects/Paperclip-Workflow-Beta",
  "model": "claude-opus-4-7",
  "provider": "anthropic",
  "tokens_in": 12480,
  "tokens_out": 3210,
  "tokens_total": 15690,
  "cost_usd_estimated": 0.234,
  "cost_source": "log_parse",
  "artifacts": [
    "src/agents/engineer.ts",
    "src/agents/designer.ts"
  ],
  "summary": "Refactored engineer/designer agent prompts for stable v3 contract",
  "raw_ref": "~/.claude/projects/Paperclip-Workflow-Beta/session-meta/sess-abc123.json"
}
```

### 7.2 session_failed — Codex, error

```json
{
  "schema_version": "0.1.0",
  "event_id": "01HXNM9F4P7R2K8V5N3J6T1Y9X",
  "tool": "codex-cli",
  "tool_version": "0.125.0",
  "event_type": "session_failed",
  "started_at": "2026-05-14T18:01:12Z",
  "ended_at": "2026-05-14T18:02:45Z",
  "status": "failure",
  "session_id": "019e279c-a51e-7ab2-b7cf-c69207c6a268",
  "project": "Lens",
  "cwd": "/Users/cfelmer/Projects/lens",
  "model": "gpt-5.4",
  "provider": "openai",
  "tokens_in": 850,
  "tokens_out": 0,
  "tokens_total": 850,
  "cost_usd_estimated": 0.012,
  "cost_source": "log_parse",
  "error_message": "Not inside a trusted directory; pass --skip-git-repo-check",
  "raw_ref": "~/.codex/sessions/019e279c-a51e-7ab2-b7cf-c69207c6a268.json",
  "extra": {
    "reasoning_effort": "high",
    "sandbox": "read-only"
  }
}
```

### 7.3 scheduled_run_completed — orchestrator cron job

```json
{
  "schema_version": "0.1.0",
  "event_id": "01HXNQ8C3F2J5K7V9T1R4N6Y8B",
  "tool": "orchestrator",
  "tool_version": "1.0.0",
  "event_type": "scheduled_run_completed",
  "started_at": "2026-05-14T07:00:00Z",
  "ended_at": "2026-05-14T07:00:34Z",
  "status": "success",
  "project": "orchestrator",
  "cwd": "/Users/cfelmer/.claude/orchestrator",
  "summary": "Morning briefing generated; 3 newsletter items flagged for review",
  "tags": ["morning", "daily"],
  "raw_ref": "~/.claude/orchestrator/logs/2026-05-14-morning.log"
}
```

### 7.4 parser_error — adapter resilience

```json
{
  "schema_version": "0.1.0",
  "event_id": "01HXNRD2K8V5T7N3J6F1Y9P4W2",
  "tool": "lens-adapter",
  "event_type": "parser_error",
  "started_at": "2026-05-14T18:30:00Z",
  "status": "failure",
  "error_message": "Unparseable JSON in source record",
  "extra": {
    "adapter": "claude-code",
    "source_path": "~/.claude/projects/Foo/session-meta/sess-xyz.json",
    "parser_version": "0.1.0",
    "reason": "Unexpected token '}' at line 42, column 17"
  }
}
```

## 8. Adapter contract

An adapter for tool X converts X's native logs/transcripts into a sequence of `agent-activity.v1` events.

### 8.1 Adapter requirements

- **MUST** emit valid events per this schema (all required fields present, types correct).
- **MUST** handle parser errors gracefully per §8.2 (never crash the consumer).
- **MUST** emit one event per terminal session/run; the `event_id` MUST be deterministically derivable from `(tool, session_id, started_at)` so re-parsing the same source produces the same `event_id` (idempotency).
- **SHOULD** set `tool_version` when discoverable from the source.
- **SHOULD** prefer `"Uncategorized"` project assignment over dropping an event when cwd→project mapping fails.
- **SHOULD** emit `started_at` and `ended_at` in UTC even if the source records local time.

### 8.2 Parser failure taxonomy

When an adapter encounters a record it cannot parse:

**Fatal — skip the record, emit a `parser_error` event:**
- Unparseable JSON or malformed input
- Missing `event_id` source (cannot derive a unique identifier)
- Missing `started_at` source (cannot place on timeline)
- Missing `tool` indicator (cannot route)
- Missing `event_type` indicator

**Recoverable — emit a best-effort event with the missing fields omitted or defaulted:**
- Unknown `event_type` value in source (emit with `status: "unknown"`, log in `extra.original_event_type`)
- Unknown source fields (drop them or capture in `extra`)
- Missing optional fields (emit without them)
- Source schema version drift (record source schema in `extra.source_schema_version`, attempt best-effort parse)

The principle: false-negative events are worse than degraded-quality events. Skip only when the event cannot be placed on a timeline at all.

## 9. Consumer contract

- **MUST** ignore unknown fields.
- **MUST NOT** fail on additive schema changes (new event_types, new optional fields, new enum values).
- **MUST** validate that required fields are present; reject events missing required fields and log the rejection.
- **SHOULD** warn (not fail) when an event's `schema_version` is higher than the consumer's supported range. Process best-effort.
- **MAY** compute derived data (episodes, project rollups, cost summaries) but **MUST NOT** mutate received events. Derived data is consumer-local state.
- **MAY** discard events older than a configurable retention window. Retention is consumer policy, not schema policy.

## 10. Non-goals (V1)

The following are explicitly out of scope. Some may land in V1.1 or V2; none in V1.

- **Episode_id as a first-class producer-emitted field.** Episodes are consumer-derived in V1.
- **Cryptographic signing or tamper-evidence.** No event integrity guarantees in V1.
- **Multi-machine event correlation.** A single Lens install sees only one machine's events. Cross-machine sync is a V2 paid feature.
- **Streaming event protocol.** V1 is batch/append-only file format. WebSocket / SSE streaming is V1.1+.
- **Cost reconciliation logic.** When producer-reported and log-derived costs disagree, V1 consumers trust producer if both are present. Reconciliation rules are V1.1+.
- **Tool-call granularity.** V1 events represent whole sessions; per-call (Edit, Write, Bash) granularity is V1.1+.
- **JSON Schema (.schema.json) formal validator artifact.** V1 ships this Markdown spec only. Machine-readable JSON Schema is V1.1.

## 11. Open questions

These are decisions deferred to first contact with reality (i.e., parsing Clay's actual logs in Week 1) rather than resolved up front.

1. **ULID vs UUIDv7.** Both are sortable + time-prefixed. ULID is more widely supported in Rust today, but UUIDv7 is the emerging standard. Pick one in Week 1 and commit.
2. **Idempotent event_id derivation.** Should the deterministic recipe be `hash(tool || session_id || started_at)`, or something including `raw_ref`? Decide once we see how Claude Code and Codex session identifiers actually behave under re-parsing.
3. **How to represent ongoing sessions.** A Claude Code session that's still active when Lens parses the log — emit `session_started` only, no terminal event? Emit a synthetic `session_partial` with `status: "partial"` and current state? V1 leans toward the former; revisit if it causes UI confusion.
4. **Project mapping format.** YAML mapping of `cwd_prefix → project_name`. Should it support glob patterns? Regex? Plain prefix-match is simpler; decide based on how messy Clay's actual cwd's look.
5. **Cost when the model is unknown.** Some session logs lack model identification (e.g., older Claude Code versions). Emit with `cost_source: "none"` and `cost_usd_estimated` omitted, or estimate against a default model? V1: omit.

## 12. Changelog

| Version | Date | Change |
|---|---|---|
| 0.1.0 | 2026-05-14 | Initial DRAFT. Five event types (`session_started`, `session_completed`, `session_failed`, `scheduled_run_completed`, `parser_error`). Producer + consumer contracts. Parser failure taxonomy. Reserved tool identifiers. Examples for four event types. |
| 0.1.1 | 2026-05-14 | Added §3.1 OpenTelemetry GenAI alignment. Producer guidance to populate `extra.gen_ai.*` aliases for fields with direct OTel mappings (session_id, provider, model, tokens_in, tokens_out). Justified structural divergence (Lens event-per-session vs OTel span-tree-per-request) and documented Lens-specific fields with no OTel equivalent (status, cost_usd_estimated, cwd, project). No breaking changes to V1 producers or consumers. |
