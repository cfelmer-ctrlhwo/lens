// Purpose: Slide-in detail panel for a clicked timeline row. Fetches the full AgentActivityEvent and renders
//          all fields, with `extra` (the heterogeneous raw payload) shown as a pretty-printed JSON block.
// Process: Mounted whenever `eventId` is non-null. Fires invoke("get_event_detail", { eventId }) on mount and
//          whenever the id changes. Renders a backdrop that closes the panel on click. Escape key also closes.
// Connections: Rendered by App.tsx when selectedEventId !== null. Calls invoke from @tauri-apps/api/core.
//              Reads AgentActivityEvent from ../types/ipc. Styled by App.css (.lens-detail.*).

import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { AgentActivityEvent } from "../types/ipc";

type Props = {
  eventId: string;
  onClose: () => void;
};

type FetchState =
  | { kind: "loading" }
  | { kind: "loaded"; event: AgentActivityEvent }
  | { kind: "missing" }
  | { kind: "error"; message: string };

/** Render a single label/value row inside the detail panel. Skips when value is null/undefined/empty-string. */
function Field({ label, value }: { label: string; value: React.ReactNode | string | null | undefined }) {
  if (value === null || value === undefined || value === "") return null;
  return (
    <div className="lens-detail__field">
      <div className="lens-detail__field-label">{label}</div>
      <div className="lens-detail__field-value">{value}</div>
    </div>
  );
}

/** Format an ISO 8601 timestamp as local "YYYY-MM-DD HH:mm:ss". Falls back to raw on parse failure. */
function fmtTimestamp(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const y = d.getFullYear();
  const mo = String(d.getMonth() + 1).padStart(2, "0");
  const da = String(d.getDate()).padStart(2, "0");
  const h = String(d.getHours()).padStart(2, "0");
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  return `${y}-${mo}-${da} ${h}:${m}:${s}`;
}

function fmtTokens(n: number | null | undefined): string | null {
  if (n === null || n === undefined) return null;
  return n.toLocaleString();
}

function fmtCost(n: number | null | undefined): string | null {
  if (n === null || n === undefined) return null;
  return `$${n.toFixed(4)}`;
}

export function EventDetailPanel({ eventId, onClose }: Props) {
  const [state, setState] = useState<FetchState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    setState({ kind: "loading" });
    invoke<AgentActivityEvent | null>("get_event_detail", { eventId })
      .then((event) => {
        if (cancelled) return;
        setState(event ? { kind: "loaded", event } : { kind: "missing" });
      })
      .catch((e) => {
        if (cancelled) return;
        setState({ kind: "error", message: String(e) });
      });
    return () => {
      cancelled = true;
    };
  }, [eventId]);

  // Close on Escape.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <>
      <div className="lens-detail__backdrop" onClick={onClose} aria-hidden />
      <aside className="lens-detail" role="dialog" aria-label="Event detail">
        <header className="lens-detail__header">
          <span className="lens-detail__title">Event detail</span>
          <button
            type="button"
            className="lens-detail__close"
            onClick={onClose}
            aria-label="Close detail panel"
          >
            ×
          </button>
        </header>

        {state.kind === "loading" ? (
          <div className="lens-detail__placeholder">Loading event…</div>
        ) : null}

        {state.kind === "error" ? (
          <div className="lens-detail__placeholder lens-detail__placeholder--error">
            Failed to load event: {state.message}
          </div>
        ) : null}

        {state.kind === "missing" ? (
          <div className="lens-detail__placeholder">
            Event <code>{eventId}</code> not found. It may have been deleted or never existed.
          </div>
        ) : null}

        {state.kind === "loaded" ? (
          <DetailBody event={state.event} />
        ) : null}
      </aside>
    </>
  );
}

function DetailBody({ event }: { event: AgentActivityEvent }) {
  return (
    <div className="lens-detail__body">
      <Field label="event_id" value={<code className="lens-detail__code">{event.event_id}</code>} />
      <Field label="tool" value={event.tool} />
      <Field label="tool_version" value={event.tool_version} />
      <Field label="event_type" value={event.event_type} />
      <Field label="status" value={event.status} />
      <Field label="started_at" value={fmtTimestamp(event.started_at)} />
      <Field label="ended_at" value={fmtTimestamp(event.ended_at)} />
      <Field label="session_id" value={event.session_id} />
      <Field label="project" value={event.project} />
      <Field label="cwd" value={event.cwd ? <code className="lens-detail__code">{event.cwd}</code> : null} />
      <Field label="model" value={event.model} />
      <Field label="provider" value={event.provider} />
      <Field label="tokens_in" value={fmtTokens(event.tokens_in)} />
      <Field label="tokens_out" value={fmtTokens(event.tokens_out)} />
      <Field label="tokens_total" value={fmtTokens(event.tokens_total)} />
      <Field label="cost_usd_estimated" value={fmtCost(event.cost_usd_estimated)} />
      <Field label="cost_source" value={event.cost_source} />
      <Field
        label="artifacts"
        value={
          event.artifacts && event.artifacts.length > 0 ? (
            <ul className="lens-detail__list">
              {event.artifacts.map((a, i) => (
                <li key={i}>
                  <code className="lens-detail__code">{a}</code>
                </li>
              ))}
            </ul>
          ) : null
        }
      />
      <Field label="error_message" value={event.error_message} />
      <Field label="summary" value={event.summary} />
      <Field
        label="tags"
        value={
          event.tags && event.tags.length > 0 ? (
            <div className="lens-detail__tags">
              {event.tags.map((t) => (
                <span key={t} className="lens-detail__tag">
                  {t}
                </span>
              ))}
            </div>
          ) : null
        }
      />
      <Field
        label="raw_ref"
        value={event.raw_ref ? <code className="lens-detail__code">{event.raw_ref}</code> : null}
      />
      {event.extra ? (
        <div className="lens-detail__field">
          <div className="lens-detail__field-label">extra</div>
          <pre className="lens-detail__json">{JSON.stringify(event.extra, null, 2)}</pre>
        </div>
      ) : null}
      <div className="lens-detail__field">
        <div className="lens-detail__field-label">raw event JSON</div>
        <pre className="lens-detail__json">{JSON.stringify(event, null, 2)}</pre>
      </div>
    </div>
  );
}
