# save_audio_stream

A Rust CLI tool for downloading and re-encoding Shoutcast/Icecast audio streams to AAC, Opus, or WAV format with SQLite storage.

## Purpose

This tool connects to internet radio streams, decodes the audio in real-time, and re-encodes it to a compressed format optimized for voice/speech. All audio is stored in SQLite databases for reliability and easy syncing. Useful for recording radio broadcasts, podcasts, or any Shoutcast/Icecast compatible stream.

## Features

- Downloads audio from Shoutcast/Icecast streams
- Supports MP3 and AAC input formats
- Re-encodes to AAC-LC (16kHz mono), Opus (48kHz mono), or WAV (lossless)
- **SQLite storage** for reliability and incremental syncing
- **Automatic segment splitting** at configurable intervals with gapless playback
- Configurable recording duration and bitrate
- Configuration file support (TOML format)
- Customizable output directory (defaults to `tmp/`)
- Database synchronization for remote backup

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

#### Web Frontend Feature Flag

The project includes a `web-frontend` feature flag that controls whether the web UI is built and embedded into the binary:

- **Default**: Web frontend is **enabled** (requires Node.js and npm)
- **Disabled**: Build without web frontend (no npm required)

**Build without web frontend:**
```bash
cargo build --release --no-default-features
```

This is useful for:
- CI/CD environments without Node.js/npm
- Headless server deployments
- Reducing binary size when web UI is not needed

When built without the web frontend:
- All API endpoints remain fully functional
- Web UI routes (`/`, `/assets/*`) return 404 with message "Web frontend not available in this build"
- The server can still serve audio and handle sync operations

## Usage

### Record Command

```bash
save_audio_stream record -c <CONFIG_FILE> [-p <PORT>]
```

**CLI Options:**

| Option | Description |
|--------|-------------|
| `-c, --config` | Path to multi-session config file (required) |
| `-p, --port` | Override global API server port (optional) |

### Config File Format

The config file uses TOML format. All config files must specify a `config_type` to indicate whether they're for recording or syncing.

#### Recording Config

```toml
# Required
config_type = 'record'

# Global settings
output_dir = 'recordings'  # default: 'tmp' (applies to all sessions)
api_port = 17000           # default: 17000 (API server for all sessions)

[[sessions]]
# Required
url = 'https://stream.example.com/radio'
name = 'myradio'

# Optional (with defaults)
audio_format = 'opus'      # default: 'opus' (options: aac, opus, wav)
bitrate = 24               # default: 32 for AAC, 16 for Opus
split_interval = 300       # default: 0 (no splitting, in seconds)
storage_format = 'sqlite'  # default: 'sqlite'
retention_hours = 72       # Optional: auto-delete old recordings after N hours

# Schedule configuration (required)
[sessions.schedule]
record_start = '14:00'     # UTC time to start recording (HH:MM)
record_end = '16:00'       # UTC time to stop recording (HH:MM)

[[sessions]]
# Add more sessions as needed
url = 'https://stream2.example.com/radio'
name = 'myradio2'

[sessions.schedule]
record_start = '18:00'
record_end = '20:00'
```

#### Receiver/Sync Config

