// Purpose: Poll get_app_status every N seconds so the header counters stay live.
// Process: Hook starts polling on mount. Pauses when document.visibilityState !== "visible" so we don't burn cycles
//          while the window is in the background. First fetch runs immediately, then on the interval.
// Connections: Calls invoke("get_app_status") via @tauri-apps/api/core. Returns the AppStatus type from ../types/ipc.
//              Consumed by App.tsx → Header.tsx.

import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";
import type { AppStatus } from "../types/ipc";

type UseAppStatusReturn = {
  status: AppStatus | null;
  error: string | null;
  refresh: () => Promise<void>;
};

const POLL_INTERVAL_MS = 2000;

/**
 * Live poll of get_app_status. Polling pauses when the window is hidden (per Page Visibility API) and resumes
 * on visibilitychange. Errors are exposed but don't stop polling — the next tick might succeed (e.g. transient
 * mutex contention is possible during ingestion).
 */
export function useAppStatus(): UseAppStatusReturn {
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await invoke<AppStatus>("get_app_status");
      setStatus(next);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    let intervalId: number | null = null;

    const tick = () => {
      if (!cancelled && document.visibilityState === "visible") {
        void refresh();
      }
    };

    // Immediate fetch on mount.
    tick();

    const start = () => {
      if (intervalId !== null) return;
      intervalId = window.setInterval(tick, POLL_INTERVAL_MS);
    };
    const stop = () => {
      if (intervalId !== null) {
        window.clearInterval(intervalId);
        intervalId = null;
      }
    };

    const onVisibility = () => {
      if (document.visibilityState === "visible") {
        // Re-fetch immediately on resume so the user sees fresh data right away.
        void refresh();
        start();
      } else {
        stop();
      }
    };

    if (document.visibilityState === "visible") start();
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      cancelled = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh]);

  return { status, error, refresh };
}
