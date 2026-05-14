// Purpose: Three rows of filter chips (project / tool / status) above the timeline list.
// Process: Parent (App.tsx) owns the current EventFilters and the list of known values it has seen so far.
//          Clicking a chip calls onChange with the new filter object. Clicking the currently-selected chip
//          clears that dimension (sets it to undefined).
// Connections: Rendered by App.tsx. Reads EventFilters from ../types/ipc. Styled by App.css (.lens-filters.*).
//              Note: V1 backend EventFilters only accepts ONE value per dimension (not multi-select).
//              The brief mentions multi-select for project/tool — see LANE_C_REPORT.md for the deviation.

import type { EventFilters } from "../types/ipc";

type Props = {
  filters: EventFilters;
  /** Distinct project names observed in the loaded timeline, plus filter currently-selected. */
  knownProjects: string[];
  knownTools: string[];
  onChange: (next: EventFilters) => void;
};

const STATUSES = ["success", "failure", "partial", "unknown"] as const;

type Dim = "project" | "tool" | "status";

function ChipRow({
  label,
  options,
  selected,
  onSelect,
}: {
  label: string;
  options: string[];
  selected: string | undefined;
  onSelect: (value: string | undefined) => void;
}) {
  return (
    <div className="lens-filters__row">
      <span className="lens-filters__row-label">{label}</span>
      <div className="lens-filters__chips">
        <button
          type="button"
          className={
            "lens-chip" + (selected === undefined ? " lens-chip--selected" : "")
          }
          onClick={() => onSelect(undefined)}
        >
          all
        </button>
        {options.map((opt) => (
          <button
            key={opt}
            type="button"
            className={
              "lens-chip" + (selected === opt ? " lens-chip--selected" : "")
            }
            onClick={() => onSelect(selected === opt ? undefined : opt)}
            title={opt}
          >
            {opt}
          </button>
        ))}
      </div>
    </div>
  );
}

export function FilterChips({ filters, knownProjects, knownTools, onChange }: Props) {
  const setDim = (dim: Dim, value: string | undefined) => {
    onChange({ ...filters, [dim]: value });
  };

  return (
    <section className="lens-filters" aria-label="Timeline filters">
      <ChipRow
        label="project"
        options={knownProjects}
        selected={filters.project}
        onSelect={(v) => setDim("project", v)}
      />
      <ChipRow
        label="tool"
        options={knownTools}
        selected={filters.tool}
        onSelect={(v) => setDim("tool", v)}
      />
      <ChipRow
        label="status"
        options={[...STATUSES]}
        selected={filters.status}
        onSelect={(v) => setDim("status", v)}
      />
    </section>
  );
}
