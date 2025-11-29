import { React, Routes, Route, useParams, Link } from "../deps.ts";
const { useEffect, useState } = React;
import { SessionCard, type SessionInfo } from "./components/SessionCard.tsx";
import { NowPlayingSection } from "./components/NowPlayingSection.tsx";
import { PaginationControls } from "./components/PaginationControls.tsx";
import { DateFilterControls } from "./components/DateFilterControls.tsx";
import { useSessionNavigation } from "./hooks/useSessionNavigation.ts";

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
  const [activeSession, setActiveSession] = useState<SessionInfo | null>(null);
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
      // Parse date string as local time (YYYY-MM-DD format)
      // Note: new Date("YYYY-MM-DD") parses as UTC, so we split and construct manually
      const [year, month, day] = filterDate.split("-").map(Number);
      const startOfDay = new Date(year, month - 1, day, 0, 0, 0, 0);
      const startTs = startOfDay.getTime();
      // Calculate 12am of next day
      const endOfDay = new Date(year, month - 1, day + 1, 0, 0, 0, 0);
      const endTs = endOfDay.getTime();
      url += `?start_ts=${startTs}&end_ts=${endTs}`;
    }
    return url;
  };

  // Reset active session when show changes
  useEffect(() => {
    setActiveSession(null);
  }, [decodedShowName]);

  useEffect(() => {
    if (!decodedShowName) return;

    const loadShowData = async () => {
      setLoading(true);

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

        // Restore last played session for auto-select (only if no active session)
        if (!activeSession) {
          try {
            const lastSessionKey = `${metadata.unique_id}_lastSession`;
            const lastSessionId = localStorage.getItem(lastSessionKey);
            if (lastSessionId) {
              const sectionId = parseInt(lastSessionId, 10);
              const foundSession = sessionsData.sessions.find(
                (s: SessionInfo) => s.section_id === sectionId
              );
              if (foundSession) {
                setActiveSession(foundSession);
              }
            }
          } catch (err) {
            console.error("Failed to restore last session:", err);
          }
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

  // Use shared navigation hook (must be called before any conditional returns)
  const { handleGoToActiveSession } = useSessionNavigation({
    activeSession,
    sessions: data?.sessions || [],
    dateFilter,
    pageSize,
    setDateFilter,
    setCurrentPage,
  });

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const sessionsData = await fetch(buildSessionsUrl(dateFilter)).then((r) => r.json());
      setData(sessionsData);
      setLastKnownEndId(getMaxEndId(sessionsData.sessions));
      setNewDataAvailable(false);

      // Update activeSession with refreshed data (e.g., new end_id for pending sessions)
      if (activeSession) {
        const updatedSession = sessionsData.sessions.find(
          (s: SessionInfo) => s.section_id === activeSession.section_id
        );
        if (updatedSession) {
          setActiveSession(updatedSession);
        }
      }

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
        <DateFilterControls
          dateFilter={dateFilter}
          onFilterChange={(date) => {
            setDateFilter(date);
            setCurrentPage(1);
          }}
          onClearFilter={handleClearFilter}
          sessionCount={totalSessions}
          inputId="date-filter"
        />

        {/* Now Playing Section */}
        <NowPlayingSection
          activeSession={activeSession}
          audioFormat={audioFormat}
          dbUniqueId={dbUniqueId}
          getSavedPosition={getSavedPosition}
          getHlsUrl={getHlsUrl}
          onGoToSession={handleGoToActiveSession}
          formatDuration={formatDuration}
          formatDateWithTimeRange={formatDateWithTimeRange}
          showName={decodedShowName}
        />

        {totalSessions === 0 ? (
          <p>{dateFilter ? "No sessions found for this date." : "No recording sessions found."}</p>
        ) : (
          <div className="sessions-list">
            {paginatedSessions.map((session) => (
              <SessionCard
                key={session.section_id}
                session={session}
                isActive={activeSession?.section_id === session.section_id}
                onSelect={setActiveSession}
                getHlsUrl={getHlsUrl}
                savedPosition={getSavedPosition(session.section_id)}
                formatDuration={formatDuration}
                formatDateWithTimeRange={formatDateWithTimeRange}
                formatPosition={formatPosition}
              />
            ))}
          </div>
        )}

        {/* Pagination controls */}
        <PaginationControls
          currentPage={currentPage}
          totalPages={totalPages}
          onPageChange={setCurrentPage}
        />
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
  const [activeSession, setActiveSession] = useState<SessionInfo | null>(null);
  const [dbUniqueId, setDbUniqueId] = useState<string>("");
  const [isReloading, setIsReloading] = useState(false);

  // Pagination and filtering state
  const [currentPage, setCurrentPage] = useState(1);
  const [pageSize] = useState(20);
  const [dateFilter, setDateFilter] = useState<string>("");

  // Helper to build sessions URL with optional date filter
  const buildSessionsUrl = (filterDate: string) => {
    let url = "/api/sessions";
    if (filterDate) {
      // Parse date string as local time (YYYY-MM-DD format)
      // Note: new Date("YYYY-MM-DD") parses as UTC, so we split and construct manually
      const [year, month, day] = filterDate.split("-").map(Number);
      const startOfDay = new Date(year, month - 1, day, 0, 0, 0, 0);
      const startTs = startOfDay.getTime();
      // Calculate 12am of next day
      const endOfDay = new Date(year, month - 1, day + 1, 0, 0, 0, 0);
      const endTs = endOfDay.getTime();
      url += `?start_ts=${startTs}&end_ts=${endTs}`;
    }
    return url;
  };

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
          fetch(buildSessionsUrl(dateFilter)).then((r) => r.json()),
        ]);

        setAudioFormat(formatData.format || "opus");
        setData(sessionsData);

        // Fetch metadata to get database unique_id
        const metadata = await fetch("/api/metadata").then((r) => r.json());
        setDbUniqueId(metadata.unique_id);

        // Restore last played session for auto-select (only if no active session)
        if (!activeSession) {
          try {
            const lastSessionKey = `${metadata.unique_id}_lastSession`;
            const lastSessionId = localStorage.getItem(lastSessionKey);
            if (lastSessionId) {
              const sectionId = parseInt(lastSessionId, 10);
              const foundSession = sessionsData.sessions.find(
                (s: SessionInfo) => s.section_id === sectionId
              );
              if (foundSession) {
                setActiveSession(foundSession);
              }
            }
          } catch (err) {
            console.error("Failed to restore last session:", err);
          }
        }

        setLoading(false);
      } catch (err) {
        console.error("Failed to load data:", err);
        setError(`Error loading data: ${err instanceof Error ? err.message : String(err)}`);
        setLoading(false);
      }
    };

    loadInspectData();
  }, [dateFilter]);

  const handleReloadSessions = async () => {
    if (isReloading) return;

    setIsReloading(true);
    try {
      const sessionsData = await fetch(buildSessionsUrl(dateFilter)).then((r) => r.json());
      setData(sessionsData);

      // Update activeSession with refreshed data (e.g., new end_id for pending sessions)
      if (activeSession) {
        const updatedSession = sessionsData.sessions.find(
          (s: SessionInfo) => s.section_id === activeSession.section_id
        );
        if (updatedSession) {
          setActiveSession(updatedSession);
        }
      }

      setIsReloading(false);
    } catch (err) {
      console.error("Failed to reload sessions:", err);
      setError(`Failed to reload sessions: ${err instanceof Error ? err.message : String(err)}`);
      setIsReloading(false);
    }
  };

  const handleClearFilter = () => {
    setDateFilter("");
    setCurrentPage(1);
  };

  // Use shared navigation hook
  const { handleGoToActiveSession } = useSessionNavigation({
    activeSession,
    sessions: data?.sessions || [],
    dateFilter,
    pageSize,
    setDateFilter,
    setCurrentPage,
  });

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

        {/* Filter controls */}
        <DateFilterControls
          dateFilter={dateFilter}
          onFilterChange={(date) => {
            setDateFilter(date);
            setCurrentPage(1);
          }}
          onClearFilter={handleClearFilter}
          sessionCount={data.sessions.length}
          inputId="date-filter-inspect"
        />

        {/* Now Playing Section */}
        <NowPlayingSection
          activeSession={activeSession}
          audioFormat={audioFormat}
          dbUniqueId={dbUniqueId}
          getSavedPosition={getSavedPosition}
          getHlsUrl={getHlsUrl}
          onGoToSession={handleGoToActiveSession}
          formatDuration={formatDuration}
          formatDateWithTimeRange={formatDateWithTimeRange}
        />

        {data.sessions.length === 0 ? (
          <p>{dateFilter ? "No sessions found for this date." : "No recording sessions found."}</p>
        ) : (
          <div className="sessions-list">
            {(() => {
              const startIndex = (currentPage - 1) * pageSize;
              const endIndex = startIndex + pageSize;
              const paginatedSessions = data.sessions.slice(startIndex, endIndex);
              return paginatedSessions.map((session: SessionInfo) => (
                <SessionCard
                  key={session.section_id}
                  session={session}
                  isActive={activeSession?.section_id === session.section_id}
                  onSelect={setActiveSession}
                  getHlsUrl={getHlsUrl}
                  savedPosition={getSavedPosition(session.section_id)}
                  formatDuration={formatDuration}
                  formatDateWithTimeRange={formatDateWithTimeRange}
                  formatPosition={formatPosition}
                />
              ));
            })()}
          </div>
        )}

        {/* Pagination controls */}
        <PaginationControls
          currentPage={currentPage}
          totalPages={Math.ceil(data.sessions.length / pageSize)}
          onPageChange={setCurrentPage}
        />
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
