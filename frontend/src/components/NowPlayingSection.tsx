import { React } from "../../deps.ts";
const { useState } = React;
import { AudioPlayer } from "./AudioPlayer.tsx";
import { HlsUrlModal } from "./HlsUrlModal.tsx";
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
  const [showHlsModal, setShowHlsModal] = useState(false);

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
  const fullHlsUrl = window.location.origin + getHlsUrl(activeSession);

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
          className="show-hls-btn"
          onClick={() => setShowHlsModal(true)}
          title="Show HLS URL"
        >
          HLS URL
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
      {showHlsModal && (
        <HlsUrlModal
          url={fullHlsUrl}
          onClose={() => setShowHlsModal(false)}
        />
      )}
    </div>
  );
}
