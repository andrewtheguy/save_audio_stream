import { React } from "../../deps.ts";

interface DateFilterControlsProps {
  dateFilter: string;
  onFilterChange: (date: string) => void;
  onClearFilter: () => void;
  sessionCount: number;
  inputId?: string;
}

export function DateFilterControls({
  dateFilter,
  onFilterChange,
  onClearFilter,
  sessionCount,
  inputId = "date-filter",
}: DateFilterControlsProps) {
  return (
    <div className="filter-controls">
      <div className="filter-group">
        <label htmlFor={inputId}>Date:</label>
        <input
          type="date"
          id={inputId}
          value={dateFilter}
          onChange={(e: React.ChangeEvent<HTMLInputElement>) => onFilterChange(e.target.value)}
          className="date-input"
        />
        {dateFilter && (
          <button
            className="clear-filter-btn"
            onClick={onClearFilter}
            title="Clear date filter"
          >
            Clear
          </button>
        )}
      </div>
      <div className="filter-info">
        {dateFilter
          ? `${sessionCount} session${sessionCount !== 1 ? "s" : ""} on ${dateFilter}`
          : `${sessionCount} session${sessionCount !== 1 ? "s" : ""} total`}
      </div>
    </div>
  );
}