```toml
# Required
config_type = 'receiver'
remote_url = 'http://remote:17000'  # URL of remote recording server
local_dir = './synced'              # Local directory for synced databases

# Optional
shows = ['show1', 'show2']  # Show names to sync (omit to sync all shows from remote)
chunk_size = 100            # default: 100 (batch size for fetching chunks)
port = 18000                # default: 18000 (HTTP server port for web UI)
sync_interval_seconds = 60  # default: 60 (polling interval for background sync)
```

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
| `remote_url` | URL of remote recording server (e.g., http://remote:17000) |
| `local_dir` | Local directory for synced databases |

**Optional:**

| Option | Description | Default |
|--------|-------------|---------|
| `shows` | Array of show names to sync (whitelist) | All shows from remote |
| `chunk_size` | Batch size for fetching chunks | 100 |
| `port` | HTTP server port for web UI | 18000 |
| `sync_interval_seconds` | Polling interval for background sync | 60 |

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
| AAC-LC | 16 kHz | Mono | 32 kbps | **⚠️ Experimental** - See warning below |
| Opus | 48 kHz | Mono | 16 kbps | Best quality/size ratio |
| WAV | Source rate | Mono | N/A | Lossless, large files |

**⚠️ AAC Encoding Warning:**

AAC encoding support is **experimental** and has known limitations:
- **Not gapless**: AAC files may not provide seamless playback when concatenated
- **Stability issues**: The underlying `fdk-aac` library binding may have stability issues because it is not widely used in Rust
- **Encoder priming delay**: AAC has inherent encoder padding that affects split files

**Recommendation**: Use **Opus** for production workloads. It provides better quality at lower bitrates and guaranteed gapless playback.

### AAC Implementation Notes

**For future AAC decoding needs:**
- **Use Symphonia AAC decoder** - More stable and reliable than fdk-aac decoder
- The `fdk-aac` crate is only used for **encoding** because it's the only practical choice for AAC encoding in Rust without FFmpeg
- Decoding with Symphonia provides better error handling and stability
- The AAC encoder (fdk-aac) is necessary for encoding but has known stability issues (see warning above)

## Gapless Playback

Split files are designed for **gapless playback** when concatenated:

- **WAV**: Sample-perfect splitting - concatenated files are bit-identical to unsplit recording
- **Opus**: Continuous granule positions across files for seamless playback
- **AAC**: ⚠️ Not guaranteed gapless - AAC has inherent encoder priming delay and the encoding library has stability issues (see warning above)

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

The application supports one-way synchronization from a remote recording server to local databases for asynchronous replication.

### Quick Start

Create a sync config file (e.g., `config/sync.toml`):

**Sync all shows from remote (recommended):**
```toml
config_type = 'receiver'
remote_url = 'http://remote:17000'
local_dir = './synced'
# shows parameter is omitted - will sync all available shows
```

**Or sync specific shows only (whitelist):**
```toml
config_type = 'receiver'
remote_url = 'http://remote:17000'
local_dir = './synced'
shows = ['myradio']  # or ['show1', 'show2'] for multiple shows
```

Run the receiver command:

```bash
save_audio_stream receiver -c config/sync.toml
```

**Key Features:**
- **Web UI with show selection**: Browse and play synced shows through a web interface
- **Background continuous sync**: Automatic polling at configurable intervals
- **Manual sync trigger**: "Sync Now" button in UI for on-demand syncing
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

**Configuration with SFTP export:**

```toml
config_type = 'record'
output_dir = './tmp/recorded'
api_port = 17000

# Enable SFTP export (global setting)
export_to_sftp = true
export_to_remote_periodically = true  # Optional: periodic auto-export

# SFTP configuration (global)
[sftp]
host = 'sftp.example.com'
port = 22
username = 'uploader'
credential_profile = 'my-sftp-server'  # Reference to ~/.config/save_audio_stream/credentials
remote_dir = '/uploads/radio'

[[sessions]]
url = 'https://stream.example.com/radio'
name = 'myradio'

[sessions.schedule]
record_start = '14:00'
record_end = '16:00'
```

**Credential file format** (`~/.config/save_audio_stream/credentials`):

```toml
[my-sftp-server]
password = 'secret123'

[another-server]
password = 'another_password'
```

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

Then configure your session to use the local server:

```toml
config_type = 'record'
export_to_sftp = true

[sftp]
host = 'localhost'
port = 2233
username = 'demo'
credential_profile = 'local-dev'  # Reference to ~/.config/save_audio_stream/credentials
remote_dir = '/'

[[sessions]]
url = 'https://stream.example.com/radio'
name = 'myradio'

[sessions.schedule]
record_start = '14:00'
record_end = '16:00'
```

And add credentials:

```bash
# Create credentials file
mkdir -p ~/.config/save_audio_stream
cat > ~/.config/save_audio_stream/credentials << 'EOF'
[local-dev]
password = "demo"
EOF
```

**Error Responses:**

- `404 Not Found`: Show or section doesn't exist
- `409 Conflict`: Export already in progress for this section
- `500 Internal Server Error`: Database, file system, or SFTP connection error

### Development Workflow

The server has two modes with different asset serving strategies:

#### Debug Mode (Development)

Run the Vite dev server and Axum server in separate terminals:

```bash
# Terminal 1: Start Vite dev server on port 21173
cd app && npm run dev

# Terminal 2: Run the Axum server
cargo run -- inspect database.sqlite -p 16000
```

Visit `http://localhost:16000` to access the web UI. The Axum server proxies frontend requests to Vite, enabling Hot Module Replacement (HMR) for live updates during development.

**Prerequisites for development:**
```bash
cd app
npm install
```

#### Release Mode (Production)

Build and run the production server with embedded assets:

```bash
# Build release binary (automatically builds and embeds frontend)
cargo build --release

# Run the server
./target/release/save_audio_stream inspect database.sqlite -p 16000
```

In release mode:
- Frontend assets are automatically built via `npm run build` during cargo build
- Assets are embedded into the binary using `rust-embed`
- No separate Vite server needed
- Single binary deployment

**Note:** If the `web-frontend` feature is disabled (`--no-default-features`), the frontend build is skipped and web UI routes will return 404.

### Build Process

The `build.rs` script automatically:
1. Detects release builds with `web-frontend` feature enabled
2. Checks for npm availability
3. Runs `npm install` in the `app/` directory
4. Runs `npm run build` to compile frontend assets to `app/dist/`
5. Embeds assets into the binary at compile time via `rust-embed`

If npm is not available or the `web-frontend` feature is disabled, the frontend build is skipped.

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

## License

MIT
