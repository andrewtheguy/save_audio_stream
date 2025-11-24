import { React } from "../deps.ts";
const { useEffect, useState } = React;
import { AudioPlayer } from "./components/AudioPlayer.tsx";

interface SessionInfo {
  section_id: number;
  start_id: number;
  end_id: number;
  timestamp_ms: number;
  duration_seconds: number;
}

interface SessionsResponse {
  name: string;
  sessions: SessionInfo[];
}

function formatDuration(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = Math.floor(seconds % 60);

  if (hours > 0) {
    return `${hours}h ${minutes}m ${secs}s`;
  } else if (minutes > 0) {
    return `${minutes}m ${secs}s`;
  } else {
    return `${secs}s`;
  }
}

function formatTimestamp(timestampMs: number): string {
  const date = new Date(timestampMs);
  return date.toLocaleString();
}

function App() {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<SessionsResponse | null>(null);
  const [audioFormat, setAudioFormat] = useState<string>("opus");
  const [selectedSessionIndex, setSelectedSessionIndex] = useState<number | null>(null);
  const [dbUniqueId, setDbUniqueId] = useState<string>("");
  const [initialTime, setInitialTime] = useState<number | undefined>(undefined);
  const [savedSectionId, setSavedSectionId] = useState<number | null>(null);
  const [isReloading, setIsReloading] = useState(false);

  useEffect(() => {
    Promise.all([
      fetch("/api/format").then((r) => r.json()),
      fetch("/api/sessions").then((r) => r.json()),
    ])
      .then(([formatData, sessionsData]) => {
        setAudioFormat(formatData.format || "opus");
        setData(sessionsData);

        // Fetch metadata to get database unique_id
        return fetch(`/api/metadata`).then((r) => r.json())
          .then((metadata) => {
            setDbUniqueId(metadata.unique_id);

            // Restore last playback session from localStorage
            try {
              const storageKey = `${metadata.unique_id}_lastPlayback`;
              const stored = localStorage.getItem(storageKey);
              if (stored) {
                const { section_id, position } = JSON.parse(stored);
                // Find the session with matching section_id
                const sessionIndex = sessionsData.sessions.findIndex(
                  (s: SessionInfo) => s.section_id === section_id
                );
                if (sessionIndex !== -1) {
                  setSelectedSessionIndex(sessionIndex);
                  setInitialTime(position);
                  setSavedSectionId(section_id);
                }
              }
            } catch (err) {
              console.error("Failed to restore last playback:", err);
            }

            setLoading(false);
          });
      })
      .catch((err) => {
        console.error("Failed to load data:", err);
        setError(
          `Error loading data: ${err instanceof Error ? err.message : String(err)}`
        );
        setLoading(false);
      });
  }, []);

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const sessionsData = await fetch("/api/sessions").then((r) => r.json());
      setData(sessionsData);

      // If a session is currently selected, update it with the new data
      if (selectedSessionIndex !== null && sessionsData.sessions[selectedSessionIndex]) {
        // The AudioPlayer will automatically re-render with the new end_id from props
        // We don't need to do anything special here
      }

      setIsReloading(false);
    } catch (err) {
      console.error("Failed to reload sessions:", err);
      setError(`Failed to reload sessions: ${err instanceof Error ? err.message : String(err)}`);
      setIsReloading(false);
    }
  };

  if (loading) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div className="loading">Loading recording sessions...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div className="error">{error}</div>
      </div>
    );
  }

  if (!data) {
    return null;
  }

  return (
    <div id="app">
      <div className="app-header">
        <h1>Audio Stream Server - {data.name}</h1>
        <button
          className="reload-sessions-btn"
          onClick={handleReloadSessions}
          disabled={isReloading}
          title="Reload sessions to check for new recordings"
          aria-label="Reload sessions"
        >
          {isReloading ? "⏳ Reloading..." : "🔄 Reload Sessions"}
        </button>
      </div>

      <div className="sessions-container">
        <h2>Recording Sessions</h2>
        {data.sessions.length === 0 ? (
          <p>No recording sessions found.</p>
        ) : (
          <div className="sessions-list">
            {data.sessions.map((session, index) => {
              const isSelected = selectedSessionIndex === index;
              return (
                <div
                  key={index}
                  className={`session-card ${isSelected ? "selected" : ""}`}
                >
                  <div
                    className="session-header clickable"
                    onClick={() => setSelectedSessionIndex(isSelected ? null : index)}
                  >
                    <span className="session-time">
                      {formatTimestamp(session.timestamp_ms)}
                    </span>
                    <span className="session-duration">
                      Duration: {formatDuration(session.duration_seconds)}
                    </span>
                    <span className="expand-icon">{isSelected ? "▼" : "▶"}</span>
                  </div>
                  <div className="session-info">
                    {audioFormat === "opus" && (
                      <div className="url-row">
                        <span className="url-label">Audio:</span>
                        <a
                          href={`/audio?start_id=${session.start_id}&end_id=${session.end_id}`}
                          className="url-link"
                          onClick={(e) => e.stopPropagation()}
                        >
                          /audio?start_id={session.start_id}&end_id={session.end_id}
                        </a>
                      </div>
                    )}
                    <div className="url-row">
                      <span className="url-label">HLS:</span>
                      <a
                        href={
                          audioFormat === "aac"
                            ? `/playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`
                            : `/opus-playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`
                        }
                        className="url-link"
                        onClick={(e) => e.stopPropagation()}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {audioFormat === "aac"
                          ? `/playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`
                          : `/opus-playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`}
                      </a>
                    </div>
                  </div>
                  {isSelected && (
                    <div className="session-content">
                      <AudioPlayer
                        format={audioFormat}
                        startId={session.start_id}
                        endId={session.end_id}
                        sessionTimestamp={session.timestamp_ms}
                        dbUniqueId={dbUniqueId}
                        sectionId={session.section_id}
                        initialTime={session.section_id === savedSectionId ? initialTime : undefined}
                      />
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

export default App;
