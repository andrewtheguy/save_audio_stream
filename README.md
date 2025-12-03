# save_audio_stream

A live audio stream relay service that records Shoutcast/Icecast streams and syncs them to remote servers for playback.

> **Disclaimer**: This tool is intended for **private personal use only**, such as time-shifting live broadcasts for personal listening or capturing streams for AI/ML workflows (e.g., speech-to-text, audio analysis). Do not use this software for illegal purposes including but not limited to: setting up public relay servers, redistributing copyrighted content, or circumventing access controls. Users are responsible for complying with applicable laws and the terms of service of any streams they access.

## Screenshot
<img width="3457" height="2058" alt="Screenshot 2025-11-28 at 3 47 50 PM" src="https://github.com/user-attachments/assets/3a1e29be-e078-4ac8-8d92-89d3c64e7e76" />


## Architecture Overview

This tool is designed for scenarios where you need to:
1. **Record** audio streams on a server with stable internet connection
2. **Relay** recordings to another server that may have intermittent connectivity
3. **Play back** the synced audio via web browser

```
                                    ┌─────────────────────┐
                       pull data    │   Receiver Server   │
                        ◄────────── │ (Less Stable/Local) │
┌─────────────────────┐   via HTTP  ├─────────────────────┤
│   Recording Server  │             │  PostgreSQL DB      │
│   (Stable Network)  │             │  Web Playback UI    │
├─────────────────────┤             │  - Can be offline   │
│                     │             │  - Pulls on demand  │
│  Internet Radio ──► │             │  - Export audio     │
│  Stream Recording   │             └─────────────────────┘
│                     │
│  - Scheduled daily  │
│  - Serves sync API  │
└─────────────────────┘
```

**Note:** Recording runs on a daily schedule (e.g., 9am-5pm) with a required break each day to prevent timestamp drift.

**Design Philosophy:**
- **Recording server** has a single responsibility: capture streams and serve sync API. It stores data temporarily in SQLite with automatic retention-based cleanup.
- **Receiver server** is the long-term storage and playback hub. Audio data should be exported/downloaded from the receiver, not the recording server.

**Data Flow:**
- **Receiver pulls data** from recording server via HTTP (sync API) and stores in PostgreSQL for permanent storage and playback

**Use Case**: Record radio streams on a cloud server with reliable connectivity. Receivers (home server, NAS) pull recordings whenever they're online. Export or download audio from the receiver for archival or processing.

## Features

- **Recording Mode**: Capture Shoutcast/Icecast streams with automatic reconnection
- **Receiver Mode**: Sync recordings from remote server with resumable transfers (PostgreSQL backend)
- **Web UI**: Browse and play back synced audio in browser (HLS streaming)
- **Gapless Playback**: Seamless audio across split segments
- SQLite storage for recording, PostgreSQL for receiver/sync
- Supports MP3 and AAC input, re-encodes to Opus (recommended), AAC, or WAV
- Scheduled recording windows (e.g., 9am-5pm daily)
- Automatic cleanup of old recordings

## Installation

### Prerequisites

- Rust toolchain (cargo, rustc)
- System libraries for audio encoding:
  - **macOS**: `brew install fdk-aac opus`
  - **Ubuntu/Debian**: `apt install libfdk-aac-dev libopus-dev`
  - **Windows**: Install via vcpkg (see below)

### Build

**macOS/Linux:**
```bash
cargo build --release
```

**Windows:**
```powershell
# Install vcpkg if not already installed
cd C:\
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
.\bootstrap-vcpkg.bat
.\vcpkg integrate install

# Install dependencies
vcpkg install fdk-aac:x64-windows opus:x64-windows

# Set VCPKG_ROOT and build
$env:VCPKG_ROOT = "C:\vcpkg"
cargo build --release
```

The binary will be at `target/release/save_audio_stream`.

Note: windows file locking is not tested yet.

## Quick Start

**On your recording server (stable connection):**

1. Copy [`config/record.example.toml`](config/record.example.toml) to `record.toml` and customize
2. Start recording:
   ```bash
   ./save_audio_stream record -c record.toml
   ```

