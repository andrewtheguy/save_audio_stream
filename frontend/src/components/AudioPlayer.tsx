import { React, Hls } from "../../deps.ts";
const { useEffect, useRef, useState } = React;

interface AudioPlayerProps {
  format: string;
  startId: number;
  endId: number;
  sessionTimestamp: number;
  dbUniqueId: string;
  sectionId: number;
  initialTime?: number;
  showName?: string | null;
}

function formatTime(seconds: number): string {
  if (!isFinite(seconds)) return "--:--";
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = Math.floor(seconds % 60);

  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
  }
  return `${minutes}:${secs.toString().padStart(2, "0")}`;
}

function formatAbsoluteTime(timestampMs: number, offsetSeconds: number): string {
  if (!isFinite(offsetSeconds)) return "--:--:--";
  const absoluteTime = new Date(timestampMs + offsetSeconds * 1000);
  const now = new Date();

  // Check if the absolute time is today
  const isToday = absoluteTime.getDate() === now.getDate() &&
                  absoluteTime.getMonth() === now.getMonth() &&
                  absoluteTime.getFullYear() === now.getFullYear();

  if (isToday) {
    return absoluteTime.toLocaleTimeString();
  } else {
    return `${absoluteTime.toLocaleDateString()} ${absoluteTime.toLocaleTimeString()}`;
  }
}

function formatAbsoluteTimeOnly(timestampMs: number, offsetSeconds: number): string {
  if (!isFinite(offsetSeconds)) return "--:--:--";
  const absoluteTime = new Date(timestampMs + offsetSeconds * 1000);
  return absoluteTime.toLocaleTimeString();
}

