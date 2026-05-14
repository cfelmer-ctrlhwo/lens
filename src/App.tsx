// Purpose: Top-level Lens UI surface. Stitches the header, filter chips, virtualized timeline, detail panel,
//          error banner, empty state, and a small dev panel (insert demo event + IPC bridge smoke check) together.
// Process: Owns the active EventFilters and selectedEventId state. Subscribes to useAppStatus (header counters,
//          2s poll) and useTimeline (event list + cursor pagination). Errors from either hook funnel into the
//          single ErrorBanner. Refresh re-runs both fetches in parallel.
// Connections: Reads/writes EventFilters, TimelineRow, AppStatus from ./types/ipc. Calls invoke("insert_demo_event")
//              and invoke("greet") from @tauri-apps/api/core directly only for the dev panel — everything else
//              goes through the hooks. Styled by ./App.css.

import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useState } from "react";
import "./App.css";

import { EmptyState } from "./components/EmptyState";
import { ErrorBanner } from "./components/ErrorBanner";
import { EventDetailPanel } from "./components/EventDetailPanel";
import { FilterChips } from "./components/FilterChips";
import { Header } from "./components/Header";
import { Timeline } from "./components/Timeline";
import { useAppStatus } from "./hooks/useAppStatus";
import { useTimeline } from "./hooks/useTimeline";
import type { EventFilters } from "./types/ipc";

function App() {
  const [filters, setFilters] = useState<EventFilters>({});
  const [selectedEventId, setSelectedEventId] = useState<string | null>(null);
  const [bridgeStatus, setBridgeStatus] = useState<string>("(checking)");
  const [demoBusy, setDemoBusy] = useState(false);
  const [demoError, setDemoError] = useState<string | null>(null);

  const { status, error: statusError, refresh: refreshStatus } = useAppStatus();
  const {
    events,
    loading: timelineLoading,
    error: timelineError,
    hasMore,
    fetchMore,
    refetch: refetchTimeline,
    clearError: clearTimelineError,
  } = useTimeline(filters);

  // One-off IPC smoke check — kept from the placeholder so dev builds can verify the bridge.
  useEffect(() => {
    invoke<string>("greet", { name: "Lens" })
      .then((reply) => setBridgeStatus(`OK -- ${reply}`))
      .catch((err) => setBridgeStatus(`ERROR -- ${String(err)}`));
  }, []);

  // Aggregate any error source into one dismissible banner. Whichever surfaced most recently wins;
  // dismissing clears the underlying source so it doesn't re-appear on next render.
  const aggregatedError = timelineError ?? statusError ?? demoError;
  const dismissError = useCallback(() => {
    clearTimelineError();
    setDemoError(null);
    // statusError clears itself on next successful poll; nothing to do here.
  }, [clearTimelineError]);

  // Distinct project + tool names from what we've loaded — feeds the filter chips. Includes the
  // currently-selected value (in case the chosen project/tool has scrolled off-page).
  const knownProjects = useMemo(() => {
    const set = new Set<string>();
    for (const e of events) if (e.project) set.add(e.project);
    if (filters.project) set.add(filters.project);
    return Array.from(set).sort((a, b) => a.localeCompare(b));
  }, [events, filters.project]);

  const knownTools = useMemo(() => {
    const set = new Set<string>();
    for (const e of events) set.add(e.tool);
    if (filters.tool) set.add(filters.tool);
    return Array.from(set).sort((a, b) => a.localeCompare(b));
  }, [events, filters.tool]);

  const handleRefresh = useCallback(() => {
    refetchTimeline();
    void refreshStatus();
  }, [refetchTimeline, refreshStatus]);

  const handleInsertDemo = useCallback(async () => {
    setDemoBusy(true);
    setDemoError(null);
    try {
      await invoke<string>("insert_demo_event");
      // Reload both views so the new row shows up at the top.
      refetchTimeline();
      void refreshStatus();
    } catch (err) {
      setDemoError(String(err));
    } finally {
      setDemoBusy(false);
    }
  }, [refetchTimeline, refreshStatus]);

  const showEmptyState =
    events.length === 0 && !timelineLoading && !timelineError;

  return (
    <main className="lens-app">
      <ErrorBanner message={aggregatedError ?? null} onDismiss={dismissError} />

      <Header status={status} onRefresh={handleRefresh} busy={timelineLoading} />

      <FilterChips
        filters={filters}
        knownProjects={knownProjects}
        knownTools={knownTools}
        onChange={setFilters}
      />

      <section className="lens-main">
        {showEmptyState ? (
          <EmptyState onRefresh={handleRefresh} />
        ) : (
          <Timeline
            events={events}
            hasMore={hasMore}
            loading={timelineLoading}
            onLoadMore={fetchMore}
            selectedEventId={selectedEventId}
            onSelect={setSelectedEventId}
          />
        )}
      </section>

      {selectedEventId ? (
        <EventDetailPanel
          eventId={selectedEventId}
          onClose={() => setSelectedEventId(null)}
        />
      ) : null}

      {import.meta.env.DEV ? (
        <footer className="lens-dev">
          <div className="lens-dev__row">
            <span className="lens-dev__label">Bridge</span>
            <span className="lens-dev__value">{bridgeStatus}</span>
          </div>
          <div className="lens-dev__row">
            <span className="lens-dev__label">Schema</span>
            <span className="lens-dev__value">
              agent-activity.v1 DRAFT 0.1.1
              {status ? ` / DB schema v${status.schema_version}` : ""}
            </span>
          </div>
          <div className="lens-dev__row">
            <button
              type="button"
              className="lens-button lens-button--ghost"
              onClick={handleInsertDemo}
              disabled={demoBusy}
            >
              {demoBusy ? "Inserting..." : "Insert demo event"}
            </button>
            <span className="lens-dev__hint">
              Click to verify the React → IPC → SQLite → IPC → React loop. Counters tick up; a new row appears.
            </span>
          </div>
        </footer>
      ) : null}
    </main>
  );
}

export default App;
