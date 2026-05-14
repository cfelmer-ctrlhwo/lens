// Purpose: TypeScript mirrors of the Rust Tauri IPC types from src-tauri/src/{ipc,storage,agent_activity}.
// Process: Imported by every component/hook that calls `invoke<...>(...)`. Single source of truth so a Rust-side
//          rename surfaces as a TS error in one place.
// Connections: Mirrors src-tauri/src/storage/query.rs (Cursor/EventFilters/EventPage/TimelineRow),
//              src-tauri/src/storage/issues.rs (StoredIssue), src-tauri/src/ipc/mod.rs (AppStatus),
//              src-tauri/src/agent_activity.rs (AgentActivityEvent).

/** Filter dimensions for the timeline. Fields are AND'd. Undefined = no constraint. */
export type EventFilters = {
  project?: string;
  tool?: string;
  status?: string;
};

/** Opaque cursor for pagination. Echoed back as-is on the next get_timeline call. */
export type Cursor = {
  before_ingest_seq: number;
};

/** Status values the Rust EventStatus enum serializes to. */
export type EventStatus = "success" | "failure" | "partial" | "unknown";

/** Compact projection used for timeline rendering. Mirror of storage::query::TimelineRow. */
export type TimelineRow = {
  event_id: string;
  ingest_seq: number;
  tool: string;
  project: string | null;
  /** ISO 8601 UTC string with Z suffix. */
  started_at: string;
  ended_at: string | null;
  /** Status as a raw string — we narrow at display time. */
  status: string;
  cost_usd_estimated: number | null;
  model: string | null;
};

/** One page of timeline events + cursor for the page after. */
export type EventPage = {
  events: TimelineRow[];
  next_cursor: Cursor | null;
};

/** Top-line counters that drive the header strip. */
export type AppStatus = {
  schema_version: number;
  total_events: number;
  total_issues: number;
  total_projects: number;
};

export type IssueSeverity = "fatal" | "recoverable";

/** One stored ingestion issue (parser failure, malformed record, etc.). */
export type StoredIssue = {
  issue_id: number;
  occurred_at: string;
  adapter: string;
  source_path: string | null;
  project: string | null;
  severity: IssueSeverity;
  reason: string;
  parser_version: string | null;
};

/** Event-type enum mirror. */
export type EventType =
  | "session_started"
  | "session_completed"
  | "session_failed"
  | "scheduled_run_completed"
  | "parser_error";

export type CostSource = "log_parse" | "api_billing" | "tool_reported" | "none";

/** Full agent-activity.v1 event. Mirror of agent_activity::AgentActivityEvent.
 *  All optional fields use `null` over IPC because serde skips None on the wire,
 *  but TypeScript-side they may be present-and-null, undefined, or filled. */
export type AgentActivityEvent = {
  schema_version: string;
  event_id: string;
  tool: string;
  tool_version?: string | null;
  event_type: EventType;
  started_at: string;
  ended_at?: string | null;
  status: EventStatus;
  session_id?: string | null;
  project?: string | null;
  cwd?: string | null;
  model?: string | null;
  provider?: string | null;
  tokens_in?: number | null;
  tokens_out?: number | null;
  tokens_total?: number | null;
  cost_usd_estimated?: number | null;
  cost_source?: CostSource | null;
  artifacts?: string[] | null;
  error_message?: string | null;
  summary?: string | null;
  tags?: string[] | null;
  raw_ref?: string | null;
  extra?: Record<string, unknown> | null;
};
