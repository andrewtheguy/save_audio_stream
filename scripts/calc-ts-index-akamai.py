#!/usr/bin/env python3
"""
Calculate the .ts segment URL for a given target time using Akamai's X-Akamai-Live-Origin-QoS header.

This script fetches a reference .ts segment and extracts the Unix timestamp from the
X-Akamai-Live-Origin-QoS header to calculate segment indices.

Usage:
    python3 calc-ts-index-akamai.py "10:00"           # Today at 10:00 +0800
    python3 calc-ts-index-akamai.py "2025-11-26 10:00" # Specific date and time
"""

import sys
import re
import urllib.request
from datetime import datetime, timezone, timedelta

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


def fetch_akamai_timestamp(ts_index):
    """Fetch the X-Akamai-Live-Origin-QoS header from a .ts segment."""
    url = f"{BASE_URL}index_64_a{ts_index}.ts"

    request = urllib.request.Request(url, method='HEAD')
    with urllib.request.urlopen(request) as response:
        qos_header = response.headers.get('X-Akamai-Live-Origin-QoS')

    if not qos_header:
        raise ValueError(f"No X-Akamai-Live-Origin-QoS header in response for {url}")

    # Parse t=1764137375.472 from the header
    match = re.search(r't=([\d.]+)', qos_header)
    if not match:
        raise ValueError(f"Could not parse timestamp from header: {qos_header}")

    # Also parse duration d=10000 (in milliseconds)
    duration_match = re.search(r'd=(\d+)', qos_header)
    duration_ms = int(duration_match.group(1)) if duration_match else 10000

    timestamp = float(match.group(1))
    return timestamp, duration_ms / 1000  # Convert duration to seconds


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
    """Calculate the .ts URL for the given target time using Akamai timestamp."""
    # Get reference segment from playlist
    ref_index, playlist_duration = fetch_playlist()

    # Get Akamai timestamp for that segment
    akamai_timestamp, akamai_duration = fetch_akamai_timestamp(ref_index)

    # Convert Akamai timestamp to datetime (UTC)
    ref_time = datetime.fromtimestamp(akamai_timestamp, tz=timezone.utc)

    # Calculate time difference in seconds
    diff_seconds = (ref_time - target_time).total_seconds()

    # Calculate index offset
    segment_duration = akamai_duration  # Use duration from Akamai header
    index_offset = int(diff_seconds / segment_duration)

    target_index = ref_index - index_offset

    return (
        f"{BASE_URL}index_64_a{target_index}.ts",
        target_index,
        ref_time,
        ref_index,
        segment_duration,
        akamai_timestamp,
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

    url, target_index, ref_time, ref_index, segment_duration, akamai_ts = calculate_ts_url(target_time)

    print(f"Segment duration: {segment_duration}s")
    print(f"Akamai timestamp: {akamai_ts}")
    print(f"Reference: index {ref_index} at {ref_time.astimezone(TZ_HK).isoformat()}")
    print(f"Target:    index {target_index} at {target_time.isoformat()}")
    print(f"\n{url}")


if __name__ == "__main__":
    main()