**On your receiver server (can be offline, syncs when available):**

Prerequisites: PostgreSQL server running locally or accessible remotely.

1. Copy [`config/user/credentials.example.toml`](config/user/credentials.example.toml) to `~/.config/save_audio_stream/credentials.toml` and add your PostgreSQL password
2. Copy [`config/receiver.example.toml`](config/receiver.example.toml) to `receiver.toml` and customize
3. Start receiver:
   ```bash
   ./save_audio_stream receiver --config receiver.toml
   ```

The receiver creates PostgreSQL databases named `save_audio_{prefix}_{show_name}` for each synced show (default prefix is `show`, e.g., `save_audio_show_myradio`).

Open `http://localhost:18000` to browse and play synced recordings.

## Usage

### Recording Mode

Runs on a server with stable internet to capture audio streams continuously.

#### Record Command

```bash
save_audio_stream record -c <CONFIG_FILE> [-p <PORT>]
```

**CLI Options:**

| Option | Description |
|--------|-------------|
| `-c, --config` | Path to multi-session config file (required) |
| `-p, --port` | Override global API server port (optional) |

#### Recording Config

Config files use TOML format with `config_type = 'record'`. See [`config/record.example.toml`](config/record.example.toml) for a complete example.

### Receiver Mode

Runs on a server that may have intermittent connectivity. Syncs recordings from the recording server to PostgreSQL and provides web playback. The receiver doesn't need to be online 24/7 - it catches up automatically whenever it connects.

**Prerequisites:** PostgreSQL server with a user that has CREATE DATABASE privileges.

#### Receiver Command

```bash
save_audio_stream receiver --config <CONFIG_FILE> [--sync-only]
```

**CLI Options:**

