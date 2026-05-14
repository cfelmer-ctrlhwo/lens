// Purpose: Manage the timeline event list — fetch the first page, paginate via cursor, refetch on filter change.
// Process: Component mounts → useTimeline(filters) fires the first fetch. Filter change → reset + refetch. Scroll
//          near the bottom → consumer calls fetchMore() to append the next page. Errors surface via the `error`
//          string and stop pagination until cleared.
// Connections: Calls invoke("get_timeline", ...) from @tauri-apps/api/core. Reads/writes EventFilters + Cursor
//              from ../types/ipc. Consumed by Timeline.tsx (and indirectly by App.tsx for the empty-state check).

import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Cursor, EventFilters, EventPage, TimelineRow } from "../types/ipc";

type UseTimelineReturn = {
  events: TimelineRow[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  fetchMore: () => void;
  refetch: () => void;
  clearError: () => void;
};

/**
 * Fetch + paginate the timeline. Resets on any filter change.
 *
 * The pagination cursor lives in the latest page object — we don't need to thread it through state separately.
 * A small `requestId` ref guards against stale responses landing after a filter change. (React 19 is fast enough
 * that this is rare in practice, but the cost is tiny and it makes the hook obviously correct.)
 */
export function useTimeline(filters: EventFilters): UseTimelineReturn {
  const [pages, setPages] = useState<EventPage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Bumps on every reset; in-flight responses with a stale id are discarded.
  const requestIdRef = useRef(0);

  const fetchPage = useCallback(
    async (cursor: Cursor | undefined, fetchId: number) => {
      setLoading(true);
      try {
        const page = await invoke<EventPage>("get_timeline", {
          filters,
          cursor: cursor ?? null,
        });
        // If a newer fetch has started, drop this response.
        if (fetchId !== requestIdRef.current) return;
        setPages((prev) => (cursor === undefined ? [page] : [...prev, page]));
        setError(null);
      } catch (e) {
        if (fetchId !== requestIdRef.current) return;
        setError(String(e));
      } finally {
        if (fetchId === requestIdRef.current) setLoading(false);
      }
    },
    [filters],
  );

  // Reset + first-page fetch on filter change. The filter fields are primitive
  // so the dep array is stable across re-renders with the same values.
  useEffect(() => {
    const id = ++requestIdRef.current;
    setPages([]);
    setError(null);
    fetchPage(undefined, id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filters.project, filters.tool, filters.status]);

  const events = useMemo(() => pages.flatMap((p) => p.events), [pages]);

  const nextCursor = pages.length > 0 ? pages[pages.length - 1].next_cursor : null;
  const hasMore = nextCursor !== null;

  const fetchMore = useCallback(() => {
    if (!nextCursor || loading) return;
    fetchPage(nextCursor, requestIdRef.current);
  }, [nextCursor, loading, fetchPage]);

  const refetch = useCallback(() => {
    const id = ++requestIdRef.current;
    setPages([]);
    setError(null);
    fetchPage(undefined, id);
  }, [fetchPage]);

  const clearError = useCallback(() => setError(null), []);

  return { events, loading, error, hasMore, fetchMore, refetch, clearError };
}
