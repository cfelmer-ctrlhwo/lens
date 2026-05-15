# Lens

> Mac-native AI activity dashboard. Parses your Claude Code, Codex, and other AI tool session logs into a unified project-first timeline. No cloud account, no API auth, no instrumentation of your tools — just observation of what's already there.

**Status:** pre-V1 (Week 0 — Day 1 data spike complete, scaffold in progress)
**License:** MIT (planned)
**Schema:** [agent-activity.v1](docs/agent-activity-v1.md) (DRAFT 0.1.1, OpenTelemetry GenAI aligned)

## What V1 will ship

- Unified **chronological timeline** across multiple AI tools (Claude Code, Codex in V1; Grok, Perplexity, Paperclip in V1.x)
- **Project-first organization** — not tool-first. Lens groups your work by what you're building, not which model you used.
- **Cost tracking** — per-session, per-project, per-week. Derived from session-log token counts and a manually-maintained pricing table ([`pricing.yaml`](pricing.yaml)).
- **Open emission schema** — [`agent-activity.v1`](docs/agent-activity-v1.md). Lens consumes JSONL/JSON tool logs in V1; any AI tool can natively emit `agent-activity.v1` events in V1.1+ instead of being scraped.
- **Local-first.** SQLite event store. Nothing leaves your Mac. Optional encrypted iCloud sync is a V2 paid feature.

## What V1.1 will add (not V1)

Causal **stitching across tools** — the "morning replay" feature where Lens auto-assembles overnight activity into per-project episodes with causal arrows. The Day 1 spike (2026-05-14) found Codex's session-level cwd presence is only 31.1%, which fails the per-adapter usability gate. V1.1 implements LLM-based stitching using a local 3B-class model (Phi-3.5-mini-instruct or Qwen2.5-Coder-3B-Instruct), informed by the [Skill Boundary Detection](https://hf.co/papers/2503.10684) prediction-error technique.

## Install (planned)

```bash
# Once V1 ships:
brew install cfelmer-ctrlhwo/lens/lens
```

Direct binary download will also be available from GitHub Releases. macOS-only at launch; Linux + Windows follow because Tauri is cross-platform.

## Build from source

```bash
git clone https://github.com/cfelmer-ctrlhwo/lens
cd lens
bun install
cargo tauri dev   # local dev mode
cargo tauri build # production build
```

(Tauri scaffold to be added Saturday 2026-05-17.)

## Schema spec

The wire format for AI activity events is documented in [`docs/agent-activity-v1.md`](docs/agent-activity-v1.md) — 12 sections, 5 event types, full field reference, adapter + consumer contracts, OpenTelemetry GenAI alignment table, worked examples. The schema is intentionally minimal in V1 and grows additively.

## Related projects

Lens sits in the **observe / unify** quadrant of the multi-agent space. Most existing projects are in the **dispatch / orchestrate** quadrant — they spawn agents; Lens shows you what those agents did. They feed each other naturally.

**Complementary upstream — agent dispatchers / orchestrators that create the activity Lens observes:**
- [ComposioHQ/agent-orchestrator](https://github.com/ComposioHQ/agent-orchestrator) (7K stars, enterprise scale) — plans tasks, spawns agents in worktrees, routes CI feedback and review comments back to agents. Has a `tracker` plugin slot — natural integration surface for a future Lens emitter plugin.
- [johannesjo/parallel-code](https://github.com/johannesjo/parallel-code) (627 stars, solo-dev scale) — desktop GUI that runs Claude Code + Codex + Gemini side-by-side, each in its own git worktree.
- [winfunc/opcode](https://github.com/winfunc/opcode) (21K stars) — Tauri-based Claude Code session launcher + custom-agent IDE. Single-tool focus, but the most popular dispatcher.
- [qingchencloud/clawpanel](https://github.com/qingchencloud/clawpanel) (2.7K stars) — multi-engine AI management panel for OpenClaw + Hermes.

**Schema alignment partner:**
- [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/) — `agent-activity.v1` aliases its fields into OTel attribute paths so OTel-aware tools can consume Lens events without translation. See `docs/agent-activity-v1.md` §3.1.

**Different market — production AI observability** (teams running AI services in production, not solo devs running many AI tools on their Mac):
- [Langfuse](https://github.com/langfuse/langfuse), [Opik](https://github.com/comet-ml/opik), [Helicone](https://github.com/Helicone/helicone), [Logfire](https://github.com/pydantic/logfire), [Langtrace](https://github.com/Scale3-Labs/langtrace) (OTel-based), [Laminar](https://github.com/lmnr-ai/lmnr).

**V2 emission target** — projects that build agent runtime + would benefit from native `agent-activity.v1` emission:
- [Agent-Field/agentfield](https://github.com/Agent-Field/agentfield) (1.8K stars + ecosystem) — agent backend with "observable from day one" as a marketing pillar.

## Contributing

Pre-V1 — internal build. After V1 ships, contributions welcome via PR. Adapter PRs for additional AI tools especially welcome; see the schema spec's adapter contract (§8) for what a new adapter must do.

## Design documents (internal)

Build context lives in `~/.gstack/projects/ClaudeCode/`:
- `cfelmer-unknown-design-20260514-134014.md` — main design doc (post-review, scope locked)
- `agent-activity-v1.md` — schema spec (DRAFT 0.1.1)
- `cfelmer-unknown-eng-review-test-plan-20260514-152500.md` — test plan
- `day1-spike-results.md` — Day 1 data-availability spike output
- `lens-starter/PORT-REFERENCE-session-ingester.md` — Rust adapter port reference

These will not be committed to the public repo. They are build-time scaffolding.

---

Generated by `claude` + `gstack` skills + a lot of careful review. See the design doc for the full provenance.
