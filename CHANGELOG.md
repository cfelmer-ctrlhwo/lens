# Changelog

All notable changes to Lens. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Under v1.0.0, 0.x.y versions may include breaking changes between minor bumps.

## [Unreleased]

### Added
- **Codex CLI adapter** (when Lane A lands) — second tool support beyond Claude Code.
- **GitHub Actions CI** (when Lane B lands) — macOS build, cargo test, frontend type-check on push + PR.
- **Homebrew tap formula** (when Lane B lands) — placeholder Ruby formula at `Formula/lens.rb`; activates on first GitHub Release.
- **Auto-refresh on backfill complete** — Tauri backend emits `lens:backfill-complete` event when ingestion finishes; React frontend listens and refetches timeline + counters immediately, no 2s status-poll delay.
- **User-editable config from app data dir** — `projects.yaml` and `pricing.yaml` are bootstrapped into `~/Library/Application Support/com.cfelmer.lens/` on first launch. Edit them there for project-mapping and per-model cost overrides. Restart to pick up changes (V1.x will add hot-reload).
- **Strict Tauri CSP** — `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: asset: https://asset.localhost; connect-src ipc: http://ipc.localhost 'self'; font-src 'self' data:`. Closes the /cso audit's only hardening recommendation.

### Changed
- Repository moved to `github.com/cfelmer-ctrlhwo/lens` (was tentatively `cfelmer/lens` in scaffolding). Cargo.toml and README updated. Homebrew install command is `brew install cfelmer-ctrlhwo/lens/lens` once the tap is published.

## [0.1.0] — 2026-05-14 (initial alpha)

First runnable release of Lens. Not yet shipped to users; this is the internal "V1 build complete" milestone. The app launches, ingests real Claude Code session logs from `~/.claude/projects/**/*.jsonl`, and renders them in a virtualized timeline. Smoke-tested against 97 of Clay's real session files.

### Added

#### Architecture
- **`agent-activity.v1` schema** (DRAFT 0.1.1, OpenTelemetry GenAI-aligned) — published as `docs/agent-activity-v1.md`. 5 event types, ~25 fields, full producer + consumer contracts, parser failure taxonomy. The wire-format moat per the project thesis.
- **Tauri 2 + Rust + React + TypeScript** desktop app. macOS-native, ~10 MB binary, no cloud account required.
- **SQLite + FTS5** local store at `~/Library/Application Support/com.cfelmer.lens/lens.db`. WAL mode, single-writer task pattern, hybrid storage (hot columns + raw_event JSON blob).

#### Adapters
- **Claude Code adapter** — parses `~/.claude/projects/**/*.jsonl`. Handles 5 entry types (user, assistant, attachment, queue-operation, last-prompt). Aggregates tokens, tool counts, files modified, git ops. Tolerates control-char contamination via strip-and-retry. Dedupe by `message.id` and `tool_use.id` (prevents 20-40% overcounting).
- **ProjectResolver** — `projects.yaml`-driven cwd → project name mapping. First-prefix-match wins. Path-boundary-safe (no `/foo` matching `/foobar`).
- **PricingTable** — `pricing.yaml`-driven token cost lookup. Exact match takes precedence over wildcard. Last-updated staleness warning.

#### Storage
- **R1A UPSERT-on-content-change** — re-parsing the same source file produces the same event_id (deterministic ULID); content_hash dedup avoids no-op writes. `ingest_seq` autoincrement is preserved across UPSERTs for stable cursor pagination.
- **Cursor-paginated timeline reads** — page size 200, max 500. Filters by project / tool / status. AND-combined.
- **Separate `ingestion_issues` table** per R2B — parser errors live in their own table so timeline queries stay clean and FTS5 indexes stay unpolluted.
- **Schema migrations** — `_meta` table tracks schema version; init refuses to open if schema is too new (downgrade protection).

#### Ingestion
- **Backfill mode** — walks `~/.claude/projects/**/*.jsonl`, skips files in `subagents/` and modified within last hour, parses via adapter, UPSERTs into storage. Runs on app launch in background thread; emits `lens:backfill-complete` event when done.
- **`notify`-based watch mode** — declared in API but returns `NotImplemented` in V1; lands in V1.x.

#### UI
- **Virtualized timeline** via `@tanstack/react-virtual` — handles 100k events smoothly at 60fps.
- **Filter chips** — project, tool, status (single-select; multi-select needs backend widening planned for V1.x).
- **Event detail panel** — slide-in from right, full pretty-printed JSON, 160ms CSS animation.
- **Empty state**, **error banner** (dismissible, IPC-aware), **dev panel** (Insert demo event button gated by `import.meta.env.DEV`).
- **Monospace builder aesthetic** with token-driven dark mode (`prefers-color-scheme: dark` flips CSS variables).

#### Distribution
- **MIT license**, LICENSE file at repo root.
- **README** with V1 scope, V1.1 deferral note (causal stitching), related-projects positioning, schema spec link.

### Build context

- 16 commits, ~4,400 lines of Rust, ~1,800 lines of React/TS.
- 111 unit + integration tests passing.
- 0 compiler warnings.
- /cso security audit (daily mode, 8/10 confidence gate): **zero findings**. Four informational items below the bar (CSP=null since fixed, hardcoded test-fixture path, no CI yet, no cargo-audit installed).
- Real-data smoke test (`cargo run --example smoke_real_data 25`): **PASS** — 24/25 OK, 1/25 Recoverable (cost warning), 0/25 Fatal across 25 real Claude Code session files.

### Not in V1 (deferred to V1.1+)

- **Causal stitching** — the morning-replay killer feature. Day 1 spike found Codex per-session usable cwd at 31.1% (below the 85% R3A threshold), so stitching ships as V1.1 work via LLM-based methods (Phi-3.5-mini or Qwen2.5-Coder-3B running locally).
- **Codex CLI adapter** — in flight as Lane A of this session's parallel build.
- **Hot-reload of projects.yaml + pricing.yaml** — currently requires restart.
- **Filesystem watch mode** — currently backfill-on-launch only.
- **Code signing + Apple Developer ID notarization** — Homebrew handles Gatekeeper quarantine for tap installs; direct binary downloads will need notarization.
- **Multi-machine sync** — V2 paid feature.
- **MCP server emitting agent-activity.v1 natively** — V1.1 leverage move.

### Schema policy

`agent-activity.v1` is in **DRAFT** status. Schema version `0.x.y`: additive changes allowed without consumer breakage; breaking changes bump the minor version. The wire format promotes to STABLE `1.0.0` once ≥2 independent adapters have operated against it for one continuous week without rewrite.

[Unreleased]: https://github.com/cfelmer-ctrlhwo/lens/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/cfelmer-ctrlhwo/lens/releases/tag/v0.1.0