| Option | Description |
|--------|-------------|
| `--config` | Path to receiver config file (required) |
| `--sync-only` | Sync once and exit (don't start web server) |

#### Receiver Config

Config files use TOML format with `config_type = 'receiver'`. See [`config/receiver.example.toml`](config/receiver.example.toml) for a complete example.

Credentials are stored in `~/.config/save_audio_stream/credentials.toml`. See [`config/user/credentials.example.toml`](config/user/credentials.example.toml) for the format.

**Database naming:** Each show is stored in a separate PostgreSQL database named `save_audio_{prefix}_{show_name}` (default prefix is `show`). The databases are created automatically if they don't exist.

**Note:** Periodic sync runs automatically in the backend at `sync_interval_seconds` intervals. The web UI displays sync status but does not control the sync schedule.

### Config Options

#### Recording Config Options

**Required:**

| Option | Description |
|--------|-------------|
| `config_type` | Must be `'record'` for recording configurations |

**Global Options:**

| Option | Description | Default |
|--------|-------------|---------|
| `output_dir` | Base output directory for all sessions | tmp |
| `api_port` | Port for API server serving all sessions | 17000 |

**Session Options:**

| Option | Description | Default | Required |
|--------|-------------|---------|----------|
| `url` | URL of the Shoutcast/Icecast stream | - | Yes |
| `name` | Name prefix for output | - | Yes |
| `schedule` | Schedule configuration table (contains `record_start` and `record_end`) | - | Yes |
| `audio_format` | Audio encoding: `aac`, `opus`, or `wav` | opus | No |
| `bitrate` | Bitrate in kbps | 32 (AAC), 16 (Opus) | No |
| `split_interval` | Split chunks every N seconds (0 = no split) | 0 | No |
| `storage_format` | Storage format (currently only `sqlite` supported) | sqlite | No |
| `retention_hours` | Auto-delete recordings older than N hours | None (keep forever) | No |

**Schedule Options (inside `[sessions.schedule]`):**

| Option | Description | Required |
|--------|-------------|----------|
| `record_start` | Recording start time in UTC (HH:MM format) | Yes |
| `record_end` | Recording end time in UTC (HH:MM format) | Yes |

**Note:** The API server always runs in the main thread on the configured `api_port` (default: 17000). It provides synchronization endpoints for all shows being recorded, enabling remote access and database syncing while recording is in progress. The API server is required for sync functionality.

#### Receiver/Sync Config Options

**Required:**

| Option | Description |
|--------|-------------|
| `config_type` | Must be `'receiver'` for receiver/sync configurations |
| `remote_url` | URL of remote recording server (e.g., `http://remote:17000`) |
| `postgres_url` | PostgreSQL connection URL without password (e.g., `postgres://user@localhost:5432`) |
| `credential_profile` | Profile name to look up password from `~/.config/save_audio_stream/credentials` |

**Optional:**

| Option | Description | Default |
|--------|-------------|---------|
| `shows` | Array of show names to sync (whitelist) | All shows from remote |
| `chunk_size` | Batch size for fetching chunks | 100 |
| `port` | HTTP server port for web UI | 18000 |
| `sync_interval_seconds` | Polling interval for background sync | 60 |
| `database_prefix` | Prefix in database name (`save_audio_{prefix}_{show}`) | `show` |

### Examples

Record multiple sessions from config:
```bash
save_audio_stream record -c config/sessions.toml
```

Override global API server port:
```bash
save_audio_stream record -c config/sessions.toml -p 17000
```

### Receiver Command (Primary Serving Mode)

The receiver command is the primary way to serve and browse recorded audio:

```bash
save_audio_stream receiver -c config/receiver.toml
```

**Features:**
- Web UI for browsing and playing multiple shows
- Background sync from remote recording server
- Manual "Sync Now" button for on-demand syncing
- Configurable sync intervals

**Sync-only mode** (sync without starting server):
```bash
save_audio_stream receiver -c config/receiver.toml --sync-only
```

### Inspect Command (Single Database)

The inspect command is for inspecting individual SQLite database files directly:

```bash
save_audio_stream inspect <database.sqlite> [-p PORT]
```

**Use cases:**
- Debugging a specific database file
- Quick preview of a single recording
- Testing without a full receiver setup

## Output

All recordings are stored in SQLite databases:

```
output_dir/name.sqlite
```

**Database Schema:**

```sql
-- Key-value metadata
CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Keys: version (schema version, currently "4"),
--       unique_id, name, audio_format, split_interval, bitrate, sample_rate,
--       is_recipient (for sync databases)

-- Recording sections (sessions)
CREATE TABLE sections (
    id INTEGER PRIMARY KEY,                   -- Microsecond timestamp when section started
    start_timestamp_ms INTEGER NOT NULL       -- Timestamp from HTTP Date header (milliseconds)
);

-- Audio segments
CREATE TABLE segments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms INTEGER NOT NULL,            -- Unix timestamp in milliseconds
    is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,  -- 1 for session boundaries
    audio_data BLOB NOT NULL,
    section_id INTEGER NOT NULL REFERENCES sections(id)  -- References sections table
);

-- Indexes for efficient queries
CREATE INDEX idx_segments_boundary ON segments(is_timestamp_from_source, timestamp_ms);
CREATE INDEX idx_segments_section_id ON segments(section_id);
CREATE INDEX idx_sections_start_timestamp ON sections(start_timestamp_ms);
```

**Note:** Files can be generated from the database if needed. The database format provides better reliability and supports incremental syncing.

### Output Format Specifications

| Format | Sample Rate | Channels | Default Bitrate | Notes |
|--------|-------------|----------|-----------------|-------|
| AAC-LC | 16 kHz | Mono | 32 kbps | Good compatibility |
| Opus | 48 kHz | Mono | 16 kbps | Best quality/size ratio |
| WAV | Source rate | Mono | N/A | Lossless, large files |

### AAC Implementation Notes

- The `fdk-aac` crate is used for **encoding** - it's the only practical choice for AAC encoding in Rust without FFmpeg
- **Symphonia AAC decoder** is recommended for decoding - more stable and reliable than fdk-aac decoder
- Priming sample metadata is written to enable gapless playback during decoding

## Gapless Playback

Split files are designed for **gapless playback** when concatenated:

- **WAV**: Sample-perfect splitting - concatenated files are bit-identical to unsplit recording
- **Opus**: Continuous granule positions across files for seamless playback
- **AAC**: Gapless when priming samples from metadata are accounted for during decoding

### Verifying Gapless Splits

Run the test suite to verify gapless behavior:
```bash
cargo test
```

The tests verify:
- WAV files: Exact sample match between input and concatenated split outputs
- Opus files: Continuous granule positions and sample count verification
- Edge cases: Various split intervals, boundary conditions, prime sample counts

### Running SFTP Tests

The SFTP module includes integration tests that are ignored by default. These tests require rclone to provide a test SFTP server.

**Prerequisites:**

Install rclone if not already installed:
```bash
# macOS
brew install rclone

# Ubuntu/Debian
sudo apt install rclone

# Or download from https://rclone.org/downloads/
```

**Automated Testing (Recommended):**

Use the provided helper script that automatically starts rclone and runs the tests:

```bash
./scripts/run-sftp-tests.sh
```

The script will:
- Start rclone SFTP server with `:memory:` backend
- Run all SFTP integration tests
- Automatically clean up the server when done

**Manual Testing (Alternative):**

1. Start rclone SFTP test server in a separate terminal:
```bash
rclone serve sftp :memory: --addr :13222 --user demo --pass demo
```

2. Run the SFTP integration tests:
```bash
cargo test --test sftp_test -- --ignored
```

3. Stop the server with `Ctrl+C` (memory backend is automatically discarded)

**What Gets Tested:**
- Small file uploads (1KB)
- Large file uploads (10MB) with progress callbacks
- Streaming uploads from memory without local files
- Nested directory creation (including multiple files in same directory)
- Atomic vs non-atomic upload modes
- **CRC32 checksum validation** for all uploads (data integrity)
- **Temporary file cleanup** verification (no `.tmpupload` files left behind)
- Connection and authentication error handling

## Recording Session Boundaries

The application uses `is_timestamp_from_source` flag in the database to track recording session boundaries:

- Each HTTP connection starts a new recording session
- The first segment of each connection has `is_timestamp_from_source = 1`
- This segment gets its timestamp from the HTTP Date header (accurate wall-clock time)
- Subsequent segments have calculated timestamps

**Benefits:**
- Accurate detection of contiguous segments from the same recording session
- Natural boundaries when connections drop and reconnect or at schedule breaks
- Database can accumulate multiple sessions over time
- API can list individual sessions with accurate start times

## Database Synchronization

The application supports one-way synchronization from a remote recording server (SQLite) to a local PostgreSQL database for asynchronous replication.

### Quick Start

**1. Set up credentials and config:**

- Copy [`config/user/credentials.example.toml`](config/user/credentials.example.toml) to `~/.config/save_audio_stream/credentials.toml` and add your PostgreSQL password
- Copy [`config/receiver.example.toml`](config/receiver.example.toml) and customize for your setup

**2. Run the receiver command:**

```bash
save_audio_stream receiver -c config/receiver.toml
```

Each show is stored in a separate PostgreSQL database named `save_audio_{prefix}_{show_name}` (e.g., `save_audio_show_myradio` with default prefix). Databases are created automatically if they don't exist.

**Key Features:**
- **Web UI with show selection**: Browse and play synced shows through a web interface
- **Background continuous sync**: Automatic polling at configurable intervals
- **Manual sync trigger**: "Sync Now" button in UI for on-demand syncing
- **PostgreSQL storage**: Each show in its own database for isolation and scalability
- Resumable sync with automatic checkpoint tracking
- Database protection with `is_recipient` flag prevents recording to sync targets
- Sequential processing with fail-fast error handling
- REST API endpoints for show listing and segment fetching

📖 **For detailed documentation, see [docs/syncing_design.md](docs/syncing_design.md)**

## Supported Input Formats

- `audio/mpeg` / `audio/mp3` (MP3)
- `audio/aac` / `audio/aacp` / `audio/x-aac` (AAC)

## HTTP Server

The tool includes an HTTP server to stream recorded audio from SQLite databases.

### Commands

```bash
# Primary: Receiver mode - serve multiple synced shows with background sync
save_audio_stream receiver -c config/receiver.toml

# Alternative: Inspect mode - serve a single database file directly
save_audio_stream inspect <database.sqlite> [-p PORT]
```

### Server Features

- **Web UI**: Browse and access recorded audio segments
- **HLS Streaming**: HTTP Live Streaming with Opus or AAC audio
- **fMP4 Segments**: Individual audio segments for player compatibility
- **REST API**: Query segment ranges and metadata

### Available Endpoints

#### Playback API (Inspect & Receiver Modes)

These endpoints are available for browsing and playing back recorded audio. The main difference between modes is URL structure:

- **Inspect mode**: Direct paths (e.g., `/api/sessions`)
- **Receiver mode**: Show-prefixed paths (e.g., `/api/show/{show_name}/sessions`)

| Endpoint | Inspect Mode | Receiver Mode | Description |
|----------|--------------|---------------|-------------|
| Web UI | `GET /` | `GET /` | Web UI for browsing and playback |
| Mode check | - | `GET /api/mode` | Returns `{"mode": "receiver"}` |
| Show list | - | `GET /api/shows` | List all synced shows |
| Format | `GET /api/format` | `GET /api/show/{show}/format` | Audio format (opus/aac) |
| Metadata | `GET /api/metadata` | `GET /api/show/{show}/metadata` | Show metadata (format, bitrate, sample rate) |
| Segment range | `GET /api/segments/range` | `GET /api/show/{show}/segments/range` | Min/max segment IDs |
| Sessions | `GET /api/sessions` | `GET /api/show/{show}/sessions` | List recording sessions with metadata |
| Session latest | `GET /api/session/{id}/latest` | - | Latest segment info for a session |
| Estimate segment | `GET /api/session/{id}/estimate_segment?timestamp_ms=N` | `GET /api/show/{show}/session/{id}/estimate_segment?timestamp_ms=N` | Estimate segment ID from timestamp |
| Sync status | - | `GET /api/sync/status` | Check if background sync is in progress |
| Trigger sync | - | `POST /api/sync` | Manually trigger sync |

**HLS Streaming Endpoints (format-specific):**

| Audio Format | Inspect Mode | Receiver Mode | Description |
|--------------|--------------|---------------|-------------|
| Opus playlist | `GET /opus-playlist.m3u8?start_id=N&end_id=N` | `GET /show/{show}/opus-playlist.m3u8?...` | HLS playlist for Opus |
| Opus segment | `GET /opus-segment/{id}.m4s` | `GET /show/{show}/opus-segment/{id}.m4s` | fMP4 audio segment |
| AAC playlist | `GET /playlist.m3u8?start_id=N&end_id=N` | `GET /show/{show}/playlist.m3u8?...` | HLS playlist for AAC |
| AAC segment | `GET /aac-segment/{id}.aac` | `GET /show/{show}/aac-segment/{id}.aac` | AAC audio segment |

#### Record Command API (Sync)

When running `record` command, an API server provides synchronization endpoints:

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health check endpoint |
| `GET /api/sync/shows` | List available shows for syncing |
| `GET /api/sync/shows/{show_name}/metadata` | Show metadata (format, bitrate, etc.) |
| `GET /api/sync/shows/{show_name}/sections` | List all sections (recording sessions) |
| `GET /api/sync/shows/{show_name}/segments?start_id=N&end_id=N&limit=N` | Fetch segments for syncing |

### API Details

#### Estimating Segment ID from Timestamp

The estimate segment API calculates which segment corresponds to a given timestamp within a session.

**Endpoints:**
- Inspect mode: `GET /api/session/{section_id}/estimate_segment?timestamp_ms={timestamp}`
- Receiver mode: `GET /api/show/{show_name}/session/{section_id}/estimate_segment?timestamp_ms={timestamp}`

**Features:**
- **Linear interpolation**: Estimates segment ID based on position within session duration
- **Bound checking**: Returns error if timestamp is outside session boundaries
- **Session info**: Error responses include session start/end timestamps for reference

**Example Usage:**

```bash
# Get sessions first to find section_id
curl http://localhost:16000/api/sessions

# Estimate segment for a specific timestamp
curl "http://localhost:16000/api/session/1737550800000000/estimate_segment?timestamp_ms=1737552600000"

# Response:
{
  "section_id": 1737550800000000,
  "estimated_segment_id": 150,
  "timestamp_ms": 1737552600000
}
```

**Error Responses:**

- `400 Bad Request`: Timestamp is outside session bounds (response includes `section_start_ms` and `section_end_ms`)
- `404 Not Found`: Section doesn't exist or has no segments

```json
{
  "error": "Timestamp 1737550000000 is before section start (1737550800000)",
  "section_start_ms": 1737550800000,
  "section_end_ms": 1737554400000
}
```

### Development Workflow

The server has two modes with different asset serving strategies:

#### Debug Mode (Development)

Run the Deno dev server and Axum server in separate terminals:

```bash
# Terminal 1: Start Deno dev server on port 21173
cd app && deno task dev

# Terminal 2: Run the Axum server
cargo run -- inspect database.sqlite -p 16000
```

Visit `http://localhost:16000` to access the web UI. The Axum server proxies frontend requests to the dev server. The dev server watches for file changes and rebuilds automatically (manual browser refresh required).

**Prerequisites for development:**
- Deno installed (https://deno.land)

#### Release Mode (Production)

Build and run the production server with embedded assets:

```bash
# Build release binary (automatically builds and embeds frontend)
cargo build --release

# Run the server
./target/release/save_audio_stream inspect database.sqlite -p 16000
```

In release mode:
- Frontend assets are automatically built via `deno task build` during cargo build
- Assets are embedded into the binary using `include_bytes!`
- No separate dev server needed
- Single binary deployment

### Build Process

The `build.rs` script automatically:
1. Detects release builds
2. Checks for Deno availability
3. Runs `deno task build` in the `frontend/` directory to compile frontend assets to `frontend/dist/`
4. Embeds assets into the binary at compile time via `include_bytes!`

### Example Usage

**Start server on default port (16000):**
```bash
save_audio_stream inspect ./tmp/myradio.sqlite
```

**Start server on custom port:**
```bash
save_audio_stream inspect ./tmp/myradio.sqlite -p 8080
```

**Access the web UI:**
```
http://localhost:16000
```

The web UI displays:
- **For Opus databases:**
  - HLS URL: `/opus-playlist.m3u8?start_id={min}&end_id={max}`
- **For AAC databases:**
  - HLS URL: `/playlist.m3u8?start_id={min}&end_id={max}`
- Current segment range from the database

## Releasing

The GitHub Action supports two modes:

- **Prerelease** (default): Leave the version field empty to create a prerelease with a timestamp tag
- **Release**: Enter a version (e.g., `0.1.2`) to create a stable release with matching Docker tag

### Creating a Release

**Automated (Recommended):**

Use the release script to automate version bumping and workflow triggering:

```bash
python3 scripts/release.py
```

The script will:
1. Verify git working directory is clean
2. Verify you're on the `main` branch and synced with remote
3. Calculate the next patch version (e.g., `0.1.8` → `0.1.9`)
4. Trigger the GitHub Actions workflow which bumps `Cargo.toml`/`Cargo.lock`, commits, and builds

**Prerequisites:** `gh` CLI must be installed and authenticated (`gh auth login`).

**Manual:**

1. Go to Actions → Build → Run workflow
2. Enter the version number (e.g., `0.1.2`)
3. Check "bump_version" to have the workflow update Cargo.toml/Cargo.lock
4. Run the workflow

The workflow automatically bumps the version, commits, creates the GitHub release, and builds Docker multi-arch images with the version tag.

## License

MIT
