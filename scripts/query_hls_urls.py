#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "requests",
# ]
# ///
"""
Query HLS playlist URLs for a show by date and approximate time range.

Usage:
    uv run scripts/query_hls_urls.py --date 2025-12-02 --start 19:00 --end 20:00
    uv run scripts/query_hls_urls.py --date 2025-12-02 --start 19:00 --end 20:00 --show am1430
"""

import argparse
import json
import sys
from datetime import datetime

import requests

DEFAULT_SERVER = "https://saveaudio.local.168234.xyz"
DEFAULT_SHOW = "am1430"


def parse_args():
    parser = argparse.ArgumentParser(description="Query HLS URLs for a date and time range")
    parser.add_argument("--server", default=DEFAULT_SERVER, help=f"Server URL (default: {DEFAULT_SERVER})")
    parser.add_argument("--show", default=DEFAULT_SHOW, help=f"Show name (default: {DEFAULT_SHOW})")
    parser.add_argument("--date", required=True, help="Date in YYYY-MM-DD format (local time)")
    parser.add_argument("--start", required=True, help="Start time in HH:MM format (local time)")
    parser.add_argument("--end", required=True, help="End time in HH:MM format (local time)")
    parser.add_argument("--json", action="store_true", help="Output as JSON")
    parser.add_argument("--verbose", "-v", action="store_true", help="Print API requests/responses")
    return parser.parse_args()


def api_get(url: str, verbose: bool = False) -> dict:
    if verbose:
        print(f"  -> GET {url}", file=sys.stderr)
    resp = requests.get(url)
    resp.raise_for_status()
    data = resp.json()
    if verbose:
        print(f"  <- {resp.status_code}", file=sys.stderr)
        print(json.dumps(data, indent=2), file=sys.stderr)
    return data


def to_timestamp_ms(date_str: str, time_str: str) -> int:
    dt_naive = datetime.strptime(f"{date_str} {time_str}", "%Y-%m-%d %H:%M")
    return int(dt_naive.astimezone().timestamp() * 1000)


def fmt_ts(ms: int) -> str:
    return datetime.fromtimestamp(ms / 1000).strftime("%Y-%m-%d %H:%M:%S")


def find_overlapping_sessions(sessions: list, start_ms: int, end_ms: int) -> list:
    """Return sessions that overlap with [start_ms, end_ms), sorted by start time."""
    overlapping = []
    for s in sessions:
        s_start = s["timestamp_ms"]
        s_end = s_start + s["duration_ms"]
        if s_start < end_ms and s_end > start_ms:
            overlapping.append(s)
    overlapping.sort(key=lambda s: s["timestamp_ms"])
    return overlapping


def estimate_segment(server: str, show: str, section_id, ts_ms: int, verbose: bool) -> int:
    url = f"{server}/api/show/{show}/session/{section_id}/estimate_segment?timestamp_ms={ts_ms}"
    return api_get(url, verbose)["estimated_segment_id"]


def main():
    args = parse_args()

    start_ms = to_timestamp_ms(args.date, args.start)
    end_ms = to_timestamp_ms(args.date, args.end)
    if end_ms <= start_ms:
        print("Error: end time must be after start time", file=sys.stderr)
        sys.exit(2)

    sessions = api_get(f"{args.server}/api/show/{args.show}/sessions", args.verbose).get("sessions", [])
    audio_format = (
        api_get(f"{args.server}/api/show/{args.show}/format", args.verbose).get("audio_format")
        or "aac"
    )
    playlist_path = "opus-playlist.m3u8" if audio_format == "opus" else "playlist.m3u8"

    overlapping = find_overlapping_sessions(sessions, start_ms, end_ms)
    if not overlapping:
        print(f"No sessions overlap {fmt_ts(start_ms)} - {fmt_ts(end_ms)}", file=sys.stderr)
        print("Available sessions:", file=sys.stderr)
        for s in sorted(sessions, key=lambda s: s["timestamp_ms"]):
            s_start = s["timestamp_ms"]
            s_end = s_start + s["duration_ms"]
            print(f"  {s['section_id']}: {fmt_ts(s_start)} - {fmt_ts(s_end)}", file=sys.stderr)
        sys.exit(1)

    results = []
    for s in overlapping:
        s_start = s["timestamp_ms"]
        s_end = s_start + s["duration_ms"]
        clip_start = max(s_start, start_ms)
        clip_end = min(s_end, end_ms)
        section_id = s["section_id"]

        start_seg = estimate_segment(args.server, args.show, section_id, clip_start, args.verbose)
        end_seg = estimate_segment(args.server, args.show, section_id, clip_end, args.verbose)

        url = (
            f"{args.server}/show/{args.show}/{playlist_path}"
            f"?start_id={start_seg}&end_id={end_seg}"
        )
        results.append({
            "section_id": section_id,
            "session_start": fmt_ts(s_start),
            "session_end": fmt_ts(s_end),
            "clip_start": fmt_ts(clip_start),
            "clip_end": fmt_ts(clip_end),
            "start_segment": start_seg,
            "end_segment": end_seg,
            "segment_count": end_seg - start_seg + 1,
            "hls_url": url,
        })

    if args.json:
        print(json.dumps({"audio_format": audio_format, "results": results}, indent=2))
        return

    print(f"Show: {args.show}  Format: {audio_format}")
    print(f"Range: {fmt_ts(start_ms)} - {fmt_ts(end_ms)}")
    print(f"Matching sessions: {len(results)}")
    for r in results:
        print()
        print(f"  section_id:  {r['section_id']}")
        print(f"  session:     {r['session_start']} - {r['session_end']}")
        print(f"  clip:        {r['clip_start']} - {r['clip_end']}")
        print(f"  segments:    {r['start_segment']} - {r['end_segment']} ({r['segment_count']})")
        print(f"  hls_url:     {r['hls_url']}")


if __name__ == "__main__":
    main()
