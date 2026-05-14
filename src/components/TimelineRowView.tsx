// Purpose: Render a single timeline row — status dot, time, tool chip, project, model, cost.
// Process: Pure presentational component. Receives one TimelineRow + onClick callback. The parent (Timeline.tsx)
//          places it inside the virtualizer's absolute-positioned row container, so this component just fills
//          its parent and stays vertical-rhythm clean.
// Connections: Rendered by Timeline.tsx for each virtualized item. Reads TimelineRow from ../types/ipc.
//              Styled by App.css (.lens-row.*).

import type { TimelineRow } from "../types/ipc";

type Props = {
  row: TimelineRow;
  isSelected: boolean;
  onClick: (eventId: string) => void;
};

/** Format an ISO 8601 UTC timestamp as local "HH:mm:ss". Falls back to the raw string on parse failure. */
function formatTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const h = String(d.getHours()).padStart(2, "0");
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  return `${h}:${m}:${s}`;
}

/** Format the started_at date as "YYYY-MM-DD". Used to label day-change boundaries (the row shows time alone). */
function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const y = d.getFullYear();
  const mo = String(d.getMonth() + 1).padStart(2, "0");
  const da = String(d.getDate()).padStart(2, "0");
  return `${y}-${mo}-${da}`;
}

/** Format cost as `$0.234`. Returns "--" when null. */
function formatCost(cost: number | null): string {
  if (cost === null || cost === undefined) return "--";
  if (cost === 0) return "$0.000";
  // Pad to 3 decimal places. Costs are small (sub-dollar typical), so we leave them as e.g. "$0.005".
  return `$${cost.toFixed(3)}`;
}

/** Deterministic color hash for tool name. Uses a small palette so the timeline stays calm.
 *  djb2-ish hash, mod palette length. Same input always yields the same color. */
const TOOL_PALETTE = [
  { fg: "#1e7ae0", bg: "rgba(30, 122, 224, 0.10)" }, // blue
  { fg: "#1f9b50", bg: "rgba(31, 155, 80, 0.10)" }, // green
  { fg: "#b66200", bg: "rgba(182, 98, 0, 0.10)" }, // amber
  { fg: "#8b5cf6", bg: "rgba(139, 92, 246, 0.10)" }, // violet
  { fg: "#0f9b9b", bg: "rgba(15, 155, 155, 0.10)" }, // teal
  { fg: "#d04085", bg: "rgba(208, 64, 133, 0.10)" }, // pink
];

function toolColors(tool: string): { fg: string; bg: string } {
  let h = 5381;
  for (let i = 0; i < tool.length; i++) {
    h = ((h << 5) + h + tool.charCodeAt(i)) | 0;
  }
  const idx = Math.abs(h) % TOOL_PALETTE.length;
  return TOOL_PALETTE[idx];
}

function statusDotClass(status: string): string {
  switch (status) {
    case "success":
      return "lens-row__dot lens-row__dot--success";
    case "failure":
      return "lens-row__dot lens-row__dot--failure";
    case "partial":
      return "lens-row__dot lens-row__dot--partial";
    default:
      return "lens-row__dot lens-row__dot--unknown";
  }
}

export function TimelineRowView({ row, isSelected, onClick }: Props) {
  const { fg, bg } = toolColors(row.tool);
  const projectDisplay = row.project ?? "Uncategorized";
  const projectIsUncategorized = row.project === null;

  return (
    <button
      type="button"
      className={"lens-row" + (isSelected ? " lens-row--selected" : "")}
      onClick={() => onClick(row.event_id)}
      title={`${row.tool} · ${projectDisplay} · ${row.event_id}`}
    >
      <span className={statusDotClass(row.status)} aria-label={`status ${row.status}`} />
      <span className="lens-row__time">
        <span className="lens-row__time-date">{formatDate(row.started_at)}</span>
        <span className="lens-row__time-clock">{formatTime(row.started_at)}</span>
      </span>
      <span
        className="lens-row__tool"
        style={{ color: fg, backgroundColor: bg }}
        title={`tool: ${row.tool}`}
      >
        {row.tool}
      </span>
      <span
        className={
          "lens-row__project" + (projectIsUncategorized ? " lens-row__project--dim" : "")
        }
      >
        {projectDisplay}
      </span>
      <span className="lens-row__model">{row.model ?? "--"}</span>
      <span className="lens-row__cost">{formatCost(row.cost_usd_estimated)}</span>
    </button>
  );
}
