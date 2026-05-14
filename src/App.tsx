import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

// Pre-V1 placeholder. The real Lens UI lands during Week 1 of the build.
// For now this confirms: Tauri shell launches, Rust <-> React IPC works,
// the scaffold is alive.

function App() {
  const [ipcStatus, setIpcStatus] = useState<string>("(checking)");

  useEffect(() => {
    // Smoke-test the Tauri IPC bridge so we know the shell is wired correctly.
    // Calls the default `greet` command included with `bun create tauri-app`.
    invoke<string>("greet", { name: "Lens" })
      .then((reply) => setIpcStatus(`IPC bridge OK -- ${reply}`))
      .catch((err) => setIpcStatus(`IPC bridge ERROR -- ${String(err)}`));
  }, []);

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
          <span className="value">agent-activity.v1 DRAFT 0.1.1 / OpenTelemetry-aligned</span>
        </div>
        <div className="status-row">
          <span className="label">Ingestion:</span>
          <span className="value">not yet wired</span>
        </div>
        <div className="status-row">
          <span className="label">Bridge:</span>
          <span className="value">{ipcStatus}</span>
        </div>
      </section>

      <section className="next">
        <h2>What's next</h2>
        <ol>
          <li>Port <code>claude_code.rs</code> adapter per <code>PORT-REFERENCE-session-ingester.md</code></li>
          <li>SQLite hybrid store (hot columns + raw_event JSON, WAL mode, UPSERT semantics)</li>
          <li>Cursor-paginated timeline IPC</li>
          <li>Virtualized React timeline UI</li>
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
