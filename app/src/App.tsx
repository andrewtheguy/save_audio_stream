import { React } from "../deps.ts";
const { useEffect, useState, useCallback } = React;
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

interface ShowInfo {
  name: string;
  audio_format: string | null;
}

interface ShowsResponse {
  shows: ShowInfo[];
}

interface ModeResponse {
  mode: string;
}

interface SyncStatusResponse {
  in_progress: boolean;
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
  // Common state
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<"inspect" | "receiver">("inspect");

  // Receiver mode state
  const [shows, setShows] = useState<ShowInfo[]>([]);
  const [selectedShow, setSelectedShow] = useState<string | null>(null);
  const [isSyncing, setIsSyncing] = useState(false);
  const [syncStatus, setSyncStatus] = useState<boolean>(false);

  // Inspect mode / show-specific state
  const [data, setData] = useState<SessionsResponse | null>(null);
  const [audioFormat, setAudioFormat] = useState<string>("opus");
  const [selectedSessionIndex, setSelectedSessionIndex] = useState<number | null>(null);
  const [dbUniqueId, setDbUniqueId] = useState<string>("");
  const [initialTime, setInitialTime] = useState<number | undefined>(undefined);
  const [savedSectionId, setSavedSectionId] = useState<number | null>(null);
  const [isReloading, setIsReloading] = useState(false);

  // Detect mode and load initial data
  useEffect(() => {
    // Try to detect receiver mode by calling /api/mode
    fetch("/api/mode")
      .then((r) => {
        if (r.ok) return r.json();
        // If /api/mode doesn't exist, we're in inspect mode
        return { mode: "inspect" };
      })
      .then((modeData: ModeResponse) => {
        if (modeData.mode === "receiver") {
          setMode("receiver");
          // Load shows list
          return loadShows();
        } else {
          setMode("inspect");
          // Load single database data (original behavior)
          return loadInspectData();
        }
      })
      .catch((err) => {
        console.error("Failed to detect mode:", err);
        // Default to inspect mode
        setMode("inspect");
        return loadInspectData();
      });
  }, []);

  // Poll sync status in receiver mode
  useEffect(() => {
    if (mode !== "receiver") return;

    const checkSyncStatus = async () => {
      try {
        const resp = await fetch("/api/sync/status");
        if (resp.ok) {
          const status: SyncStatusResponse = await resp.json();
          setSyncStatus(status.in_progress);
        }
      } catch (err) {
        console.error("Failed to check sync status:", err);
      }
    };

    checkSyncStatus();
    const interval = setInterval(checkSyncStatus, 3000);
    return () => clearInterval(interval);
  }, [mode]);

  const loadShows = async () => {
    try {
      const resp = await fetch("/api/shows");
      const showsData: ShowsResponse = await resp.json();
      setShows(showsData.shows);
      setLoading(false);
    } catch (err) {
      console.error("Failed to load shows:", err);
      setError(`Failed to load shows: ${err instanceof Error ? err.message : String(err)}`);
      setLoading(false);
    }
  };

