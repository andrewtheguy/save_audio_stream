#!/usr/bin/env python3
"""
Calculate the .ts segment URL for a given target time based on the HLS playlist.

Usage:
    python3 calc-ts-index.py "10:00"           # Today at 10:00 +0800
    python3 calc-ts-index.py "2025-11-26 10:00" # Specific date and time
"""

import sys
import re
import urllib.request
from datetime import datetime, timezone, timedelta

PLAYLIST_URL = "https://rthkradio2-live.akamaized.net/hls/live/2040078/radio2/index_64_a.m3u8"
BASE_URL = "https://rthkradio2-live.akamaized.net/hls/live/2040078/radio2/"

TZ_HK = timezone(timedelta(hours=8))


def fetch_playlist():
    """Fetch the HLS playlist and extract reference index, timestamp, and segment duration."""
    with urllib.request.urlopen(PLAYLIST_URL) as response:
        content = response.read().decode('utf-8')

    # Extract segment duration from #EXT-X-TARGETDURATION
    duration_match = re.search(r'#EXT-X-TARGETDURATION:(\d+)', content)
    if not duration_match:
        raise ValueError("Could not parse segment duration from playlist")
    segment_duration = int(duration_match.group(1))

    # Find EXT-X-PROGRAM-DATE-TIME and the following .ts filename
    pattern = r'#EXT-X-PROGRAM-DATE-TIME:(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+\+\d{4})\s+index_64_a(\d+)\.ts'
    match = re.search(pattern, content)

    if not match:
        raise ValueError("Could not parse playlist")

    timestamp_str = match.group(1)
    index = int(match.group(2))

    # Parse timestamp (format: 2025-11-26T14:01:12.185+0800)
    ref_time = datetime.strptime(timestamp_str, "%Y-%m-%dT%H:%M:%S.%f%z")

    return ref_time, index, segment_duration


def parse_target_time(time_str):
    """Parse target time string into datetime with +0800 timezone."""
    time_str = time_str.strip()

    # Try full datetime format first
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]:
        try:
            dt = datetime.strptime(time_str, fmt)
            return dt.replace(tzinfo=TZ_HK)
        except ValueError:
            continue

    # Try time only (use today's date)
    for fmt in ["%H:%M:%S", "%H:%M"]:
        try:
            t = datetime.strptime(time_str, fmt).time()
            today = datetime.now(TZ_HK).date()
            return datetime.combine(today, t, tzinfo=TZ_HK)
        except ValueError:
            continue

    raise ValueError(f"Cannot parse time: {time_str}")


def calculate_ts_url(target_time):
    """Calculate the .ts URL for the given target time."""
    ref_time, ref_index, segment_duration = fetch_playlist()

    # Calculate time difference in seconds
    diff_seconds = (ref_time - target_time).total_seconds()

    # Calculate index offset (each segment is segment_duration seconds)
    index_offset = int(diff_seconds / segment_duration)

    target_index = ref_index - index_offset

    return f"{BASE_URL}index_64_a{target_index}.ts", target_index, ref_time, ref_index, segment_duration


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <target_time>")
        print("Examples:")
        print(f"  {sys.argv[0]} '10:00'")
        print(f"  {sys.argv[0]} '2025-11-26 10:00'")
        sys.exit(1)

    target_str = sys.argv[1]
    target_time = parse_target_time(target_str)

    url, target_index, ref_time, ref_index, segment_duration = calculate_ts_url(target_time)

    print(f"Segment duration: {segment_duration}s")
    print(f"Reference: index {ref_index} at {ref_time.isoformat()}")
    print(f"Target:    index {target_index} at {target_time.isoformat()}")
    print(f"\n{url}")


if __name__ == "__main__":
    main()
