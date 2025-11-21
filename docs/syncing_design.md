# Syncing Design Document

**Status**: ✅ Implemented

This document describes the design, rationale, and usage for the database synchronization feature.

## Overview

The application supports one-way synchronization from a remote recording server to local databases. This allows you to replicate audio recordings from a sender to multiple receivers.

## Requirements

- For recording mode, on the main thread it should enable a rest endpoint for syncing mode, in which it provides an endpoint to fetch all metadata from a particular show's sqlite database together with the min and max id at the time, and another api endpoint to fetch database records with ranges. Recording mode should reject database flagged with is_recipient = true.
- Add another command line option for syncing mode, in which it creates a new database per show with is_recipient = true or opens a separate sqlite database. If database already exists, check if database is recipient mode metadata matches from api endpoint including metadata session id, and get the last synced database id from metadata instead of max(id) because I might trim the data which makes max(id) less reliable.
- For syncing, it will keep on pulling data until it reaches the max id from the recording mode at the time of starting sync. It should pull data in chunks, and after each chunk it should update the last synced id in metadata table. After finish syncing, the program should exit.

## Rationale

Ideally the sender in recording mode should submit the recording data to a central server or database such as postgres, but currently the remote recording server I have with stable connection doesn't have enough space for database, so I need to record locally to sqlite and sync asynchronously.

## Architecture

### Sender (Recording Server)
- Records audio streams to SQLite databases
- Exposes HTTP API endpoints for listing shows and fetching segments
- Each database has `is_recipient = false` in metadata (allows recording)

### Receiver (Sync Client)
- Syncs show databases from remote sender to local storage
- Creates local databases with `is_recipient = true` in metadata
- Prevents accidental recording to sync target databases

## Usage

### Command Syntax

```bash
save_audio_stream sync \
  --remote-url <URL> \
  --local-dir <DIR> \
  --show <NAME> [--show <NAME>...] \
  [--chunk-size <SIZE>]
```

### Options

| Option | Short | Description | Default |
|--------|-------|-------------|---------|
| `--remote-url` | `-r` | URL of remote recording server (e.g., http://remote:3000) | Required |
| `--local-dir` | `-l` | Local base directory for synced databases | Required |
| `--show` | `-n` | Show name(s) to sync (can specify multiple) | Required |
| `--chunk-size` | `-s` | Batch size for segment fetching | 100 |

### Examples

**Sync a single show:**
```bash
save_audio_stream sync \
  -r http://remote:3000 \
  -l ./synced \
  -n myradio
```

**Sync multiple shows:**
```bash
save_audio_stream sync \
  -r http://remote:3000 \
  -l ./synced \
  -n show1 -n show2 -n show3
```

**Sync with custom chunk size:**
```bash
save_audio_stream sync \
  -r http://remote:3000 \
  -l ./synced \
  -n myradio \
  -s 500
```

## Behavior

- **Sequential Processing**: Shows are synced one at a time in the order specified
- **Resumable**: Automatically resumes from last synced segment if interrupted
- **Fail-Fast**: Exits immediately on any network error or metadata mismatch
- **No Retry**: Network errors are not retried - run the command again to resume
- **Validation**: Validates metadata compatibility (format, bitrate, split_interval) on resume
- **Chunked Transfer**: Fetches segments in batches to handle large datasets efficiently
- **Progress Tracking**: Uses `last_synced_id` metadata instead of `max(id)` for reliable resume

## Database Protection

The `is_recipient` metadata flag prevents database corruption:

```sql
-- Sender databases (allow recording)
INSERT INTO metadata (key, value) VALUES ('is_recipient', 'false');

-- Receiver databases (synced, read-only for recording)
INSERT INTO metadata (key, value) VALUES ('is_recipient', 'true');
```

If you attempt to record to a database with `is_recipient = true`, the application will exit with an error:
```
Cannot record to a recipient database. This database is configured for syncing only.
```

## API Endpoints

The sender (recording server) exposes these endpoints for synchronization:

| Endpoint | Description |
|----------|-------------|
| `GET /api/sync/shows` | List all available shows (databases) |
| `GET /api/sync/shows/:name/metadata` | Get show metadata and segment range |
| `GET /api/sync/shows/:name/segments?start_id=N&end_id=N&limit=N` | Fetch segment batch |

### Metadata Response

```json
{
  "unique_id": "db_a1b2c3d4e5f6",
  "name": "myradio",
  "audio_format": "opus",
  "split_interval": "300",
  "bitrate": "16",
  "sample_rate": "48000",
  "version": "1",
  "is_recipient": false,
  "min_id": 1,
  "max_id": 1000
}
```

### Segments Response

```json
[
  {
    "id": 1,
    "timestamp_ms": 1700000000000,
    "is_timestamp_from_source": 1,
    "audio_data": "<base64 encoded binary data>"
  },
  ...
]
```

## Implementation Details

### Key Components

- **src/sync.rs**: Main sync logic with `sync_shows()` and `sync_single_show()` functions
- **src/serve.rs**: API endpoints for listing shows, metadata, and segment fetching
- **src/record.rs**: Database protection to reject recording to recipient databases

### Metadata Validation

When resuming a sync, the following metadata fields are validated to ensure compatibility:
- `source_unique_id`: Must match remote `unique_id`
- `audio_format`: Must match (e.g., "opus")
- `split_interval`: Must match (e.g., "300")
- `bitrate`: Must match (e.g., "16")

### Error Handling

- Network errors cause immediate exit with error message
- Metadata mismatch causes immediate exit with details
- No retry logic - user must re-run the command to resume

### Automatic Cleanup

Recording mode automatically cleans up old segments to prevent unbound database growth:

- **Retention Period**: Configurable via `RETENTION_HOURS` constant in `src/record.rs` (default: 168 hours / ~1 week)
- **Boundary Preservation**: Always keeps complete sessions by deleting only before natural boundaries (segments with `is_timestamp_from_source = 1`)
- **Timing**: Runs once per day after each recording window completes
- **Safety**: Non-destructive - if cleanup fails, recording continues normally with a warning

**How it works:**
1. Calculates cutoff timestamp (current time - RETENTION_HOURS)
2. Finds the last boundary segment before cutoff
3. Deletes all segments with id < boundary_id
4. Logs the number of segments deleted

This ensures:
- At least ~1 week of data is always retained
- Complete sessions are preserved (no mid-session cuts)
- Disk space is managed automatically on remote servers with limited storage

**For testing:** Set `RETENTION_HOURS` to smaller values (e.g., 1 hour, 24 hours) in the code constant.
