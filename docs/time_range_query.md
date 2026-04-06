# Querying Audio by Time Range

This guide explains how to retrieve audio for a specific time range (e.g., 4pm to 6:15pm) using the API, including handling recording gaps within the same day.

## Key Concepts

- **Sessions** are contiguous recording periods. Gaps between sessions occur when the stream connection drops and reconnects.
- **Segments** are individual 10-second audio chunks, each with a sequential integer ID.
- **HLS playlists** can only cover segments within a single session — they cannot span gaps.
- To play a time range that crosses session boundaries, you need **multiple HLS URLs**, one per session.

## API Endpoints Used

All examples use receiver mode paths. For inspect mode, remove the `/show/{show_name}` prefix.

| Endpoint | Purpose |
|----------|---------|
| `GET /api/show/{show}/sessions?start_ts=X&end_ts=Y&sort_desc=true` | List sessions in a time window (timestamps in ms) |
| `GET /api/show/{show}/session/{section_id}/estimate_segment?timestamp_ms=N` | Map a wall-clock timestamp to a segment ID |
| `GET /api/show/{show}/opus-playlist.m3u8?start_id=X&end_id=Y` | HLS playlist (Opus format) for a segment range |
| `GET /api/show/{show}/playlist.m3u8?start_id=X&end_id=Y` | HLS playlist (AAC format) for a segment range |

## Simple Case: No Gaps

If your desired time range falls within a single session, you need one `estimate_segment` call for each boundary.

**Example: Get 4:00 PM to 6:15 PM on 4/5/2026 for show `am1430`**

### Step 1: Find sessions for that day

Convert start/end of day to Unix milliseconds and query sessions:

```bash
# 4/5/2026 00:00:00 PDT = 1775548800000 ms
# 4/6/2026 00:00:00 PDT = 1775635200000 ms
curl "https://example.com/api/show/am1430/sessions?start_ts=1775548800000&end_ts=1775635200000"
```

Response (simplified):
```json
{
  "name": "am1430",
  "sessions": [
    {
      "section_id": 1775397601141804,
      "start_id": 24081,
      "end_id": 29916,
      "timestamp_ms": 1775397601000,
      "duration_ms": 58360000
    }
  ]
}
```

One session covers 7:00 AM to 11:12 PM — our 4pm-6:15pm range fits entirely within it.

### Step 2: Estimate segment IDs for start and end times

```bash
# 4:00 PM PDT = 1775584800000 ms
curl "https://example.com/api/show/am1430/session/1775397601141804/estimate_segment?timestamp_ms=1775584800000"
# Returns: {"estimated_segment_id": 3252, ...}

# 6:15 PM PDT = 1775592900000 ms
curl "https://example.com/api/show/am1430/session/1775397601141804/estimate_segment?timestamp_ms=1775592900000"
# Returns: {"estimated_segment_id": 4062, ...}
```

### Step 3: Build HLS URL

```
/api/show/am1430/opus-playlist.m3u8?start_id=3252&end_id=4062
```

This gives you approximately 2 hours 15 minutes of audio.

## Gap Case: Time Range Spans Multiple Sessions

When recording gaps exist, a single time range may span multiple sessions. Each session requires its own HLS URL.

**Real-world example: Get 10:00 PM to 11:50 PM on 4/1/2026 for show `am1430`**

This date had multiple sessions with gaps:

```
Session A: 8:09 AM - 10:48:46 PM  (segments 17-5289)
  [gap: ~39 seconds]
Session B: 10:49:25 PM - 11:22:35 PM  (segments 5290-5488)
  [gap: ~2 seconds]
Session C: 11:22:37 PM - 12:00:37 AM  (segments 5489-5717)
```

### Step 1: Find sessions that overlap your time range

```bash
# 4/1/2026 10:00 PM PDT = 1775106000000 ms
# 4/2/2026 00:00 AM PDT = 1775113200000 ms (or use 11:50 PM = 1775112600000)
curl "https://example.com/api/show/am1430/sessions?start_ts=1775106000000&end_ts=1775112600000"
```

### Step 2: Identify which sessions overlap your desired range

Compare each session's time boundaries against your desired range:

| Session | Start | End (start + duration) | Overlaps 10pm-11:50pm? |
|---------|-------|------------------------|------------------------|
| A (1775056196075138) | 8:09 AM | 10:48:46 PM | Yes (10pm to 10:48:46pm) |
| B (1775108965874403) | 10:49:25 PM | 11:22:35 PM | Yes (fully within) |
| C (1775110957416805) | 11:22:37 PM | 12:00:37 AM | Yes (11:22:37pm to 11:50pm) |

A session overlaps if: `session_start < desired_end` AND `session_start + session_duration > desired_start`.

### Step 3: Estimate segment IDs at the cut points

For the **first session** (A), you need the segment at 10pm (your desired start). The session end is naturally at segment 5289:

```bash
curl "https://example.com/api/show/am1430/session/1775056196075138/estimate_segment?timestamp_ms=1775106000000"
# Returns: {"estimated_segment_id": 4996, ...}
```

For the **middle session** (B), use the full session range (5290-5488) — no estimation needed.

For the **last session** (C), the session start is naturally at segment 5489. You need the segment at 11:50pm:

```bash
curl "https://example.com/api/show/am1430/session/1775110957416805/estimate_segment?timestamp_ms=1775112600000"
# Returns: {"estimated_segment_id": 5653, ...}
```

### Step 4: Build HLS URLs (one per session)

| # | Time Covered | HLS URL |
|---|-------------|---------|
| 1 | 10:00 PM - 10:48:46 PM | `/api/show/am1430/opus-playlist.m3u8?start_id=4996&end_id=5289` |
| 2 | 10:49:25 PM - 11:22:35 PM | `/api/show/am1430/opus-playlist.m3u8?start_id=5290&end_id=5488` |
| 3 | 11:22:37 PM - 11:50 PM | `/api/show/am1430/opus-playlist.m3u8?start_id=5489&end_id=5653` |

The gaps (39 seconds and 2 seconds) represent periods where no audio was recorded.

## Summary: Algorithm for Time-Range Queries

```
Given: show_name, desired_start_ms, desired_end_ms

1. GET /api/show/{show}/sessions?start_ts={desired_start_ms}&end_ts={desired_end_ms}

2. For each session that overlaps [desired_start_ms, desired_end_ms]:
   a. Compute session_end_ms = session.timestamp_ms + session.duration_ms
   b. Determine effective_start:
      - If desired_start_ms > session.timestamp_ms:
          Call estimate_segment(section_id, desired_start_ms) → start_segment_id
      - Else: start_segment_id = session.start_id
   c. Determine effective_end:
      - If desired_end_ms < session_end_ms:
          Call estimate_segment(section_id, desired_end_ms) → end_segment_id
      - Else: end_segment_id = session.end_id
   d. Build HLS URL: /api/show/{show}/opus-playlist.m3u8?start_id={start}&end_id={end}

3. Result: ordered list of HLS URLs, one per session, with gaps between them.
```

## Notes

- `estimate_segment` uses linear interpolation — the returned segment ID is approximate but generally accurate to within a few seconds.
- If `timestamp_ms` falls outside a session's boundaries, the API returns a `400` error with the session's actual `section_start_ms` and `section_end_ms`.
- Timestamps are Unix milliseconds. Convert local times to UTC before querying.
- For AAC format, replace `opus-playlist.m3u8` with `playlist.m3u8` and `opus-segment` with `aac-segment`.
