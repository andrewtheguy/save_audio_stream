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
api_port = 3000            # default: 3000 (API server for all sessions)

[[sessions]]
# Required
url = 'https://stream.example.com/radio'
name = 'myradio'
record_start = '14:00'     # UTC time to start recording (HH:MM)
record_end = '16:00'       # UTC time to stop recording (HH:MM)

# Optional (with defaults)
audio_format = 'opus'      # default: 'opus' (options: aac, opus, wav)
bitrate = 24               # default: 32 for AAC, 16 for Opus
split_interval = 300       # default: 0 (no splitting, in seconds)

[[sessions]]
# Add more sessions as needed
url = 'https://stream2.example.com/radio'
name = 'myradio2'
record_start = '18:00'
record_end = '20:00'
```

#### Sync Config

```toml
# Required
config_type = 'sync'
remote_url = 'http://remote:3000'  # URL of remote recording server
local_dir = './synced'              # Local directory for synced databases

# Optional
shows = ['show1', 'show2']  # Show names to sync (omit to sync all shows from remote)
chunk_size = 100            # default: 100 (batch size for fetching segments)
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
| `api_port` | Port for API server serving all sessions | 3000 |

**Session Options:**

| Option | Description | Default | Required |
|--------|-------------|---------|----------|
| `url` | URL of the Shoutcast/Icecast stream | - | Yes |
| `name` | Name prefix for output | - | Yes |
| `record_start` | Recording start time in UTC (HH:MM) | - | Yes |
| `record_end` | Recording end time in UTC (HH:MM) | - | Yes |
| `audio_format` | Audio encoding: `aac`, `opus`, or `wav` | opus | No |
| `bitrate` | Bitrate in kbps | 32 (AAC), 16 (Opus) | No |
| `split_interval` | Split segments every N seconds (0 = no split) | 0 | No |

**Note:** The API server always runs in the main thread on the configured `api_port` (default: 3000). It provides synchronization endpoints for all shows being recorded, enabling remote access and database syncing while recording is in progress. The API server is required for sync functionality.

#### Sync Config Options

**Required:**

| Option | Description |
|--------|-------------|
| `config_type` | Must be `'sync'` for sync configurations |
| `remote_url` | URL of remote recording server (e.g., http://remote:3000) |
| `local_dir` | Local directory for synced databases |

**Optional:**

| Option | Description | Default |
|--------|-------------|---------|
| `shows` | Array of show names to sync (whitelist) | All shows from remote |
| `chunk_size` | Batch size for fetching segments | 100 |

### Examples

Record multiple sessions from config:
```bash
save_audio_stream record -c config/sessions.toml
```

Override global API server port:
```bash
save_audio_stream record -c config/sessions.toml -p 3000
```

### Other Commands

**Serve recorded audio:**
```bash
save_audio_stream serve <database.sqlite> [-p PORT]
```

**Sync from remote server:**
```bash
save_audio_stream sync -c config/sync.toml
```

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
-- Keys: uuid, name, audio_format, split_interval

-- Audio segments
CREATE TABLE segments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms INTEGER NOT NULL,  -- Unix timestamp in milliseconds
    audio_data BLOB NOT NULL
);
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
- **Stability issues**: The underlying `fdk-aac` library binding may have stability issues
- **Encoder priming delay**: AAC has inherent encoder padding that affects split files
- **Future migration**: May switch to FFmpeg-based encoding in future versions for better stability

**Recommendation**: Use **Opus** for production workloads. It provides better quality at lower bitrates and guaranteed gapless playback.

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
config_type = 'sync'
remote_url = 'http://remote:3000'
local_dir = './synced'
# shows parameter is omitted - will sync all available shows
```

**Or sync specific shows only (whitelist):**
```toml
config_type = 'sync'
remote_url = 'http://remote:3000'
local_dir = './synced'
shows = ['myradio']  # or ['show1', 'show2'] for multiple shows
```

Run the sync command:

```bash
save_audio_stream sync -c config/sync.toml
```

**Key Features:**
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
# Record audio to SQLite database
save_audio_stream record -c config.toml

# Serve audio via HTTP
save_audio_stream serve <database.sqlite> [-p PORT]
```

### Server Features

- **Web UI**: Browse and access recorded audio segments
- **Audio Streaming**: Serve audio in Ogg/Opus format with Range request support
- **DASH Streaming**: Dynamic Adaptive Streaming over HTTP with MPD manifests
- **WebM Segments**: Individual audio segments for player compatibility
- **REST API**: Query segment ranges and metadata

### Available Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /` | Web UI with dynamic segment URLs |
| `GET /audio?start_id=N&end_id=N` | Ogg/Opus audio stream |
| `GET /audio/session/{id}` | Cached audio session with Range support |
| `GET /manifest.mpd?start_id=N&end_id=N` | DASH MPD manifest |
| `GET /init.webm` | WebM initialization segment |
| `GET /segment/{id}` | Individual WebM audio segment |
| `GET /api/segments/range` | JSON with min/max segment IDs |

### Development Workflow

The server has two modes with different asset serving strategies:

#### Debug Mode (Development)

Run the Vite dev server and Axum server in separate terminals:

```bash
# Terminal 1: Start Vite dev server on port 21173
cd app && npm run dev

# Terminal 2: Run the Axum server
cargo run -- serve database.sqlite -p 3000
```

Visit `http://localhost:3000` to access the web UI. The Axum server proxies frontend requests to Vite, enabling Hot Module Replacement (HMR) for live updates during development.

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
./target/release/save_audio_stream serve database.sqlite -p 3000
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

**Start server on default port (3000):**
```bash
save_audio_stream serve ./tmp/myradio.sqlite
```

**Start server on custom port:**
```bash
save_audio_stream serve ./tmp/myradio.sqlite -p 8080
```

**Access the web UI:**
```
http://localhost:3000
```

The web UI displays:
- Audio URL: `/audio?start_id={min}&end_id={max}`
- MPD URL: `/manifest.mpd?start_id={min}&end_id={max}`
- Current segment range from the database

## License

MIT
