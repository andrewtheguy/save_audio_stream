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
│  Internet Radio ──► │             └─────────────────────┘
│  Stream Recording   │             ┌─────────────────────┐
│                     │    push     │   SFTP Storage      │
│  - Scheduled daily  │ ──────────► │   (Optional)        │
│  - Serves sync API  │ audio files │  - Long-term backup │
└─────────────────────┘             └─────────────────────┘
```

**Note:** Recording runs on a daily schedule (e.g., 9am-5pm) with a required break each day to prevent timestamp drift.

**Data Flow:**
- **Receiver pulls data** from recording server via HTTP (sync API) and stores in PostgreSQL - for live playback
- **SFTP receives exported audio files** pushed from recording server (optional) - for long-term archive

**Use Case**: Record radio streams on a cloud server with reliable connectivity. Receivers (home server, NAS) pull recordings whenever they're online. Optionally archive completed sessions to SFTP storage for long-term backup.

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

#### Receiver/Inspect Endpoints

**For Opus databases:**

| Endpoint | Description |
|----------|-------------|
| `GET /` | Web UI with dynamic segment URLs |
| `GET /opus-playlist.m3u8?start_id=N&end_id=N` | HLS playlist for Opus |
| `GET /opus-segment/{id}.m4s` | fMP4 audio segment for HLS |
| `GET /api/segments/range` | JSON with min/max segment IDs |
| `GET /api/sessions` | List all recording sessions with metadata |

**For AAC databases:**

| Endpoint | Description |
|----------|-------------|
| `GET /` | Web UI with dynamic segment URLs |
| `GET /playlist.m3u8?start_id=N&end_id=N` | HLS playlist for AAC |
| `GET /aac-segment/{id}.aac` | AAC audio segment |
| `GET /api/segments/range` | JSON with min/max segment IDs |
| `GET /api/sessions` | List all recording sessions with metadata |

#### Record Command API Endpoints

When running `record` command, an API server is available for database synchronization and audio export:

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health check endpoint |
| `GET /api/sync/shows` | List available shows for syncing |
| `GET /api/sync/shows/{show_name}/metadata` | Show metadata (format, bitrate, etc.) |
| `GET /api/sync/shows/{show_name}/sections` | List all sections (recording sessions) for a show |
| `GET /api/sync/shows/{show_name}/sections/{section_id}/export` | **Export section audio to file** |
| `GET /api/sync/shows/{show_name}/segments` | Fetch segments for syncing |

### Exporting Audio Sections

The export API allows you to export individual recording sections (sessions) as audio files without re-encoding.

**Endpoint:** `GET /api/sync/shows/{show_name}/sections/{section_id}/export`

**Features:**
- **No re-encoding**: Direct export from database to file or SFTP
- **Format-specific output**:
  - Opus → `.ogg` file (Ogg container)
  - AAC → `.aac` file (raw ADTS frames)
- **Smart filename**: `{showname}_{yyyymmdd_hhmmss}_{hex_section_id}.{ext}`
  - Timestamp based on section start time
  - Section ID in hexadecimal for uniqueness
- **Concurrent safety**: File locking prevents multiple simultaneous exports of the same section
- **Export destinations**:
  - Local files: `tmp/` directory by default
  - SFTP: Direct streaming to remote server (see SFTP configuration below)

#### SFTP Export Configuration

When SFTP export is configured globally, audio sections are streamed directly to the remote SFTP server from memory without creating local temporary files.

See [`config/record_with_export.example.toml`](config/record_with_export.example.toml) for a complete configuration example.

Credentials are stored in `~/.config/save_audio_stream/credentials.toml`. See [`config/user/credentials.example.toml`](config/user/credentials.example.toml) for the format.

**SFTP Export Features:**
- **Zero-disk I/O**: Audio data streams directly from database to SFTP server without local file creation
- **Atomic uploads**: Files are uploaded to a temporary location and renamed atomically to prevent partial uploads
- **Data integrity**: CRC32 checksum validation ensures uploaded data matches the source
- **Automatic cleanup**: No temporary files left behind on either local or remote systems

**Example Usage:**

```bash
# First, get available sections for a show
curl http://localhost:17000/api/sync/shows/am1430/sections

# Export a specific section (with SFTP configured)
curl http://localhost:17000/api/sync/shows/am1430/sections/1737550800000000/export

# Response (SFTP upload):
{
  "remote_path": "sftp://sftp.example.com/uploads/radio/am1430_20250122_143000_62c4b12369400.ogg",
  "section_id": 1737550800000000,
  "format": "opus",
  "duration_seconds": 3600.0
}

# Response (local file, when SFTP not configured):
{
  "file_path": "tmp/am1430_20250122_143000_62c4b12369400.ogg",
  "section_id": 1737550800000000,
  "format": "opus",
  "duration_seconds": 3600.0
}
```

**Testing with Local SFTP Server:**

For development and testing, you can quickly spin up a local SFTP server using rclone:

```bash
# Create a test directory for uploads
mkdir -p /tmp/sftp-uploads

# Start rclone SFTP server
rclone serve sftp /tmp/sftp-uploads --addr :2233 --user demo --pass demo
```

Then configure using [`config/record_with_export.example.toml`](config/record_with_export.example.toml) as a template, setting `host = 'localhost'`, `port = 2233`, `username = 'demo'`, and add the credential to `~/.config/save_audio_stream/credentials.toml`:

```toml
[sftp.local-dev]
password = "demo"
```

**Error Responses:**

- `404 Not Found`: Show or section doesn't exist
- `409 Conflict`: Export already in progress for this section
- `500 Internal Server Error`: Database, file system, or SFTP connection error

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

The GitHub Action creates prereleases by default. To promote a prerelease to a stable release:

### 1. Update Cargo.toml

Update the version in `Cargo.toml` before triggering the build so the binary has the correct version.

### 2. Promote the GitHub Release

After the build completes, go to the [Releases page](https://github.com/ai03/save_audio_stream/releases), edit the prerelease, uncheck "Set as a pre-release", and publish.

### 3. Tag the Docker Image

Tag the prerelease Docker image with the version number:

```bash
# Create multi-arch manifest for the new version
docker manifest create ghcr.io/andrewtheguy/save_audio_stream:X.Y.Z \
  ghcr.io/andrewtheguy/save_audio_stream:TIMESTAMP-x86_64 \
  ghcr.io/andrewtheguy/save_audio_stream:TIMESTAMP-arm64

# Push the manifest
docker manifest push ghcr.io/andrewtheguy/save_audio_stream:X.Y.Z
```

Replace `TIMESTAMP` with the prerelease tag (e.g., `20251129000040`) and `X.Y.Z` with the version (e.g., `0.1.2`).

## License

MIT
