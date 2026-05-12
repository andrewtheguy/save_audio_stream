import { useState } from "react";
import { HlsUrlModal } from "./HlsUrlModal";

interface SessionInfo {
  section_id: number;
  start_id: number;
  end_id: number;
  timestamp_ms: number;
  duration_ms: number;
}

interface SessionCardProps {
  session: SessionInfo;
  isActive: boolean;
  onSelect: (session: SessionInfo) => void;
  audioFormat: string;
  showName?: string;
  savedPosition?: number;
  formatDuration: (seconds: number) => string;
  formatDateWithTimeRange: (startMs: number, endMs: number) => string;
  formatPosition: (seconds: number | undefined) => string;
}

export function SessionCard({
  session,
  isActive,
  onSelect,
  audioFormat,
  showName,
  savedPosition,
  formatDuration,
  formatDateWithTimeRange,
  formatPosition,
}: SessionCardProps) {
  const endTimestampMs = session.timestamp_ms + session.duration_ms;
  const [showHlsModal, setShowHlsModal] = useState(false);

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
          Duration: {formatDuration(session.duration_ms / 1000)}
        </span>
        <span className="session-position">
          Position: {formatPosition(savedPosition)}
        </span>
        <div className="session-actions">
          <button
            className="show-hls-btn"
            onClick={() => setShowHlsModal(true)}
            title="Show HLS URL"
          >
            HLS URL
          </button>
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
      </div>
      {showHlsModal && (
        <HlsUrlModal
          session={session}
          audioFormat={audioFormat}
          showName={showName}
          formatDateWithTimeRange={formatDateWithTimeRange}
          onClose={() => setShowHlsModal(false)}
        />
      )}
    </div>
  );
}

export type { SessionInfo };
