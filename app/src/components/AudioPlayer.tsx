import { useEffect, useRef, useState } from "react";
import dashjs from "dashjs";

interface AudioPlayerProps {
  manifestUrl: string;
  startId: number;
  endId: number;
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

export function AudioPlayer({ manifestUrl, startId, endId }: AudioPlayerProps) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const playerRef = useRef<dashjs.MediaPlayerClass | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!audioRef.current) return;

    const player = dashjs.MediaPlayer().create();
    playerRef.current = player;

    player.initialize(audioRef.current, manifestUrl, false);

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

    return () => {
      if (playerRef.current) {
        playerRef.current.destroy();
        playerRef.current = null;
      }
    };
  }, [manifestUrl]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;

    const updateTime = () => setCurrentTime(audio.currentTime);
    const updateDuration = () => setDuration(audio.duration);
    const handlePlay = () => setIsPlaying(true);
    const handlePause = () => setIsPlaying(false);
    const handleEnded = () => setIsPlaying(false);

    audio.addEventListener("timeupdate", updateTime);
    audio.addEventListener("durationchange", updateDuration);
    audio.addEventListener("loadedmetadata", updateDuration);
    audio.addEventListener("play", handlePlay);
    audio.addEventListener("pause", handlePause);
    audio.addEventListener("ended", handleEnded);

    return () => {
      audio.removeEventListener("timeupdate", updateTime);
      audio.removeEventListener("durationchange", updateDuration);
      audio.removeEventListener("loadedmetadata", updateDuration);
      audio.removeEventListener("play", handlePlay);
      audio.removeEventListener("pause", handlePause);
      audio.removeEventListener("ended", handleEnded);
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

        <div className="time-display">{formatTime(currentTime)}</div>

        <input
          type="range"
          className="progress-bar"
          min="0"
          max={duration || 0}
          value={currentTime}
          onChange={handleSeek}
          disabled={!duration || !!error}
        />

        <div className="time-display">{formatTime(duration)}</div>

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

      <div className="player-info">
        <span className="info-label">DASH:</span>
        <a
          href={manifestUrl}
          className="manifest-link"
          target="_blank"
          rel="noopener noreferrer"
        >
          /manifest.mpd?start_id={startId}&end_id={endId}
        </a>
      </div>
    </div>
  );
}