  const loadInspectData = async () => {
    try {
      const [formatData, sessionsData] = await Promise.all([
        fetch("/api/format").then((r) => r.json()),
        fetch("/api/sessions").then((r) => r.json()),
      ]);

      setAudioFormat(formatData.format || "opus");
      setData(sessionsData);

      // Fetch metadata to get database unique_id
      const metadata = await fetch("/api/metadata").then((r) => r.json());
      setDbUniqueId(metadata.unique_id);

      // Restore last playback session from localStorage
      try {
        const storageKey = `${metadata.unique_id}_lastPlayback`;
        const stored = localStorage.getItem(storageKey);
        if (stored) {
          const { section_id, position } = JSON.parse(stored);
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
    } catch (err) {
      console.error("Failed to load data:", err);
      setError(`Error loading data: ${err instanceof Error ? err.message : String(err)}`);
      setLoading(false);
    }
  };

  const loadShowData = async (showName: string) => {
    setLoading(true);
    setSelectedShow(showName);
    setSelectedSessionIndex(null);
    setInitialTime(undefined);
    setSavedSectionId(null);

    try {
      const [formatData, sessionsData] = await Promise.all([
        fetch(`/api/show/${showName}/format`).then((r) => r.json()),
        fetch(`/api/show/${showName}/sessions`).then((r) => r.json()),
      ]);

      setAudioFormat(formatData.format || "opus");
      setData(sessionsData);

      // Fetch metadata for unique_id
      const metadata = await fetch(`/api/show/${showName}/metadata`).then((r) => r.json());
      setDbUniqueId(metadata.unique_id);

      // Restore last playback session from localStorage
      try {
        const storageKey = `${metadata.unique_id}_lastPlayback`;
        const stored = localStorage.getItem(storageKey);
        if (stored) {
          const { section_id, position } = JSON.parse(stored);
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
    } catch (err) {
      console.error("Failed to load show data:", err);
      setError(`Failed to load show: ${err instanceof Error ? err.message : String(err)}`);
      setLoading(false);
    }
  };

  const handleBackToShows = () => {
    setSelectedShow(null);
    setData(null);
    setSelectedSessionIndex(null);
  };

  const handleTriggerSync = async () => {
    if (isSyncing || syncStatus) return;

    setIsSyncing(true);
    try {
      const resp = await fetch("/api/sync", { method: "POST" });
      if (resp.ok) {
        setSyncStatus(true);
      }
    } catch (err) {
      console.error("Failed to trigger sync:", err);
    }
    setIsSyncing(false);
  };

  const handleRefreshShows = async () => {
    if (isReloading) return;
    setIsReloading(true);
    await loadShows();
    setIsReloading(false);
  };

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const endpoint = mode === "receiver" && selectedShow
        ? `/api/show/${selectedShow}/sessions`
        : "/api/sessions";
      const sessionsData = await fetch(endpoint).then((r) => r.json());
      setData(sessionsData);
      setIsReloading(false);
    } catch (err) {
      console.error("Failed to reload sessions:", err);
      setError(`Failed to reload sessions: ${err instanceof Error ? err.message : String(err)}`);
      setIsReloading(false);
    }
  };

  // Get HLS URL for current mode
  const getHlsUrl = (session: SessionInfo): string => {
    if (mode === "receiver" && selectedShow) {
      return audioFormat === "aac"
        ? `/show/${selectedShow}/playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`
        : `/show/${selectedShow}/opus-playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`;
    }
    return audioFormat === "aac"
      ? `/playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`
      : `/opus-playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`;
  };

  if (loading) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div className="loading">Loading...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div className="error">{error}</div>
        {mode === "receiver" && selectedShow && (
          <button className="back-btn" onClick={handleBackToShows}>
            Back to Shows
          </button>
        )}
      </div>
    );
  }

  // Receiver mode: show selection screen
  if (mode === "receiver" && !selectedShow) {
    return (
      <div id="app">
        <div className="app-header">
          <h1>Audio Stream Receiver</h1>
          <div className="header-buttons">
            <button
              className="sync-btn"
              onClick={handleTriggerSync}
              disabled={isSyncing || syncStatus}
              title={syncStatus ? "Sync in progress..." : "Trigger sync from remote server"}
            >
              {syncStatus ? "Syncing..." : "Sync Now"}
            </button>
            <button
              className="reload-sessions-btn"
              onClick={handleRefreshShows}
              disabled={isReloading}
              title="Refresh shows list"
            >
              {isReloading ? "Refreshing..." : "Refresh"}
            </button>
          </div>
        </div>

        {syncStatus && (
          <div className="sync-status">
            Sync in progress...
          </div>
        )}

        <div className="shows-container">
          <h2>Available Shows</h2>
          {shows.length === 0 ? (
            <p>No shows available. Click "Sync Now" to fetch from remote server.</p>
          ) : (
            <div className="shows-list">
              {shows.map((show) => (
                <div
                  key={show.name}
                  className="show-card clickable"
                  onClick={() => loadShowData(show.name)}
                >
                  <span className="show-name">{show.name}</span>
                  {show.audio_format && (
                    <span className="show-format">{show.audio_format.toUpperCase()}</span>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    );
  }

  // Show sessions view (both inspect mode and receiver mode after show selection)
  if (!data) {
    return null;
  }

  return (
    <div id="app">
      <div className="app-header">
        <h1>
          {mode === "receiver" && selectedShow ? (
            <>
              <button className="back-btn" onClick={handleBackToShows} title="Back to shows">
                &larr;
              </button>
              {" "}{data.name}
            </>
          ) : (
            <>Audio Stream Server - {data.name}</>
          )}
        </h1>
        <div className="header-buttons">
          {mode === "receiver" && (
            <button
              className="sync-btn"
              onClick={handleTriggerSync}
              disabled={isSyncing || syncStatus}
              title={syncStatus ? "Sync in progress..." : "Trigger sync from remote server"}
            >
              {syncStatus ? "Syncing..." : "Sync Now"}
            </button>
          )}
          <button
            className="reload-sessions-btn"
            onClick={handleReloadSessions}
            disabled={isReloading}
            title="Reload sessions to check for new recordings"
            aria-label="Reload sessions"
          >
            {isReloading ? "Reloading..." : "Reload Sessions"}
          </button>
        </div>
      </div>

      {syncStatus && mode === "receiver" && (
        <div className="sync-status">
          Sync in progress...
        </div>
      )}

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
                    <div className="url-row">
                      <span className="url-label">HLS:</span>
                      <a
                        href={getHlsUrl(session)}
                        className="url-link"
                        onClick={(e) => e.stopPropagation()}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {getHlsUrl(session)}
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
                        showName={mode === "receiver" ? selectedShow : undefined}
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
