// Purpose: Friendly empty-list message shown when the timeline has zero rows.
// Process: Rendered by App.tsx when `events.length === 0 && !loading && !error`. The refresh button triggers a
//          full timeline re-fetch via the parent.
// Connections: Rendered by App.tsx. Styled by App.css (.lens-empty.*).

type Props = {
  onRefresh: () => void;
};

export function EmptyState({ onRefresh }: Props) {
  return (
    <div className="lens-empty">
      <h2 className="lens-empty__title">No events yet</h2>
      <p className="lens-empty__body">
        Ingestion isn't running yet, or no tools have produced sessions Lens can see.
      </p>
      <p className="lens-empty__body">
        Click <strong>Refresh</strong> to retry, run a Claude Code session and re-open Lens, or use the
        <code className="lens-empty__code">Insert demo event</code> dev button below to seed a synthetic row.
      </p>
      <button type="button" className="lens-button" onClick={onRefresh}>
        Refresh
      </button>
    </div>
  );
}
