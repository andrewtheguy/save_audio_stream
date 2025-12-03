#!/usr/bin/env python3
"""
Download audio segments from a show by timestamp range.

Usage:
    python scripts/test_download.py --date 2025-12-02 --start 19:00 --end 20:00
    python scripts/test_download.py --date 2025-12-02 --start 19:00 --end 20:00 --show am1430
"""

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime

import requests

# Configuration
DEFAULT_SERVER = "https://saveaudio.local.168234.xyz"
DEFAULT_SHOW = "am1430"


def parse_args():
    parser = argparse.ArgumentParser(description="Download audio segments by timestamp range")
    parser.add_argument("--server", default=DEFAULT_SERVER, help=f"Server URL (default: {DEFAULT_SERVER})")
    parser.add_argument("--show", default=DEFAULT_SHOW, help=f"Show name (default: {DEFAULT_SHOW})")
    parser.add_argument("--date", required=True, help="Date in YYYY-MM-DD format")
    parser.add_argument("--start", required=True, help="Start time in HH:MM format")
    parser.add_argument("--end", required=True, help="End time in HH:MM format")
    parser.add_argument("--output", "-o", help="Output filename (auto-generated if not specified)")
    parser.add_argument("--dry-run", action="store_true", help="Show what would be done without downloading")
    parser.add_argument("--verbose", "-v", action="store_true", help="Print API requests and responses")
    return parser.parse_args()


def api_get(url: str, verbose: bool = False) -> dict:
    """Make a GET request and return JSON response, with optional verbose logging."""
    if verbose:
        print(f"  -> GET {url}")

    resp = requests.get(url)
    resp.raise_for_status()
    data = resp.json()

    if verbose:
        print(f"  <- {resp.status_code}")
        print(json.dumps(data, indent=2))

    return data


def to_timestamp_ms(date_str: str, time_str: str) -> int:
    """Convert local date and time strings to Unix timestamp in milliseconds (UTC)."""
    # Parse as naive datetime, then treat as local time
    dt_naive = datetime.strptime(f"{date_str} {time_str}", "%Y-%m-%d %H:%M")
    # Use astimezone() to convert local time to UTC
    dt_local = dt_naive.astimezone()
    return int(dt_local.timestamp() * 1000)


def find_session(sessions: list, timestamp_ms: int) -> dict | None:
    """Find the session that contains the given timestamp."""
    for session in sessions:
        start = session["timestamp_ms"]
        end = start + session["duration_ms"]
        if start <= timestamp_ms < end:
            return session
    return None


def main():
    args = parse_args()

    # Convert times to milliseconds
    start_ms = to_timestamp_ms(args.date, args.start)
    end_ms = to_timestamp_ms(args.date, args.end)

    print(f"Looking for segments between:")
    print(f"  Start: {args.date} {args.start} ({start_ms} ms)")
    print(f"  End:   {args.date} {args.end} ({end_ms} ms)")

    # Step 1: Get sessions
    print("\n=== Fetching sessions ===")
    sessions_url = f"{args.server}/api/show/{args.show}/sessions"
    data = api_get(sessions_url, args.verbose)
    sessions = data.get("sessions", [])

    print(f"Found {len(sessions)} sessions")

    # Find the session containing our start time
    session = find_session(sessions, start_ms)
    if not session:
        print("\nCould not find a session containing the start time.")
        print("Available sessions:")
        for s in sessions:
            start = s["timestamp_ms"]
            end = start + s["duration_ms"]
            start_dt = datetime.fromtimestamp(start / 1000).strftime("%Y-%m-%d %H:%M:%S")
            end_dt = datetime.fromtimestamp(end / 1000).strftime("%Y-%m-%d %H:%M:%S")
            print(f"  {s['section_id']}: {start_dt} - {end_dt} ({s['duration_ms'] / 1000:.0f}s)")
        sys.exit(1)

    section_id = session["section_id"]
    print(f"\n=== Using section_id: {section_id} ===")

    # Step 2: Estimate segment IDs
    print("\n=== Estimating start segment ===")
    start_url = f"{args.server}/api/show/{args.show}/session/{section_id}/estimate_segment?timestamp_ms={start_ms}"
    start_data = api_get(start_url, args.verbose)
    start_segment = start_data["estimated_segment_id"]
    print(f"Start segment: {start_segment}")

    print("\n=== Estimating end segment ===")
    end_url = f"{args.server}/api/show/{args.show}/session/{section_id}/estimate_segment?timestamp_ms={end_ms}"
    end_data = api_get(end_url, args.verbose)
    end_segment = end_data["estimated_segment_id"]
    print(f"End segment: {end_segment}")

    print(f"\n=== Segment Range ===")
    print(f"Start segment: {start_segment}")
    print(f"End segment: {end_segment}")
    print(f"Total segments: {end_segment - start_segment + 1}")

    # Step 3: Get audio format
    print("\n=== Fetching audio format ===")
    format_url = f"{args.server}/api/show/{args.show}/format"
    format_data = api_get(format_url, args.verbose)
    audio_format = format_data.get("audio_format") or format_data.get("format") or "aac"
    print(f"Audio format: {audio_format}")

    # Step 4: Build playlist URL and output filename
    if audio_format == "opus":
        playlist_url = f"{args.server}/show/{args.show}/opus-playlist.m3u8?start_id={start_segment}&end_id={end_segment}"
        ext = "ogg"
    else:
        playlist_url = f"{args.server}/show/{args.show}/playlist.m3u8?start_id={start_segment}&end_id={end_segment}"
        ext = "aac"

    if args.output:
        output_file = args.output
    else:
        date_part = args.date.replace("-", "")
        start_part = args.start.replace(":", "")
        end_part = args.end.replace(":", "")
        output_file = f"{args.show}_{date_part}_{start_part}_to_{end_part}.{ext}"

    print(f"\n=== Downloading with ffmpeg ===")
    print(f"Playlist URL: {playlist_url}")
    print(f"Output file: {output_file}")

    if args.dry_run:
        print("\n[Dry run - not downloading]")
        return

    # Download to file
    print(f"Downloading to file: {output_file}")
    cmd = ["ffmpeg", "-y", "-i", playlist_url, "-c", "copy", output_file, "-loglevel", "error"]
    result = subprocess.run(cmd)


    if result.returncode == 0:
        print(f"\n=== Done! ===")
        print(f"Output saved to: {output_file}")
    else:
        print(f"\n=== Error: ffmpeg remux failed with code {result.returncode} ===")
        sys.exit(result.returncode)


if __name__ == "__main__":
    main()
