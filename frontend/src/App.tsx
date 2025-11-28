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

function formatDateWithTimeRange(startMs: number, endMs: number): string {
  const startDate = new Date(startMs);
  const endDate = new Date(endMs);
  const dateStr = startDate.toLocaleDateString();
  const startTime = startDate.toLocaleTimeString();
  const endTime = endDate.toLocaleTimeString();
  return `${dateStr} ${startTime} - ${endTime}`;
}

function formatPosition(seconds: number | undefined): string {
  if (seconds === undefined) return "Not started";
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = Math.floor(seconds % 60);
  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
  }
  return `${minutes}:${secs.toString().padStart(2, "0")}`;
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

      <div className="shows-container">
        <h2>Available Shows</h2>
        <div className="new-data-banner-container">
          {syncStatus ? (
            <div className="new-data-banner syncing">
              Sync in progress...
            </div>
          ) : (
            <div className="new-data-banner default">
              Ready
            </div>
          )}
        </div>
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
  const [activeSessionIndex, setActiveSessionIndex] = useState<number | null>(null);
  const [dbUniqueId, setDbUniqueId] = useState<string>("");
  const [isReloading, setIsReloading] = useState(false);
  const [newDataAvailable, setNewDataAvailable] = useState(false);

  // Helper to get saved position for any session
  const getSavedPosition = (sectionId: number): number | undefined => {
    if (!dbUniqueId) return undefined;
    try {
      const stored = localStorage.getItem(`${dbUniqueId}_position_${sectionId}`);
      if (stored) {
        const position = parseFloat(stored);
        return isFinite(position) ? position : undefined;
      }
    } catch (err) {
      console.error("Failed to get saved position:", err);
    }
    return undefined;
  };
  const [lastKnownEndId, setLastKnownEndId] = useState<number>(0);
  const [prevSyncStatus, setPrevSyncStatus] = useState<boolean>(false);

  // Pagination and filtering state
  const [currentPage, setCurrentPage] = useState(1);
  const [pageSize] = useState(20);
  const [dateFilter, setDateFilter] = useState<string>("");

  // Helper to build sessions URL with optional date filter
  const buildSessionsUrl = (filterDate: string) => {
    let url = `/api/show/${decodedShowName}/sessions`;
    if (filterDate) {
      // Calculate 12am local time of selected date
      const startOfDay = new Date(filterDate);
      startOfDay.setHours(0, 0, 0, 0);
      const startTs = startOfDay.getTime();
      // Calculate 12am of next day
      const endOfDay = new Date(filterDate);
      endOfDay.setDate(endOfDay.getDate() + 1);
      endOfDay.setHours(0, 0, 0, 0);
      const endTs = endOfDay.getTime();
      url += `?start_ts=${startTs}&end_ts=${endTs}`;
    }
    return url;
  };

  useEffect(() => {
    if (!decodedShowName) return;

    const loadShowData = async () => {
      setLoading(true);
      setActiveSessionIndex(null);

      try {
        const [formatData, sessionsData] = await Promise.all([
          fetch(`/api/show/${decodedShowName}/format`).then((r) => r.json()),
          fetch(buildSessionsUrl(dateFilter)).then((r) => r.json()),
        ]);

        setAudioFormat(formatData.format || "opus");
        setData(sessionsData);
        setLastKnownEndId(getMaxEndId(sessionsData.sessions));
        setNewDataAvailable(false);

        // Fetch metadata for unique_id
        const metadata = await fetch(`/api/show/${decodedShowName}/metadata`).then((r) => r.json());
        setDbUniqueId(metadata.unique_id);

        // Restore last played session for auto-select
        try {
          const lastSessionKey = `${metadata.unique_id}_lastSession`;
          const lastSessionId = localStorage.getItem(lastSessionKey);
          if (lastSessionId) {
            const sectionId = parseInt(lastSessionId, 10);
            const sessionIndex = sessionsData.sessions.findIndex(
              (s: SessionInfo) => s.section_id === sectionId
            );
            if (sessionIndex !== -1) {
              setActiveSessionIndex(sessionIndex);
            }
          }
        } catch (err) {
          console.error("Failed to restore last session:", err);
        }

        setLoading(false);
      } catch (err) {
        console.error("Failed to load show data:", err);
        setError(`Failed to load show: ${err instanceof Error ? err.message : String(err)}`);
        setLoading(false);
      }
    };

    loadShowData();
  }, [decodedShowName, dateFilter]);

  // Check for new data when sync completes
  useEffect(() => {
    // Detect transition from syncing to not syncing
    if (prevSyncStatus && !syncStatus && decodedShowName && !loading) {
      // Sync just completed, check for new data
      const checkForNewData = async () => {
        try {
          const sessionsData = await fetch(buildSessionsUrl(dateFilter)).then((r) => r.json());
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
  }, [syncStatus, prevSyncStatus, decodedShowName, lastKnownEndId, loading, dateFilter]);

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const sessionsData = await fetch(buildSessionsUrl(dateFilter)).then((r) => r.json());
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

  // Pagination calculations (server already filtered by date)
  const totalSessions = data.sessions.length;
  const totalPages = Math.ceil(totalSessions / pageSize);
  const startIndex = (currentPage - 1) * pageSize;
  const endIndex = startIndex + pageSize;
  const paginatedSessions = data.sessions.slice(startIndex, endIndex);

  const handleClearFilter = () => {
    setDateFilter("");
    setCurrentPage(1);
    setActiveSessionIndex(null);
  };

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

      <div className="sessions-container">
        <h2>Recording Sessions</h2>
        <div className="new-data-banner-container">
          {syncStatus ? (
            <div className="new-data-banner syncing">
              Sync in progress...
            </div>
          ) : newDataAvailable ? (
            <div className="new-data-banner">
              New data available. Click "Reload Sessions" to refresh.
            </div>
          ) : (
            <div className="new-data-banner default">
              Up to date
            </div>
          )}
        </div>

        {/* Filter controls */}
        <div className="filter-controls">
          <div className="filter-group">
            <label htmlFor="date-filter">Date:</label>
            <input
              type="date"
              id="date-filter"
              value={dateFilter}
              onChange={(e) => {
                setDateFilter(e.target.value);
                setCurrentPage(1);
                setActiveSessionIndex(null);
              }}
              className="date-input"
            />
            {dateFilter && (
              <button
                className="clear-filter-btn"
                onClick={handleClearFilter}
                title="Clear filter"
              >
                Clear
              </button>
            )}
          </div>
          <div className="filter-info">
            {dateFilter
              ? `${totalSessions} session${totalSessions !== 1 ? "s" : ""} on ${dateFilter}`
              : `${totalSessions} session${totalSessions !== 1 ? "s" : ""} total`}
          </div>
        </div>

        {/* Now Playing Section */}
        <div className="now-playing-section">
          {activeSessionIndex !== null && paginatedSessions[activeSessionIndex] ? (
            <>
              <div className="now-playing-info">
                <span className="now-playing-label">Now Playing:</span>
                <span className="now-playing-time">
                  {formatDateWithTimeRange(
                    paginatedSessions[activeSessionIndex].timestamp_ms,
                    paginatedSessions[activeSessionIndex].timestamp_ms + paginatedSessions[activeSessionIndex].duration_seconds * 1000
                  )}
                </span>
                <span className="now-playing-duration">
                  Duration: {formatDuration(paginatedSessions[activeSessionIndex].duration_seconds)}
                </span>
              </div>
              <AudioPlayer
                format={audioFormat}
                startId={paginatedSessions[activeSessionIndex].start_id}
                endId={paginatedSessions[activeSessionIndex].end_id}
                sessionTimestamp={paginatedSessions[activeSessionIndex].timestamp_ms}
                dbUniqueId={dbUniqueId}
                sectionId={paginatedSessions[activeSessionIndex].section_id}
                initialTime={getSavedPosition(paginatedSessions[activeSessionIndex].section_id)}
                showName={decodedShowName}
              />
            </>
          ) : (
            <div className="now-playing-placeholder">
              Select a session to play
            </div>
          )}
        </div>

        {totalSessions === 0 ? (
          <p>{dateFilter ? "No sessions found for this date." : "No recording sessions found."}</p>
        ) : (
          <div className="sessions-list">
            {paginatedSessions.map((session, index) => {
              const isActive = activeSessionIndex === index;
              const endTimestampMs = session.timestamp_ms + session.duration_seconds * 1000;
              const savedPos = getSavedPosition(session.section_id);
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
                      Position: {formatPosition(savedPos)}
                    </span>
                    {isActive ? (
                      <span className="active-badge">Active</span>
                    ) : (
                      <button
                        className="select-btn"
                        onClick={() => setActiveSessionIndex(index)}
                      >
                        Select
                      </button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}

        {/* Pagination controls */}
        {totalPages > 1 && (
          <div className="pagination-controls">
            <button
              className="pagination-btn"
              onClick={() => {
                setCurrentPage(1);
                setActiveSessionIndex(null);
              }}
              disabled={currentPage === 1}
              title="First page"
            >
              &laquo;
            </button>
            <button
              className="pagination-btn"
              onClick={() => {
                setCurrentPage((p) => Math.max(1, p - 1));
                setActiveSessionIndex(null);
              }}
              disabled={currentPage === 1}
              title="Previous page"
            >
              &lsaquo;
            </button>
            <span className="pagination-info">
              Page {currentPage} of {totalPages}
            </span>
            <button
              className="pagination-btn"
              onClick={() => {
                setCurrentPage((p) => Math.min(totalPages, p + 1));
                setActiveSessionIndex(null);
              }}
              disabled={currentPage === totalPages}
              title="Next page"
            >
              &rsaquo;
            </button>
            <button
              className="pagination-btn"
              onClick={() => {
                setCurrentPage(totalPages);
                setActiveSessionIndex(null);
              }}
              disabled={currentPage === totalPages}
              title="Last page"
            >
              &raquo;
            </button>
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
  const [activeSessionIndex, setActiveSessionIndex] = useState<number | null>(null);
  const [dbUniqueId, setDbUniqueId] = useState<string>("");
  const [isReloading, setIsReloading] = useState(false);

  // Helper to get saved position for any session
  const getSavedPosition = (sectionId: number): number | undefined => {
    if (!dbUniqueId) return undefined;
    try {
      const stored = localStorage.getItem(`${dbUniqueId}_position_${sectionId}`);
      if (stored) {
        const position = parseFloat(stored);
        return isFinite(position) ? position : undefined;
      }
    } catch (err) {
      console.error("Failed to get saved position:", err);
    }
    return undefined;
  };

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

        // Restore last played session for auto-select
        try {
          const lastSessionKey = `${metadata.unique_id}_lastSession`;
          const lastSessionId = localStorage.getItem(lastSessionKey);
          if (lastSessionId) {
            const sectionId = parseInt(lastSessionId, 10);
            const sessionIndex = sessionsData.sessions.findIndex(
              (s: SessionInfo) => s.section_id === sectionId
            );
            if (sessionIndex !== -1) {
              setActiveSessionIndex(sessionIndex);
            }
          }
        } catch (err) {
          console.error("Failed to restore last session:", err);
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

        {/* Now Playing Section */}
        <div className="now-playing-section">
          {activeSessionIndex !== null && data.sessions[activeSessionIndex] ? (
            <>
              <div className="now-playing-info">
                <span className="now-playing-label">Now Playing:</span>
                <span className="now-playing-time">
                  {formatDateWithTimeRange(
                    data.sessions[activeSessionIndex].timestamp_ms,
                    data.sessions[activeSessionIndex].timestamp_ms + data.sessions[activeSessionIndex].duration_seconds * 1000
                  )}
                </span>
                <span className="now-playing-duration">
                  Duration: {formatDuration(data.sessions[activeSessionIndex].duration_seconds)}
                </span>
              </div>
              <AudioPlayer
                format={audioFormat}
                startId={data.sessions[activeSessionIndex].start_id}
                endId={data.sessions[activeSessionIndex].end_id}
                sessionTimestamp={data.sessions[activeSessionIndex].timestamp_ms}
                dbUniqueId={dbUniqueId}
                sectionId={data.sessions[activeSessionIndex].section_id}
                initialTime={getSavedPosition(data.sessions[activeSessionIndex].section_id)}
              />
            </>
          ) : (
            <div className="now-playing-placeholder">
              Select a session to play
            </div>
          )}
        </div>

        {data.sessions.length === 0 ? (
          <p>No recording sessions found.</p>
        ) : (
          <div className="sessions-list">
            {data.sessions.map((session, index) => {
              const isActive = activeSessionIndex === index;
              const endTimestampMs = session.timestamp_ms + session.duration_seconds * 1000;
              const savedPos = getSavedPosition(session.section_id);
              return (
                <div
                  key={index}
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
                      Position: {formatPosition(savedPos)}
                    </span>
                    {isActive ? (
                      <span className="active-badge">Active</span>
                    ) : (
                      <button
                        className="select-btn"
                        onClick={() => setActiveSessionIndex(index)}
                      >
                        Select
                      </button>
                    )}
                  </div>
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
