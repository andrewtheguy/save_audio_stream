import { React, Hls } from "../../deps.ts";
const { useEffect, useRef, useState, useMemo } = React;

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

// Time mode enum
type TimeMode = "relative" | "absolute" | "hour";

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
  return `${absoluteTime.toLocaleDateString()}, ${absoluteTime.toLocaleTimeString()}`;
}

function formatAbsoluteTimeOnly(timestampMs: number, offsetSeconds: number): string {
  if (!isFinite(offsetSeconds)) return "--:--:--";
  const absoluteTime = new Date(timestampMs + offsetSeconds * 1000);
  return absoluteTime.toLocaleTimeString();
}

// Format seconds within an hour as MM:SS.ss
function formatHourTime(secondsInHour: number): string {
  if (!isFinite(secondsInHour)) return "--:--.--";
  const minutes = Math.floor(secondsInHour / 60);
  const secs = secondsInHour % 60;
  return `${minutes.toString().padStart(2, "0")}:${secs.toFixed(2).padStart(5, "0")}`;
}

// Format timestamp as time only for hour view markers
function formatTimestampTimeOnly(timestampMs: number): string {
  if (!isFinite(timestampMs)) return "--:--:--";
  const date = new Date(timestampMs);
  return date.toLocaleTimeString();
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
  // Only disable controls on fatal errors, not during retries
  const isFatalError = error !== null && !error.includes("retrying");
  const [timeMode, setTimeMode] = useState<TimeMode>("hour");
  const [selectedHourIndex, setSelectedHourIndex] = useState(0);
  const hourInitializedRef = useRef(false);

  // Hour view computed values
  const hourViewData = useMemo(() => {
    const HOUR_SECONDS = 3600;

    // Calculate session start position within its starting hour (based on absolute time)
    const sessionStartDate = new Date(sessionTimestamp);
    const sessionStartMinutes = sessionStartDate.getMinutes();
    const sessionStartSeconds = sessionStartDate.getSeconds();
    const sessionStartInHour = sessionStartMinutes * 60 + sessionStartSeconds; // seconds into the hour when session started

    // Session end position within its ending hour
    const sessionEndDate = new Date(sessionTimestamp + duration * 1000);
    const sessionEndMinutes = sessionEndDate.getMinutes();
    const sessionEndSeconds = sessionEndDate.getSeconds();
    const sessionEndInHour = sessionEndMinutes * 60 + sessionEndSeconds;

    // Calculate which absolute hours the session spans
    const startHour = Math.floor(sessionTimestamp / (HOUR_SECONDS * 1000));
    const endHour = Math.floor((sessionTimestamp + duration * 1000) / (HOUR_SECONDS * 1000));
    const totalHours = endHour - startHour + 1;

    // Which hour the current playback position is in (0-indexed relative to session)
    const currentAbsoluteTime = sessionTimestamp + currentTime * 1000;
    const currentAbsoluteHour = Math.floor(currentAbsoluteTime / (HOUR_SECONDS * 1000));
    const currentHourIndex = currentAbsoluteHour - startHour;

    // Calculate available time within the selected hour
    // selectedHourIndex is relative to the session (0 = first hour of session)
    const selectedAbsoluteHour = startHour + selectedHourIndex;
    const isFirstHour = selectedHourIndex === 0;
    const isLastHour = selectedHourIndex === totalHours - 1;

    // Start of available time in this hour (0-3599)
    const availableStartInHour = isFirstHour ? sessionStartInHour : 0;

    // End of available time in this hour (0-3600)
    const availableEndInHour = isLastHour ? sessionEndInHour : HOUR_SECONDS;

    // Current position within the clock hour (0-3599)
    const currentDate = new Date(sessionTimestamp + currentTime * 1000);
    const currentMinutesInHour = currentDate.getMinutes();
    const currentSecondsInHour = currentDate.getSeconds() + currentDate.getMilliseconds() / 1000;
    const currentTimeInHour = currentMinutesInHour * 60 + currentSecondsInHour;

    // Is current playback position within the selected hour view?
    const isPlaybackInSelectedHour = currentHourIndex === selectedHourIndex;

    // For seeking: convert hour position to audio currentTime
    // hourStartOffset = seconds from audio start to the beginning of this hour window
    const hourStartOffset = isFirstHour
      ? 0
      : (selectedAbsoluteHour * HOUR_SECONDS * 1000 - sessionTimestamp) / 1000;

    // Calculate absolute timestamps for markers
    const hourBoundaryMs = selectedAbsoluteHour * HOUR_SECONDS * 1000;
    const availableStartTimestamp = hourBoundaryMs + availableStartInHour * 1000;
    const availableEndTimestamp = hourBoundaryMs + availableEndInHour * 1000;

    return {
      totalHours: Math.max(1, totalHours),
      currentHourIndex,
      hourStartOffset,
      availableStartInHour,
      availableEndInHour,
      currentTimeInHour: isPlaybackInSelectedHour ? currentTimeInHour : (selectedHourIndex < currentHourIndex ? availableEndInHour : availableStartInHour),
      isPlaybackInSelectedHour,
      HOUR_SECONDS,
      sessionStartInHour,
      availableStartTimestamp,
      availableEndTimestamp,
    };
  }, [duration, currentTime, selectedHourIndex, sessionTimestamp]);

  // Reset hour initialization when session changes
  useEffect(() => {
    hourInitializedRef.current = false;
    setSelectedHourIndex(0);
  }, [sectionId]);

  // Initialize selectedHourIndex to the hour containing the initial/current position
  useEffect(() => {
    if (!hourInitializedRef.current && duration > 0) {
      // Use initialTime if provided, otherwise use currentTime
      const targetTime = (initialTime !== undefined && currentTime === 0) ? initialTime : currentTime;

      // Calculate which hour the target position is in
      const HOUR_SECONDS = 3600;
      const targetAbsoluteTime = sessionTimestamp + targetTime * 1000;
      const targetAbsoluteHour = Math.floor(targetAbsoluteTime / (HOUR_SECONDS * 1000));
      const startHour = Math.floor(sessionTimestamp / (HOUR_SECONDS * 1000));
      const targetHourIndex = Math.max(0, targetAbsoluteHour - startHour);

      setSelectedHourIndex(targetHourIndex);
      hourInitializedRef.current = true;
    }
  }, [duration, initialTime, currentTime, sessionTimestamp]);

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

      hls.on(Hls.Events.ERROR, (_event: unknown, data: { fatal: boolean; type: string }) => {
        console.error("HLS error:", data);
        if (data.fatal) {
          const maxRetries = 5;

          // Retry for network errors or media errors (both can be temporary)
          if (data.type === Hls.ErrorTypes.NETWORK_ERROR || data.type === Hls.ErrorTypes.MEDIA_ERROR) {
            retryCountRef.current += 1;

            if (retryCountRef.current <= maxRetries) {
              const retryDelay = Math.min(1000 * Math.pow(2, retryCountRef.current - 1), 10000);
              console.log(`HLS error (${data.type}), retrying in ${retryDelay}ms (attempt ${retryCountRef.current}/${maxRetries})`);
              setError(`Connection error, retrying... (${retryCountRef.current}/${maxRetries})`);

              setTimeout(() => {
                if (hlsRef.current) {
                  if (data.type === Hls.ErrorTypes.MEDIA_ERROR) {
                    hlsRef.current.recoverMediaError();
                  } else {
                    hlsRef.current.startLoad();
                  }
                }
              }, retryDelay);
            } else {
              // Max retries reached - show error and reset to stopped state
              setError("Connection lost. Please try again.");
              setIsLoading(false);
              setIsPlaying(false);
            }
          } else {
            // Other fatal error - don't retry
            setError("Failed to load stream");
            setIsLoading(false);
            setIsPlaying(false);
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
    const handlePlaying = () => {
      setIsLoading(false);
      // Clear any retry error when playback successfully resumes
      setError(null);
      retryCountRef.current = 0;
    };
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

  // Cycle through time modes: absolute -> hour -> absolute (relative disabled for now)
  const cycleTimeMode = () => {
    setTimeMode((prev) => {
      // When entering hour mode, set to current hour
      if (prev === "absolute") {
        setSelectedHourIndex(hourViewData.currentHourIndex);
        return "hour";
      }
      return "absolute";
    });
  };

  // Hour navigation
  const goToPrevHour = () => {
    if (selectedHourIndex > 0) {
      setSelectedHourIndex(selectedHourIndex - 1);
    }
  };

  const goToNextHour = () => {
    if (selectedHourIndex < hourViewData.totalHours - 1) {
      setSelectedHourIndex(selectedHourIndex + 1);
    }
  };

  const resetToCurrentHour = () => {
    setSelectedHourIndex(hourViewData.currentHourIndex);
  };

  // Handle seek in hour mode
  const handleHourSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (!audioRef.current) return;
    const timeInHour = parseFloat(e.target.value);
    // Convert hour position to audio currentTime
    // For first hour: audioTime = timeInHour - sessionStartInHour
    // For other hours: audioTime = hourStartOffset + timeInHour
    const audioTime = hourViewData.hourStartOffset + (timeInHour - (hourViewData.hourStartOffset === 0 ? hourViewData.sessionStartInHour : 0));
    const clampedTime = Math.max(0, Math.min(audioTime, duration));
    audioRef.current.currentTime = clampedTime;
    setCurrentTime(clampedTime);
  };

  // Auto-switch to next hour only when playback is within the selected hour view
  const prevHourIndexRef = useRef(hourViewData.currentHourIndex);
  useEffect(() => {
    if (timeMode !== "hour") return;
    if (!isPlaying) return;

    // Only auto-switch if we were previously in range (thumb was visible)
    // and playback just moved to a different hour
    if (prevHourIndexRef.current === selectedHourIndex &&
        hourViewData.currentHourIndex !== selectedHourIndex &&
        hourViewData.currentHourIndex >= 0 &&
        hourViewData.currentHourIndex < hourViewData.totalHours) {
      setSelectedHourIndex(hourViewData.currentHourIndex);
    }
    prevHourIndexRef.current = hourViewData.currentHourIndex;
  }, [currentTime, timeMode, isPlaying, selectedHourIndex, hourViewData.currentHourIndex, hourViewData.totalHours]);

  return (
    <div className="audio-player-container">
      <audio ref={audioRef} />

      {error && <div className="player-error">{error}</div>}

      {/* Progress section at top */}
      <div className="progress-section">
        <div className="current-time-display">
          <div className="current-date">{new Date(sessionTimestamp + currentTime * 1000).toLocaleDateString()}</div>
          <div className="current-time">{new Date(sessionTimestamp + currentTime * 1000).toLocaleTimeString()}</div>
        </div>

        {timeMode === "hour" ? (
          <div className="hour-slider-row">
            <button
              className="hour-nav-btn"
              onClick={goToPrevHour}
              disabled={selectedHourIndex === 0}
              aria-label="Previous hour"
              title="Previous hour"
            >
              ◀
            </button>
            <div className="hour-slider-container">
              <input
                type="range"
                className={`progress-bar ${!hourViewData.isPlaybackInSelectedHour ? 'out-of-range' : ''}`}
                min={hourViewData.availableStartInHour}
                max={hourViewData.availableEndInHour}
                value={hourViewData.isPlaybackInSelectedHour
                  ? Math.max(hourViewData.availableStartInHour, Math.min(hourViewData.currentTimeInHour, hourViewData.availableEndInHour))
                  : hourViewData.availableStartInHour}
                onChange={handleHourSeek}
                disabled={!duration || !!error || hourViewData.availableEndInHour <= hourViewData.availableStartInHour}
              />
              <div className="slider-ticks with-quarters">
                <span className="tick"></span>
                <span className="tick"></span>
                <span className="tick"></span>
                <span className="tick"></span>
                <span className="tick"></span>
              </div>
              <div className="time-markers">
                <span className="time-marker">{formatTimestampTimeOnly(hourViewData.availableStartTimestamp)}</span>
                <span className="time-marker">{formatTimestampTimeOnly(hourViewData.availableStartTimestamp + (hourViewData.availableEndTimestamp - hourViewData.availableStartTimestamp) * 0.25)}</span>
                <span className="time-marker">{formatTimestampTimeOnly(hourViewData.availableStartTimestamp + (hourViewData.availableEndTimestamp - hourViewData.availableStartTimestamp) * 0.5)}</span>
                <span className="time-marker">{formatTimestampTimeOnly(hourViewData.availableStartTimestamp + (hourViewData.availableEndTimestamp - hourViewData.availableStartTimestamp) * 0.75)}</span>
                <span className="time-marker">{formatTimestampTimeOnly(hourViewData.availableEndTimestamp)}</span>
              </div>
            </div>
            <button
              className="hour-nav-btn"
              onClick={goToNextHour}
              disabled={selectedHourIndex >= hourViewData.totalHours - 1}
              aria-label="Next hour"
              title="Next hour"
            >
              ▶
            </button>
          </div>
        ) : (
          <>
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
                {timeMode === "absolute"
                  ? formatAbsoluteTimeOnly(sessionTimestamp, 0)
                  : formatTime(0)}
              </span>
              {duration >= 120 && (
                <>
                  <span className="time-marker">
                    {timeMode === "absolute"
                      ? formatAbsoluteTimeOnly(sessionTimestamp, duration * 0.25)
                      : formatTime(duration * 0.25)}
                  </span>
                  <span className="time-marker">
                    {timeMode === "absolute"
                      ? formatAbsoluteTimeOnly(sessionTimestamp, duration * 0.5)
                      : formatTime(duration * 0.5)}
                  </span>
                  <span className="time-marker">
                    {timeMode === "absolute"
                      ? formatAbsoluteTimeOnly(sessionTimestamp, duration * 0.75)
                      : formatTime(duration * 0.75)}
                  </span>
                </>
              )}
              <span className="time-marker">
                {timeMode === "absolute"
                  ? formatAbsoluteTimeOnly(sessionTimestamp, duration)
                  : formatTime(duration)}
              </span>
            </div>
          </>
        )}
      </div>

      {/* Controls row */}
      <div className="player-controls">
        {timeMode === "hour" ? (
          <button
            className="reset-hour-btn"
            onClick={resetToCurrentHour}
            disabled={selectedHourIndex === hourViewData.currentHourIndex}
            title="Reset to current playback hour"
            aria-label="Reset to current playback hour"
          >
            ⟲
          </button>
        ) : (
          <div className="reset-hour-spacer"></div>
        )}

        <button
          className="time-mode-toggle"
          onClick={cycleTimeMode}
          title={timeMode === "absolute" ? "Switch to hour view" : "Switch to absolute time"}
          aria-label="Toggle time mode"
        >
          {timeMode === "absolute" ? "⏱" : "⏰"}
        </button>

        <button
          className="skip-btn"
          onClick={skipBackward}
          disabled={isFatalError}
          aria-label="Rewind 15 seconds"
          title="Rewind 15 seconds"
        >
          -15s
        </button>

        <button
          className="play-pause-btn"
          onClick={togglePlayPause}
          disabled={isFatalError}
          aria-label={isPlaying ? "Pause" : "Play"}
        >
          {isLoading ? "⏳" : isPlaying ? "⏸" : "▶"}
        </button>

        <button
          className="skip-btn"
          onClick={skipForward}
          disabled={isFatalError}
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
