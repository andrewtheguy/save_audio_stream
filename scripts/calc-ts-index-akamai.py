#!/usr/bin/env python3
"""
Calculate the .ts segment URL for a given target time using the Last-Modified header.

This script fetches a reference .ts segment and extracts the timestamp from the
Last-Modified header to calculate segment indices.

Usage:
    python3 calc-ts-index-akamai.py "10:00"           # Today at 10:00 +0800
    python3 calc-ts-index-akamai.py "2025-11-26 10:00" # Specific date and time
"""

import re
import sys
import urllib.request
from datetime import datetime, timezone, timedelta
from email.utils import parsedate_to_datetime

PLAYLIST_URL = "https://rthkradio2-live.akamaized.net/hls/live/2040078/radio2/index_64_a.m3u8"
BASE_URL = "https://rthkradio2-live.akamaized.net/hls/live/2040078/radio2/"

TZ_HK = timezone(timedelta(hours=8))


def fetch_playlist():
    """Fetch the HLS playlist and extract the first .ts filename and segment duration."""
    with urllib.request.urlopen(PLAYLIST_URL) as response:
        content = response.read().decode('utf-8')

    # Extract segment duration from #EXT-X-TARGETDURATION
    duration_match = re.search(r'#EXT-X-TARGETDURATION:(\d+)', content)
    if not duration_match:
        raise ValueError("Could not parse segment duration from playlist")
    segment_duration = int(duration_match.group(1))

    # Find the first .ts filename
    ts_match = re.search(r'index_64_a(\d+)\.ts', content)
    if not ts_match:
        raise ValueError("Could not find .ts segment in playlist")

    index = int(ts_match.group(1))
    return index, segment_duration


def fetch_last_modified(ts_index):
    """Fetch the Last-Modified header from a .ts segment."""
    url = f"{BASE_URL}index_64_a{ts_index}.ts"

    request = urllib.request.Request(url, method='HEAD')
    with urllib.request.urlopen(request) as response:
        last_modified = response.headers.get('Last-Modified')

    if not last_modified:
        raise ValueError(f"No Last-Modified header in response for {url}")

    # Parse Last-Modified: Wed, 26 Nov 2025 06:09:35 GMT
    ref_time = parsedate_to_datetime(last_modified)

    return ref_time


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
    """Calculate the .ts URL for the given target time using Last-Modified header."""
    # Get reference segment from playlist
    ref_index, segment_duration = fetch_playlist()

    # Get Last-Modified timestamp for that segment
    ref_time = fetch_last_modified(ref_index)

    # Calculate time difference in seconds
    diff_seconds = (ref_time - target_time).total_seconds()

    # Calculate index offset
    index_offset = int(diff_seconds / segment_duration)

    target_index = ref_index - index_offset

    return (
        f"{BASE_URL}index_64_a{target_index}.ts",
        target_index,
        ref_time,
        ref_index,
        segment_duration,
    )


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
    print(f"Reference: index {ref_index} at {ref_time.astimezone(TZ_HK).isoformat()}")
    print(f"Target:    index {target_index} at {target_time.isoformat()}")
    print(f"\n{url}")


if __name__ == "__main__":
    main()
