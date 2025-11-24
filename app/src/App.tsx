import { React, Routes, Route, useParams, Link } from "../deps.ts";
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

// Shows list component for receiver mode
function ShowsList({
  shows,
  syncStatus,
  isSyncing,
  isReloading,
  onTriggerSync,
  onRefreshShows,
}: {
  shows: ShowInfo[];
  syncStatus: boolean;
  isSyncing: boolean;
  isReloading: boolean;
  onTriggerSync: () => void;
  onRefreshShows: () => void;
}) {
  return (
    <div id="app">
      <div className="app-header">
        <h1>Audio Stream Receiver</h1>
        <div className="header-buttons">
          <button
            className="sync-btn"
            onClick={onTriggerSync}
            disabled={isSyncing || syncStatus}
            title={syncStatus ? "Sync in progress..." : "Trigger sync from remote server"}
          >
            {syncStatus ? "Syncing..." : "Sync Now"}
          </button>
          <button
            className="reload-sessions-btn"
            onClick={onRefreshShows}
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
              <Link
                key={show.name}
                to={`/show/${encodeURIComponent(show.name)}`}
                className="show-card clickable"
              >
                <span className="show-name">{show.name}</span>
                {show.audio_format && (
                  <span className="show-format">{show.audio_format.toUpperCase()}</span>
                )}
              </Link>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// Helper to get max end_id from sessions
function getMaxEndId(sessions: SessionInfo[]): number {
  if (sessions.length === 0) return 0;
  return Math.max(...sessions.map((s) => s.end_id));
}

// Show detail component for receiver mode
function ShowDetail({
  syncStatus,
  isSyncing,
  onTriggerSync,
}: {
  syncStatus: boolean;
  isSyncing: boolean;
  onTriggerSync: () => void;
}) {
  const { showName } = useParams<{ showName: string }>();
  const decodedShowName = showName ? decodeURIComponent(showName) : "";

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<SessionsResponse | null>(null);
  const [audioFormat, setAudioFormat] = useState<string>("opus");
  const [selectedSessionIndex, setSelectedSessionIndex] = useState<number | null>(null);
  const [dbUniqueId, setDbUniqueId] = useState<string>("");
  const [initialTime, setInitialTime] = useState<number | undefined>(undefined);
  const [savedSectionId, setSavedSectionId] = useState<number | null>(null);
  const [isReloading, setIsReloading] = useState(false);
  const [newDataAvailable, setNewDataAvailable] = useState(false);
  const [lastKnownEndId, setLastKnownEndId] = useState<number>(0);
  const [prevSyncStatus, setPrevSyncStatus] = useState<boolean>(false);

  useEffect(() => {
    if (!decodedShowName) return;

    const loadShowData = async () => {
      setLoading(true);
      setSelectedSessionIndex(null);
      setInitialTime(undefined);
      setSavedSectionId(null);

      try {
        const [formatData, sessionsData] = await Promise.all([
          fetch(`/api/show/${decodedShowName}/format`).then((r) => r.json()),
          fetch(`/api/show/${decodedShowName}/sessions`).then((r) => r.json()),
        ]);

        setAudioFormat(formatData.format || "opus");
        setData(sessionsData);
        setLastKnownEndId(getMaxEndId(sessionsData.sessions));
        setNewDataAvailable(false);

        // Fetch metadata for unique_id
        const metadata = await fetch(`/api/show/${decodedShowName}/metadata`).then((r) => r.json());
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

    loadShowData();
  }, [decodedShowName]);

  // Check for new data when sync completes
  useEffect(() => {
    // Detect transition from syncing to not syncing
    if (prevSyncStatus && !syncStatus && decodedShowName && !loading) {
      // Sync just completed, check for new data
      const checkForNewData = async () => {
        try {
          const sessionsData = await fetch(`/api/show/${decodedShowName}/sessions`).then((r) => r.json());
          const newMaxEndId = getMaxEndId(sessionsData.sessions);
          if (newMaxEndId > lastKnownEndId) {
            setNewDataAvailable(true);
          }
        } catch (err) {
          console.error("Failed to check for new data:", err);
        }
      };
      checkForNewData();
    }
    setPrevSyncStatus(syncStatus);
  }, [syncStatus, prevSyncStatus, decodedShowName, lastKnownEndId, loading]);

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const sessionsData = await fetch(`/api/show/${decodedShowName}/sessions`).then((r) => r.json());
      setData(sessionsData);
      setLastKnownEndId(getMaxEndId(sessionsData.sessions));
      setNewDataAvailable(false);
      setIsReloading(false);
    } catch (err) {
      console.error("Failed to reload sessions:", err);
      setError(`Failed to reload sessions: ${err instanceof Error ? err.message : String(err)}`);
      setIsReloading(false);
    }
  };

  const getHlsUrl = (session: SessionInfo): string => {
    return audioFormat === "aac"
      ? `/show/${decodedShowName}/playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`
      : `/show/${decodedShowName}/opus-playlist.m3u8?start_id=${session.start_id}&end_id=${session.end_id}`;
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
        <Link to="/" className="back-btn">Back to Shows</Link>
      </div>
    );
  }

  if (!data) {
    return null;
  }

  return (
    <div id="app">
      <div className="app-header">
        <h1>
          <Link to="/" className="back-btn" title="Back to shows">
            &larr;
          </Link>
          {" "}{data.name}
        </h1>
        <div className="header-buttons">
          <button
            className="sync-btn"
            onClick={onTriggerSync}
            disabled={isSyncing || syncStatus}
            title={syncStatus ? "Sync in progress..." : "Trigger sync from remote server"}
          >
            {syncStatus ? "Syncing..." : "Sync Now"}
          </button>
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

      {syncStatus && (
        <div className="sync-status">
          Sync in progress...
        </div>
      )}

      {newDataAvailable && (
        <div className="new-data-banner" onClick={handleReloadSessions}>
          New data available. Click to reload.
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
                        showName={decodedShowName}
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

// Inspect mode component (single database)
function InspectView() {
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

    loadInspectData();
  }, []);

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const sessionsData = await fetch("/api/sessions").then((r) => r.json());
      setData(sessionsData);
      setIsReloading(false);
    } catch (err) {
      console.error("Failed to reload sessions:", err);
      setError(`Failed to reload sessions: ${err instanceof Error ? err.message : String(err)}`);
      setIsReloading(false);
    }
  };

  const getHlsUrl = (session: SessionInfo): string => {
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
        <div className="header-buttons">
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

// Main App component with routing
function App() {
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<"inspect" | "receiver">("inspect");

  // Receiver mode state
  const [shows, setShows] = useState<ShowInfo[]>([]);
  const [isSyncing, setIsSyncing] = useState(false);
  const [syncStatus, setSyncStatus] = useState<boolean>(false);
  const [isReloading, setIsReloading] = useState(false);

  // Detect mode on mount
  useEffect(() => {
    fetch("/api/mode")
      .then((r) => {
        if (r.ok) return r.json();
        return { mode: "inspect" };
      })
      .then((modeData: ModeResponse) => {
        if (modeData.mode === "receiver") {
          setMode("receiver");
          loadShows();
        } else {
          setMode("inspect");
          setLoading(false);
        }
      })
      .catch((err) => {
        console.error("Failed to detect mode:", err);
        setMode("inspect");
        setLoading(false);
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
      setLoading(false);
    }
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

  if (loading) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div className="loading">Loading...</div>
      </div>
    );
  }

  // Inspect mode - no routing needed
  if (mode === "inspect") {
    return <InspectView />;
  }

  // Receiver mode - use routing
  return (
    <Routes>
      <Route
        path="/"
        element={
          <ShowsList
            shows={shows}
            syncStatus={syncStatus}
            isSyncing={isSyncing}
            isReloading={isReloading}
            onTriggerSync={handleTriggerSync}
            onRefreshShows={handleRefreshShows}
          />
        }
      />
      <Route
        path="/show/:showName"
        element={
          <ShowDetail
            syncStatus={syncStatus}
            isSyncing={isSyncing}
            onTriggerSync={handleTriggerSync}
          />
        }
      />
    </Routes>
  );
}

export default App;
