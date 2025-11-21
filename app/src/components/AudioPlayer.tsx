import { React, dashjs, Hls } from "../../deps.ts";
const { useEffect, useRef, useState } = React;

interface AudioPlayerProps {
  format: string;
  startId: number;
  endId: number;
  sessionTimestamp: number;
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
    return `${absoluteTime.toLocaleTimeString()} ${absoluteTime.toLocaleDateString()}`;
  }
}

export function AudioPlayer({ format, startId, endId, sessionTimestamp }: AudioPlayerProps) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const dashPlayerRef = useRef<dashjs.MediaPlayerClass | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showAbsoluteTime, setShowAbsoluteTime] = useState(true);

  const streamUrl =
    format === "aac"
      ? `/playlist.m3u8?start_id=${startId}&end_id=${endId}`
      : `/manifest.mpd?start_id=${startId}&end_id=${endId}`;

  useEffect(() => {
    if (!audioRef.current) return;

    if (format === "aac") {
      // Use HLS for AAC
      if (Hls.isSupported()) {
        const hls = new Hls();
        hlsRef.current = hls;

        hls.loadSource(streamUrl);
        hls.attachMedia(audioRef.current);

        hls.on(Hls.Events.ERROR, (event, data) => {
          console.error("HLS error:", data);
          if (data.fatal) {
            setError("Failed to load HLS stream");
            setIsLoading(false);
          }
        });

        hls.on(Hls.Events.MANIFEST_PARSED, () => {
          setIsLoading(false);
        });
      } else if (audioRef.current.canPlayType("application/vnd.apple.mpegurl")) {
        // Native HLS support (Safari)
        audioRef.current.src = streamUrl;
        setIsLoading(false);
      } else {
        setError("HLS is not supported in this browser");
      }
    } else {
      // Use DASH for Opus
      const player = dashjs.MediaPlayer().create();
      dashPlayerRef.current = player;

      player.initialize(audioRef.current, streamUrl, false);

      player.on(dashjs.MediaPlayer.events.ERROR, (e: any) => {
        console.error("DASH error:", e);
        setError("Failed to load audio stream");
        setIsLoading(false);
      });

      player.on(dashjs.MediaPlayer.events.PLAYBACK_STARTED, () => {
        setIsLoading(false);
      });

      player.on(dashjs.MediaPlayer.events.PLAYBACK_WAITING, () => {
        setIsLoading(true);
      });

      player.on(dashjs.MediaPlayer.events.PLAYBACK_PLAYING, () => {
        setIsLoading(false);
      });
    }

    return () => {
      if (dashPlayerRef.current) {
        dashPlayerRef.current.destroy();
        dashPlayerRef.current = null;
      }
      if (hlsRef.current) {
        hlsRef.current.destroy();
        hlsRef.current = null;
      }
    };
  }, [format, streamUrl]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;

    const updateTime = () => setCurrentTime(audio.currentTime);
    const updateDuration = () => setDuration(audio.duration);
    const handlePlay = () => setIsPlaying(true);
    const handlePause = () => {
      setIsPlaying(false);
      setIsLoading(false);
    };
    const handleEnded = () => setIsPlaying(false);
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
  }, []);

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

  return (
    <div className="audio-player-container">
      <audio ref={audioRef} />

      {error && <div className="player-error">{error}</div>}

      <div className="player-controls">
        <button
          className="play-pause-btn"
          onClick={togglePlayPause}
          disabled={!!error}
          aria-label={isPlaying ? "Pause" : "Play"}
        >
          {isLoading ? "⏳" : isPlaying ? "⏸" : "▶"}
        </button>

        <div className="time-display">
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

        <div className="time-display">
          {showAbsoluteTime
            ? formatAbsoluteTime(sessionTimestamp, duration)
            : formatTime(duration)}
        </div>

        <button
          className="time-mode-toggle"
          onClick={() => setShowAbsoluteTime(!showAbsoluteTime)}
          title={showAbsoluteTime ? "Show relative time" : "Show absolute time"}
          aria-label={showAbsoluteTime ? "Show relative time" : "Show absolute time"}
        >
          {showAbsoluteTime ? "⏱" : "🕐"}
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
