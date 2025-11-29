import { React } from "../../deps.ts";
import { AudioPlayer } from "./AudioPlayer.tsx";
import type { SessionInfo } from "./SessionCard.tsx";

interface NowPlayingSectionProps {
  activeSession: SessionInfo | null;
  audioFormat: string;
  dbUniqueId: string;
  getSavedPosition: (sectionId: number) => number | undefined;
  getHlsUrl: (session: SessionInfo) => string;
  onGoToSession: () => void;
  formatDuration: (seconds: number) => string;
  formatDateWithTimeRange: (startMs: number, endMs: number) => string;
  showName?: string;
}

export function NowPlayingSection({
  activeSession,
  audioFormat,
  dbUniqueId,
  getSavedPosition,
  getHlsUrl,
  onGoToSession,
  formatDuration,
  formatDateWithTimeRange,
  showName,
}: NowPlayingSectionProps) {
  if (!activeSession) {
    return (
      <div className="now-playing-section">
        <div className="now-playing-placeholder">
          Select a session to play
        </div>
      </div>
    );
  }

  const endTimestampMs = activeSession.timestamp_ms + activeSession.duration_seconds * 1000;

  return (
    <div className="now-playing-section">
      <div className="now-playing-info">
        <span className="now-playing-label">Now Playing:</span>
        <span className="now-playing-time">
          {formatDateWithTimeRange(activeSession.timestamp_ms, endTimestampMs)}
        </span>
        <span className="now-playing-duration">
          Duration: {formatDuration(activeSession.duration_seconds)}
        </span>
        <button
          className="go-to-session-btn"
          onClick={onGoToSession}
          title="Show this session in the list below"
        >
          Go to Session
        </button>
        <button
          className="copy-hls-btn"
          onClick={() => {
            navigator.clipboard.writeText(window.location.origin + getHlsUrl(activeSession));
          }}
          title={getHlsUrl(activeSession)}
        >
          Copy HLS
        </button>
      </div>
      <AudioPlayer
        key={activeSession.section_id}
        format={audioFormat}
        startId={activeSession.start_id}
        endId={activeSession.end_id}
        sessionTimestamp={activeSession.timestamp_ms}
        dbUniqueId={dbUniqueId}
        sectionId={activeSession.section_id}
        initialTime={getSavedPosition(activeSession.section_id)}
        showName={showName}
      />
    </div>
  );
}
