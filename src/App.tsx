import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

// Lens pre-V1 placeholder. Real timeline UI lands during Week 1 — this view
// renders live AppStatus from the Rust backend so we can see the system
// breathing end-to-end even before the parser produces real events.

type AppStatus = {
  schema_version: number;
  total_events: number;
  total_issues: number;
  total_projects: number;
};

function App() {
  const [bridgeStatus, setBridgeStatus] = useState<string>("(checking)");
  const [appStatus, setAppStatus] = useState<AppStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refreshStatus = useCallback(async () => {
    try {
      const status = await invoke<AppStatus>("get_app_status");
      setAppStatus(status);
      setStatusError(null);
    } catch (err) {
      setStatusError(String(err));
    }
  }, []);

  useEffect(() => {
    // Smoke-test the IPC bridge once on mount.
    invoke<string>("greet", { name: "Lens" })
      .then((reply) => setBridgeStatus(`OK -- ${reply}`))
      .catch((err) => setBridgeStatus(`ERROR -- ${String(err)}`));

    // Fetch real app status from the new IPC commands.
    refreshStatus();
  }, [refreshStatus]);

  const handleInsertDemo = useCallback(async () => {
    setBusy(true);
    try {
      const id = await invoke<string>("insert_demo_event");
      console.log("[lens] inserted demo event:", id);
      await refreshStatus();
    } catch (err) {
      setStatusError(String(err));
    } finally {
      setBusy(false);
    }
  }, [refreshStatus]);

  return (
    <main className="lens-placeholder">
      <header>
        <h1>Lens</h1>
        <p className="tagline">Mac-native AI activity dashboard</p>
      </header>

      <section className="status">
        <div className="status-row">
          <span className="label">Stage:</span>
          <span className="value">pre-V1 scaffold</span>
        </div>
        <div className="status-row">
          <span className="label">Schema:</span>
          <span className="value">
            {appStatus
              ? `agent-activity.v1 DRAFT 0.1.1 / DB schema v${appStatus.schema_version}`
              : "agent-activity.v1 DRAFT 0.1.1"}
          </span>
        </div>
        <div className="status-row">
          <span className="label">Bridge:</span>
          <span className="value">{bridgeStatus}</span>
        </div>
        {statusError ? (
          <div className="status-row error">
            <span className="label">Error:</span>
            <span className="value">{statusError}</span>
          </div>
        ) : null}
      </section>

      <section className="status">
        <div className="status-row">
          <span className="label">Events:</span>
          <span className="value mono">
            {appStatus ? appStatus.total_events.toLocaleString() : "..."}
          </span>
        </div>
        <div className="status-row">
          <span className="label">Projects:</span>
          <span className="value mono">
            {appStatus ? appStatus.total_projects.toLocaleString() : "..."}
          </span>
        </div>
        <div className="status-row">
          <span className="label">Issues:</span>
          <span className="value mono">
            {appStatus ? appStatus.total_issues.toLocaleString() : "..."}
          </span>
        </div>
        <div className="actions">
          <button onClick={refreshStatus} disabled={busy}>
            Refresh
          </button>
          <button onClick={handleInsertDemo} disabled={busy}>
            {busy ? "Inserting..." : "Insert demo event"}
          </button>
          <span className="hint">
            Click "Insert demo event" to verify the React → IPC → SQLite → IPC → React loop end-to-end.
            Counters above will tick up.
          </span>
        </div>
      </section>

      <section className="next">
        <h2>What's next</h2>
        <ol>
          <li>Port <code>claude_code.rs</code> parse() body per <code>PORT-REFERENCE-session-ingester.md</code></li>
          <li>Ingestion pipeline: walk <code>~/.claude/projects/**/*.jsonl</code>, debounce, UPSERT</li>
          <li>Replace this placeholder with a virtualized timeline view</li>
          <li>Filter chips: project, tool, status</li>
          <li>Codex adapter (V1, chronological-only)</li>
        </ol>
        <p className="defer">
          Causal stitching deferred to V1.1. Day 1 data spike (2026-05-14) found
          Codex per-session usable cwd at 31.1%, below the R3A 85% threshold.
        </p>
      </section>

      <footer>
        <span>Lens / MIT / github.com/cfelmer/lens</span>
      </footer>
    </main>
  );
}

export default App;
