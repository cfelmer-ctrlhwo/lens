// Purpose: Top "Lens" banner with title + live counters + manual refresh button.
// Process: Renders AppStatus values from the useAppStatus hook (passed down through App.tsx). Refresh button
//          delegates to a parent callback that re-runs both the timeline fetch and the app-status fetch.
// Connections: Rendered by App.tsx. Receives `status` from useAppStatus, `onRefresh` from the parent which
//              coordinates a full refetch. Styled by App.css (.lens-header.*).

import type { AppStatus } from "../types/ipc";

type Props = {
  status: AppStatus | null;
  onRefresh: () => void;
  busy?: boolean;
};

/** Format a possibly-null count for display. "..." while loading, otherwise locale-separated. */
function formatCount(value: number | undefined | null): string {
  if (value === undefined || value === null) return "...";
  return value.toLocaleString();
}

export function Header({ status, onRefresh, busy }: Props) {
  return (
    <header className="lens-header">
      <div className="lens-header__title-row">
        <h1 className="lens-header__title">Lens</h1>
        <p className="lens-header__tagline">Mac-native AI activity dashboard</p>
      </div>

      <div className="lens-header__stats">
        <div className="lens-header__stat">
          <span className="lens-header__stat-label">events</span>
          <span className="lens-header__stat-value">{formatCount(status?.total_events)}</span>
        </div>
        <div className="lens-header__stat">
          <span className="lens-header__stat-label">projects</span>
          <span className="lens-header__stat-value">{formatCount(status?.total_projects)}</span>
        </div>
        <div className="lens-header__stat">
          <span className="lens-header__stat-label">issues</span>
          <span
            className={
              "lens-header__stat-value " +
              (status && status.total_issues > 0 ? "lens-header__stat-value--warn" : "")
            }
          >
            {formatCount(status?.total_issues)}
          </span>
        </div>
        <button
          type="button"
          className="lens-button lens-header__refresh"
          onClick={onRefresh}
          disabled={busy}
          aria-label="Refresh timeline and status"
        >
          {busy ? "Refreshing..." : "Refresh"}
        </button>
      </div>
    </header>
  );
}
