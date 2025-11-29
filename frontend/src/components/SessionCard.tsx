import { React } from "../../deps.ts";

interface SessionInfo {
  section_id: number;
  start_id: number;
  end_id: number;
  timestamp_ms: number;
  duration_seconds: number;
}

interface SessionCardProps {
  session: SessionInfo;
  isActive: boolean;
  onSelect: (session: SessionInfo) => void;
  getHlsUrl: (session: SessionInfo) => string;
  savedPosition?: number;
  formatDuration: (seconds: number) => string;
  formatDateWithTimeRange: (startMs: number, endMs: number) => string;
  formatPosition: (seconds: number | undefined) => string;
}

export function SessionCard({
  session,
  isActive,
  onSelect,
  getHlsUrl,
  savedPosition,
  formatDuration,
  formatDateWithTimeRange,
  formatPosition,
}: SessionCardProps) {
  const endTimestampMs = session.timestamp_ms + session.duration_seconds * 1000;

  return (
    <div
      key={session.section_id}
      className={`session-card ${isActive ? "active" : ""}`}
    >
      <div className="session-header">
        <span className="session-time">
          {formatDateWithTimeRange(session.timestamp_ms, endTimestampMs)}
        </span>
        <span className="session-duration">
          Duration: {formatDuration(session.duration_seconds)}
        </span>
        <span className="session-position">
          Position: {formatPosition(savedPosition)}
        </span>
        {isActive ? (
          <span className="active-badge">Active</span>
        ) : (
          <button
            className="select-btn"
            onClick={() => {
              onSelect(session);
              window.scrollTo({ top: 0, behavior: "smooth" });
            }}
          >
            Select
          </button>
        )}
      </div>
      <div className="session-info">
        <div className="url-row">
          <span className="url-label">HLS:</span>
          <a
            href={getHlsUrl(session)}
            className="url-link"
            target="_blank"
            rel="noopener noreferrer"
          >
            {getHlsUrl(session)}
          </a>
        </div>
      </div>
    </div>
  );
}

export type { SessionInfo };
