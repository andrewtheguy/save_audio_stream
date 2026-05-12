import React, { useEffect, useMemo, useRef, useState } from "react";
import type { SessionInfo } from "./SessionCard";

interface HlsUrlModalProps {
  session: SessionInfo;
  audioFormat: string;
  showName?: string;
  formatDateWithTimeRange: (startMs: number, endMs: number) => string;
  onClose: () => void;
}

function pad(n: number): string {
  return n.toString().padStart(2, "0");
}

function toHms(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

// Combine the session-start local date with an HH:MM:SS string to produce a
// local-timezone timestamp. If the resulting timestamp falls before the session
// start, advance by 24h so a session that crosses midnight still resolves.
function hmsToTimestamp(hms: string, sessionStartMs: number): number | null {
  const m = /^(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(hms);
  if (!m) return null;
  const h = Number(m[1]);
  const mm = Number(m[2]);
  const ss = m[3] !== undefined ? Number(m[3]) : 0;
  if (h > 23 || mm > 59 || ss > 59) return null;
  const base = new Date(sessionStartMs);
  const candidate = new Date(
    base.getFullYear(),
    base.getMonth(),
    base.getDate(),
    h,
    mm,
    ss,
    0,
  ).getTime();
  if (candidate < sessionStartMs) {
    return candidate + 24 * 60 * 60 * 1000;
  }
  return candidate;
}

export function HlsUrlModal({
  session,
  audioFormat,
  showName,
  formatDateWithTimeRange,
  onClose,
}: HlsUrlModalProps) {
  const sessionStartMs = session.timestamp_ms;
  const sessionEndMs = session.timestamp_ms + session.duration_ms;

  const basePath = showName ? `/api/show/${showName}` : "/api";
  const playlistFile = audioFormat === "aac" ? "playlist.m3u8" : "opus-playlist.m3u8";

  const buildUrl = (startId: number, endId: number) =>
    `${window.location.origin}${basePath}/${playlistFile}?start_id=${startId}&end_id=${endId}`;

  const [copied, setCopied] = useState(false);
  const [narrowOpen, setNarrowOpen] = useState(false);
  const [startTime, setStartTime] = useState<string>(toHms(sessionStartMs));
  const [endTime, setEndTime] = useState<string>(toHms(sessionEndMs));
  const [startId, setStartId] = useState<number>(session.start_id);
  const [endId, setEndId] = useState<number>(session.end_id);
  const [estimating, setEstimating] = useState(false);
  const [rangeError, setRangeError] = useState<string | null>(null);

  const url = useMemo(() => buildUrl(startId, endId), [startId, endId, basePath, playlistFile]);

  const urlInputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const el = urlInputRef.current;
    if (el) el.scrollLeft = el.scrollWidth;
  }, [url]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const handleCopy = () => {
    navigator.clipboard.writeText(url);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleReset = () => {
    setStartTime(toHms(sessionStartMs));
    setEndTime(toHms(sessionEndMs));
    setStartId(session.start_id);
    setEndId(session.end_id);
    setRangeError(null);
  };

  // Debounced estimate when user edits start/end times while the narrow panel is open.
  const reqIdRef = useRef(0);
  useEffect(() => {
    if (!narrowOpen) return;

    const fullStart = toHms(sessionStartMs);
    const fullEnd = toHms(sessionEndMs);
    if (startTime === fullStart && endTime === fullEnd) {
      // Inputs match the full session — keep the original ids and skip the API.
      setStartId(session.start_id);
      setEndId(session.end_id);
      setRangeError(null);
      return;
    }

    const startMs = hmsToTimestamp(startTime, sessionStartMs);
    const endMs = hmsToTimestamp(endTime, sessionStartMs);
    if (startMs === null || endMs === null) {
      setRangeError("Invalid time format (use HH:MM:SS).");
      return;
    }
    if (startMs < sessionStartMs || endMs > sessionEndMs) {
      setRangeError("Times must be within the session range.");
      return;
    }
    if (startMs >= endMs) {
      setRangeError("End time must be after start time.");
      return;
    }

    setRangeError(null);
    const myReq = ++reqIdRef.current;
    const handle = window.setTimeout(async () => {
      setEstimating(true);
      try {
        const u = (ts: number) =>
          `${basePath}/session/${session.section_id}/estimate_segment?timestamp_ms=${ts}`;
        const [a, b] = await Promise.all([
          fetch(u(startMs)).then((r) => {
            if (!r.ok) throw new Error(`estimate_segment ${r.status}`);
            return r.json();
          }),
          fetch(u(endMs)).then((r) => {
            if (!r.ok) throw new Error(`estimate_segment ${r.status}`);
            return r.json();
          }),
        ]);
        if (myReq !== reqIdRef.current) return;
        setStartId(a.estimated_segment_id);
        setEndId(b.estimated_segment_id);
      } catch (err) {
        if (myReq !== reqIdRef.current) return;
        setRangeError(`Failed to estimate segments: ${err instanceof Error ? err.message : String(err)}`);
      } finally {
        if (myReq === reqIdRef.current) setEstimating(false);
      }
    }, 300);

    return () => window.clearTimeout(handle);
  }, [narrowOpen, startTime, endTime, sessionStartMs, sessionEndMs, basePath, session.section_id, session.start_id, session.end_id]);

  const titleText = formatDateWithTimeRange(sessionStartMs, sessionEndMs);

  return (
    <div className="modal-backdrop">
      <div className="modal-content">
        <div className="modal-header">
          <h3>{titleText}</h3>
          <button className="modal-close-btn" onClick={onClose} title="Close">
            &times;
          </button>
        </div>
        <div className="modal-body">
          <input
            ref={urlInputRef}
            type="text"
            className="hls-url-input"
            value={url}
            readOnly
            onClick={(e) => (e.target as HTMLInputElement).select()}
          />

          <button
            type="button"
            className="narrow-range-toggle"
            onClick={() => setNarrowOpen((v) => !v)}
            aria-expanded={narrowOpen}
          >
            {narrowOpen ? "▾" : "▸"} Narrow range
          </button>

          {narrowOpen && (
            <div className="narrow-range-panel">
              <div className="narrow-range-controls">
                <label className="range-time-label">
                  Start
                  <input
                    type="time"
                    step={1}
                    className="range-time-input"
                    value={startTime}
                    onChange={(e) => setStartTime(e.target.value)}
                  />
                </label>
                <label className="range-time-label">
                  End
                  <input
                    type="time"
                    step={1}
                    className="range-time-input"
                    value={endTime}
                    onChange={(e) => setEndTime(e.target.value)}
                  />
                </label>
                <button type="button" className="range-reset-btn" onClick={handleReset}>
                  Reset
                </button>
              </div>
              {rangeError ? (
                <div className="range-info range-error">{rangeError}</div>
              ) : (
                <div className="range-info">
                  {estimating ? "Estimating…" : `approx segments: ${startId}–${endId}`}
                  <span className="range-note"> Segment IDs are approximate.</span>
                </div>
              )}
            </div>
          )}
        </div>
        <div className="modal-footer">
          <button className="copy-url-btn" onClick={handleCopy}>
            {copied ? "Copied!" : "Copy URL"}
          </button>
        </div>
      </div>
    </div>
  );
}
