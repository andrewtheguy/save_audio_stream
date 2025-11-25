# Syncing Design Document

**Status**: ✅ Implemented

This document describes the synchronization system that enables the relay architecture.

## Overview

The sync system enables a **distributed recording architecture**:

```
                                    ┌─────────────────────┐
                         pull sync  │   Receiver Server   │
                        ◄────────── │ (Less Stable/Local) │
┌─────────────────────┐   (HTTP)    ├─────────────────────┤
│   Recording Server  │             │  Web Playback UI    │
│   (Stable Network)  │             │  Syncs when online  │
├─────────────────────┤             └─────────────────────┘
│  Radio Stream ────► │
│  SQLite Database    │             ┌─────────────────────┐
│                     │   push      │   SFTP Storage      │
│  Scheduled daily    │ ──────────► │   (Optional)        │
│  Serves sync API    │   archive   │  Long-term backup   │
└─────────────────────┘             └─────────────────────┘
```

**Key Design Goals:**
- **Recording server** runs on stable infrastructure with scheduled daily recording windows (required break prevents drift)
- **Receiver pulls** from recording server - can have intermittent connectivity, catches up automatically
- **SFTP push** (optional) - recording server archives completed sessions to remote storage
- **Resumable transfers** - interrupted syncs resume from last successful segment

## Use Case

Record radio streams on a cloud server (stable connection, limited storage), then:
- **Receivers** (home server, NAS) pull recordings whenever they're online
- **SFTP storage** receives archived sessions pushed from recording server (optional long-term backup)

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
| `--remote-url` | `-r` | URL of remote recording server (e.g., http://remote:17000) | Required |
| `--local-dir` | `-l` | Local base directory for synced databases | Required |
| `--show` | `-n` | Show name(s) to sync (can specify multiple) | Required |
| `--chunk-size` | `-s` | Batch size for segment fetching | 100 |

### Examples

**Sync a single show:**
```bash
save_audio_stream sync \
  -r http://remote:17000 \
  -l ./synced \
  -n myradio
```

**Sync multiple shows:**
```bash
save_audio_stream sync \
  -r http://remote:17000 \
  -l ./synced \
  -n show1 -n show2 -n show3
```

**Sync with custom chunk size:**
```bash
save_audio_stream sync \
  -r http://remote:17000 \
  -l ./synced \
  -n myradio \
  -s 500
```

## Receiver Mode (Continuous Sync Server)

For long-running deployments, use the `receiver` command instead of `sync`. This starts an HTTP server with:
- **Background periodic sync**: Runs automatically at configurable intervals (default: 60 seconds)
- **Web UI**: Browse and play synced audio
- **Manual sync trigger**: Optional button to trigger immediate sync

### Command Syntax

```bash
save_audio_stream receiver --config <CONFIG_FILE>
```

Or for one-time sync without starting the server:
```bash
save_audio_stream receiver --config <CONFIG_FILE> --sync-only
```

### Configuration (TOML)

```toml
config_type = "receiver"
remote_url = "http://remote:17000"
local_dir = "./synced"
shows = ["show1", "show2"]  # Optional filter
port = 18000
sync_interval_seconds = 60
chunk_size = 100
```

### Sync Architecture

**Periodic sync is backend-driven:**
- A background thread runs on the server, triggering sync at `sync_interval_seconds` intervals
- The frontend web UI only displays sync status (polls `/api/sync/status` every 3 seconds)
- The "Sync Now" button provides manual trigger but is not required for operation
- An atomic flag prevents concurrent sync operations

```
┌─────────────────────────────────────────────────────┐
│              Receiver Backend                       │
│  ┌─────────────────────────────────────────────┐   │
│  │  Background Sync Thread (std::thread)       │   │
│  │  - Runs every sync_interval_seconds         │   │
│  │  - Calls sync_shows() automatically         │   │
│  │  - Uses AtomicBool to prevent overlap       │   │
│  └─────────────────────────────────────────────┘   │
│                                                     │
│  ┌─────────────────────────────────────────────┐   │
│  │  HTTP Server (Tokio async)                  │   │
│  │  - GET /api/sync/status → check progress    │   │
│  │  - POST /api/sync → manual trigger          │   │
│  │  - Serves web UI and audio streams          │   │
│  └─────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

### Receiver Configuration Options

| Option | Description | Default |
|--------|-------------|---------|
| `remote_url` | URL of remote recording server | Required |
| `local_dir` | Local directory for synced databases | Required |
| `shows` | Show name filter (omit for all) | All shows |
| `port` | HTTP server port | 18000 |
| `sync_interval_seconds` | Seconds between automatic syncs | 60 |
| `chunk_size` | Batch size for segment fetching | 100 |

## One-Shot Sync Behavior

- **Sequential Processing**: Shows are synced one at a time in the order specified
- **Resumable**: Automatically resumes from last synced segment if interrupted
- **Fail-Fast**: Exits immediately on any network error or metadata mismatch
- **No Retry**: Network errors are not retried - run the command again to resume
- **Validation**: Validates metadata compatibility (format, bitrate, split_interval) on resume
- **Chunked Transfer**: Fetches chunks in batches to handle large datasets efficiently
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

The sender (recording server) exposes these endpoints for synchronization and audio export:

| Endpoint | Description |
|----------|-------------|
| `GET /api/sync/shows` | List all available shows (databases) |
| `GET /api/sync/shows/:name/metadata` | Get show metadata and segment range |
| `GET /api/sync/shows/:name/sections` | Get sections metadata (id, start_timestamp_ms) |
| `GET /api/sync/shows/:name/sections/:section_id/export` | **Export section audio to file** (Opus→.ogg, AAC→.aac) |
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
    "audio_data": "<base64 encoded binary data>",
    "section_id": 1700000000000000
  },
  ...
]
```

### Export Response

```json
{
  "file_path": "tmp/am1430_20250122_143000_62c4b12369400.ogg",
  "section_id": 1737550800000000,
  "format": "opus",
  "duration_seconds": 3600.0
}
```

**Export Features:**
- No re-encoding (direct database to file)
- Filename format: `{showname}_{yyyymmdd_hhmmss}_{hex_section_id}.{ext}`
- Concurrent safety via file locking (returns 409 Conflict if export in progress)
- Saved to `tmp/` directory

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

Recording mode automatically cleans up old sections to prevent unbound database growth:

- **Retention Period**: Configurable via `RETENTION_HOURS` constant in `src/record.rs` (default: 168 hours / ~1 week)
- **Boundary Preservation**: Always keeps complete sessions by deleting only before natural boundaries (segments with `is_timestamp_from_source = 1`)
- **Timing**: Runs once per day after each recording window completes
- **Safety**: Non-destructive - if cleanup fails, recording continues normally with a warning

**How it works:**
1. Calculates cutoff timestamp (current time - RETENTION_HOURS)
2. Finds the last boundary section before cutoff
3. Deletes all segments with section_id < boundary_section_id
4. Logs the number of segments and sections deleted

This ensures:
- At least ~1 week of data is always retained
- Complete sessions are preserved (no mid-session cuts)
- Disk space is managed automatically on remote servers with limited storage

**For testing:** Set `RETENTION_HOURS` to smaller values (e.g., 1 hour, 24 hours) in the code constant.
