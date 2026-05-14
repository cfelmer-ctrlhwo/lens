// Purpose: Virtualized list of TimelineRowView items + scroll-anchored auto-pagination.
// Process: Uses @tanstack/react-virtual's useVirtualizer over a scrollable parent ref. When the last rendered
//          virtual item index is within `overscan` of the events length, calls onLoadMore() to fetch the next
//          page. The parent (App.tsx) owns the loaded events and the fetchMore callback.
// Connections: Rendered by App.tsx. Reads TimelineRow from ../types/ipc. Renders TimelineRowView for each item.
//              Styled by App.css (.lens-timeline.*).

import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef } from "react";
import type { TimelineRow } from "../types/ipc";
import { TimelineRowView } from "./TimelineRowView";

type Props = {
  events: TimelineRow[];
  hasMore: boolean;
  loading: boolean;
  onLoadMore: () => void;
  selectedEventId: string | null;
  onSelect: (eventId: string) => void;
};

const ROW_HEIGHT = 56;
const OVERSCAN = 10;

export function Timeline({
  events,
  hasMore,
  loading,
  onLoadMore,
  selectedEventId,
  onSelect,
}: Props) {
  const parentRef = useRef<HTMLDivElement | null>(null);

  const virtualizer = useVirtualizer({
    count: events.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: OVERSCAN,
  });

  // Trigger pagination when the rendered window approaches the end. We watch the virtual items every render —
  // this is cheap (the array is small, capped at visible+overscan rows). Avoid infinite re-fetches by gating on
  // !loading and hasMore.
  const virtualItems = virtualizer.getVirtualItems();
  const lastIndex = virtualItems.length > 0 ? virtualItems[virtualItems.length - 1].index : -1;

  useEffect(() => {
    if (!hasMore || loading) return;
    if (events.length === 0) return;
    if (lastIndex >= events.length - OVERSCAN) {
      onLoadMore();
    }
    // We intentionally don't depend on onLoadMore — its identity changes on every parent render and the
    // dependency array above is the right gate (lastIndex change is what should trigger a re-check).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastIndex, hasMore, loading, events.length]);

  return (
    <div className="lens-timeline" ref={parentRef}>
      {/* Inner spacer sized to the total virtualized height so the scrollbar reflects the full list. */}
      <div
        className="lens-timeline__spacer"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {virtualItems.map((virtualItem) => {
          const event = events[virtualItem.index];
          if (!event) return null;
          return (
            <div
              key={event.event_id}
              className="lens-timeline__row-slot"
              style={{
                height: `${virtualItem.size}px`,
                transform: `translateY(${virtualItem.start}px)`,
              }}
            >
              <TimelineRowView
                row={event}
                isSelected={selectedEventId === event.event_id}
                onClick={onSelect}
              />
            </div>
          );
        })}
      </div>
      {loading && events.length > 0 ? (
        <div className="lens-timeline__loading">loading more…</div>
      ) : null}
      {!hasMore && events.length > 0 ? (
        <div className="lens-timeline__end">end of timeline ({events.length} events)</div>
      ) : null}
    </div>
  );
}