export function AudioPlayer({ format, startId, endId, sessionTimestamp, dbUniqueId, sectionId, initialTime, showName }: AudioPlayerProps) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const hlsRef = useRef<Hls | null>(null);
  const saveTimerRef = useRef<number | null>(null);
  const retryCountRef = useRef<number>(0);
  const savedPositionRef = useRef<number | null>(null);
  const wasPlayingRef = useRef<boolean>(false);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showAbsoluteTime, setShowAbsoluteTime] = useState(true);

  // Save playback position to localStorage (per-session)
  const savePlaybackPosition = (position: number) => {
    try {
      // Save position for this specific session
      const positionKey = `${dbUniqueId}_position_${sectionId}`;
      localStorage.setItem(positionKey, position.toString());

      // Also track this as the last played session
      const lastSessionKey = `${dbUniqueId}_lastSession`;
      localStorage.setItem(lastSessionKey, sectionId.toString());
    } catch (err) {
      console.error("Failed to save playback position:", err);
    }
  };

  // Construct stream URL based on whether we're in receiver mode (showName provided) or inspect mode
  const basePath = showName ? `/show/${showName}` : "";
  const streamUrl =
    format === "aac"
      ? `${basePath}/playlist.m3u8?start_id=${startId}&end_id=${endId}`
      : `${basePath}/opus-playlist.m3u8?start_id=${startId}&end_id=${endId}`;

  useEffect(() => {
    if (!audioRef.current) return;

    // Reset retry count when loading new stream
    retryCountRef.current = 0;

    // Use HLS for all formats (both AAC and Opus)
    if (Hls.isSupported()) {
      const hls = new Hls();
      hlsRef.current = hls;

      hls.loadSource(streamUrl);
      hls.attachMedia(audioRef.current);

      hls.on(Hls.Events.ERROR, (event, data) => {
        console.error("HLS error:", data);
        if (data.fatal) {
          // Check if it's a network error (temporary/recoverable)
          if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {
            // Retry with exponential backoff
            retryCountRef.current += 1;
            const maxRetries = 5;

            if (retryCountRef.current <= maxRetries) {
              const retryDelay = Math.min(1000 * Math.pow(2, retryCountRef.current - 1), 10000);
              console.log(`Network error, retrying in ${retryDelay}ms (attempt ${retryCountRef.current}/${maxRetries})`);
              setError(`Connection error, retrying... (${retryCountRef.current}/${maxRetries})`);

              setTimeout(() => {
                if (hlsRef.current) {
                  hlsRef.current.startLoad();
                }
              }, retryDelay);
            } else {
              // Max retries reached
              setError("Failed to load HLS stream after multiple retries");
              setIsLoading(false);
              setIsPlaying(false);
              if (audioRef.current) {
                audioRef.current.pause();
              }
            }
          } else {
            // Media error or other fatal error - don't retry
            setError("Failed to load HLS stream");
            setIsLoading(false);
            setIsPlaying(false);
            if (audioRef.current) {
              audioRef.current.pause();
            }
          }
        }
      });

      hls.on(Hls.Events.MANIFEST_PARSED, () => {
        setIsLoading(false);
        // Reset retry count and clear error on successful load
        retryCountRef.current = 0;
        setError(null);

        // Restore position: prioritize saved position (from reload) over initialTime (from mount)
        if (savedPositionRef.current !== null && audioRef.current) {
          console.log(`Restoring position after reload: ${savedPositionRef.current}, wasPlaying: ${wasPlayingRef.current}`);
          audioRef.current.currentTime = savedPositionRef.current;

          // Restore play state
          if (wasPlayingRef.current) {
            audioRef.current.play().catch((err) => {
              console.error("Failed to resume playback after reload:", err);
              setError("Failed to resume playback");
            });
          }

          // Clear saved state
          savedPositionRef.current = null;
          wasPlayingRef.current = false;
        } else if (initialTime !== undefined && audioRef.current) {
          // Initial mount: restore from localStorage
          audioRef.current.currentTime = initialTime;
        }
      });
    } else if (audioRef.current.canPlayType("application/vnd.apple.mpegurl")) {
      // Native HLS support (Safari)
      audioRef.current.src = streamUrl;
      // Restore playback position after metadata loads
      const handleLoadedMetadata = () => {
        if (!audioRef.current) return;

        // Restore position: prioritize saved position (from reload) over initialTime (from mount)
        if (savedPositionRef.current !== null) {
          console.log(`Restoring position after reload (Safari): ${savedPositionRef.current}, wasPlaying: ${wasPlayingRef.current}`);
          audioRef.current.currentTime = savedPositionRef.current;

          // Restore play state
          if (wasPlayingRef.current) {
            audioRef.current.play().catch((err) => {
              console.error("Failed to resume playback after reload:", err);
              setError("Failed to resume playback");
            });
          }

          // Clear saved state
          savedPositionRef.current = null;
          wasPlayingRef.current = false;
        } else if (initialTime !== undefined) {
          // Initial mount: restore from localStorage
          audioRef.current.currentTime = initialTime;
        }
      };
      audioRef.current.addEventListener("loadedmetadata", handleLoadedMetadata);
      setIsLoading(false);
    } else {
      setError("HLS is not supported in this browser");
    }

    return () => {
      // Save current state before cleanup (for reload scenario)
      // Only save if we're actually playing something (not initial mount)
      if (audioRef.current && audioRef.current.currentTime > 0) {
        savedPositionRef.current = audioRef.current.currentTime;
        wasPlayingRef.current = !audioRef.current.paused;
        console.log(`Cleanup: saving position ${savedPositionRef.current}, wasPlaying: ${wasPlayingRef.current}`);
      }

      if (hlsRef.current) {
        hlsRef.current.destroy();
        hlsRef.current = null;
      }
    };
  }, [format, streamUrl, showName]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;

    const updateTime = () => setCurrentTime(audio.currentTime);
    const updateDuration = () => setDuration(audio.duration);
    const handlePlay = () => {
      setIsPlaying(true);
      // Start periodic save interval (every 5 seconds)
      if (saveTimerRef.current !== null) {
        clearInterval(saveTimerRef.current);
      }
      saveTimerRef.current = window.setInterval(() => {
        if (audio && !audio.paused) {
          savePlaybackPosition(audio.currentTime);
        }
      }, 5000);
    };
    const handlePause = () => {
      setIsPlaying(false);
      setIsLoading(false);
      // Stop periodic save and save once immediately
      if (saveTimerRef.current !== null) {
        clearInterval(saveTimerRef.current);
        saveTimerRef.current = null;
      }
      savePlaybackPosition(audio.currentTime);
    };
    const handleEnded = () => {
      setIsPlaying(false);
      // Stop periodic save on end
      if (saveTimerRef.current !== null) {
        clearInterval(saveTimerRef.current);
        saveTimerRef.current = null;
      }
    };
    const handleWaiting = () => setIsLoading(true);
    const handlePlaying = () => setIsLoading(false);
    const handleCanPlay = () => setIsLoading(false);

    audio.addEventListener("timeupdate", updateTime);
    audio.addEventListener("durationchange", updateDuration);
    audio.addEventListener("loadedmetadata", updateDuration);
    audio.addEventListener("play", handlePlay);
    audio.addEventListener("pause", handlePause);
    audio.addEventListener("ended", handleEnded);
    audio.addEventListener("waiting", handleWaiting);
    audio.addEventListener("playing", handlePlaying);
    audio.addEventListener("canplay", handleCanPlay);

    return () => {
      // Stop periodic save and save position on unmount
      if (saveTimerRef.current !== null) {
        clearInterval(saveTimerRef.current);
        saveTimerRef.current = null;
      }
      // Only save if we have a valid position (avoid overwriting with 0)
      if (audio.currentTime > 0) {
        savePlaybackPosition(audio.currentTime);
      }

      audio.removeEventListener("timeupdate", updateTime);
      audio.removeEventListener("durationchange", updateDuration);
      audio.removeEventListener("loadedmetadata", updateDuration);
      audio.removeEventListener("play", handlePlay);
      audio.removeEventListener("pause", handlePause);
      audio.removeEventListener("ended", handleEnded);
      audio.removeEventListener("waiting", handleWaiting);
      audio.removeEventListener("playing", handlePlaying);
      audio.removeEventListener("canplay", handleCanPlay);
    };
  }, [dbUniqueId, sectionId]);

  const togglePlayPause = () => {
    if (!audioRef.current) return;

    if (isPlaying) {
      audioRef.current.pause();
    } else {
      setIsLoading(true);
      audioRef.current.play().catch((err) => {
        console.error("Play error:", err);
        setError("Failed to play audio");
        setIsLoading(false);
        setIsPlaying(false);
      });
    }
  };

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (!audioRef.current) return;
    const time = parseFloat(e.target.value);
    audioRef.current.currentTime = time;
    setCurrentTime(time);
  };

  const handleVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (!audioRef.current) return;
    const vol = parseFloat(e.target.value);
    audioRef.current.volume = vol;
    setVolume(vol);
  };

  const skipBackward = () => {
    if (!audioRef.current) return;
    audioRef.current.currentTime = Math.max(0, audioRef.current.currentTime - 15);
  };

  const skipForward = () => {
    if (!audioRef.current) return;
    audioRef.current.currentTime = Math.min(duration, audioRef.current.currentTime + 30);
  };

  return (
    <div className="audio-player-container">
      <audio ref={audioRef} />

      {error && <div className="player-error">{error}</div>}

      {/* Progress section at top */}
      <div className="progress-section">
        <div className="current-time-display">
          {showAbsoluteTime
            ? formatAbsoluteTime(sessionTimestamp, currentTime)
            : formatTime(currentTime)}
        </div>
        <input
          type="range"
          className="progress-bar"
          min="0"
          max={duration || 0}
          value={currentTime}
          onChange={handleSeek}
          disabled={!duration || !!error}
        />
        <div className={`slider-ticks ${duration >= 120 ? 'with-quarters' : ''}`}>
          <span className="tick"></span>
          {duration >= 120 && (
            <>
              <span className="tick"></span>
              <span className="tick"></span>
              <span className="tick"></span>
            </>
          )}
          <span className="tick"></span>
        </div>
        <div className="time-markers">
          <span className="time-marker">
            {showAbsoluteTime
              ? formatAbsoluteTimeOnly(sessionTimestamp, 0)
              : formatTime(0)}
          </span>
          {duration >= 120 && (
            <>
              <span className="time-marker">
                {showAbsoluteTime
                  ? formatAbsoluteTimeOnly(sessionTimestamp, duration * 0.25)
                  : formatTime(duration * 0.25)}
              </span>
              <span className="time-marker">
                {showAbsoluteTime
                  ? formatAbsoluteTimeOnly(sessionTimestamp, duration * 0.5)
                  : formatTime(duration * 0.5)}
              </span>
              <span className="time-marker">
                {showAbsoluteTime
                  ? formatAbsoluteTimeOnly(sessionTimestamp, duration * 0.75)
                  : formatTime(duration * 0.75)}
              </span>
            </>
          )}
          <span className="time-marker">
            {showAbsoluteTime
              ? formatAbsoluteTimeOnly(sessionTimestamp, duration)
              : formatTime(duration)}
          </span>
        </div>
      </div>

      {/* Controls row */}
      <div className="player-controls">
        <button
          className="time-mode-toggle"
          onClick={() => setShowAbsoluteTime(!showAbsoluteTime)}
          title={showAbsoluteTime ? "Show relative time" : "Show absolute time"}
          aria-label={showAbsoluteTime ? "Show relative time" : "Show absolute time"}
        >
          {showAbsoluteTime ? "⏱" : "🕐"}
        </button>

        <button
          className="skip-btn"
          onClick={skipBackward}
          disabled={!!error}
          aria-label="Rewind 15 seconds"
          title="Rewind 15 seconds"
        >
          -15s
        </button>

        <button
          className="play-pause-btn"
          onClick={togglePlayPause}
          disabled={!!error}
          aria-label={isPlaying ? "Pause" : "Play"}
        >
          {isLoading ? "⏳" : isPlaying ? "⏸" : "▶"}
        </button>

        <button
          className="skip-btn"
          onClick={skipForward}
          disabled={!!error}
          aria-label="Forward 30 seconds"
          title="Forward 30 seconds"
        >
          +30s
        </button>

        <div className="volume-control">
          <span className="volume-icon">🔊</span>
          <input
            type="range"
            className="volume-slider"
            min="0"
            max="1"
            step="0.1"
            value={volume}
            onChange={handleVolumeChange}
          />
        </div>
      </div>
    </div>
  );
}
