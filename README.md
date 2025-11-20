# save_audio_stream

A Rust CLI tool for downloading and re-encoding Shoutcast/Icecast audio streams to AAC, Opus, or WAV format with support for automatic file splitting.

## Purpose

This tool connects to internet radio streams, decodes the audio in real-time, and re-encodes it to a compressed format optimized for voice/speech. Useful for recording radio broadcasts, podcasts, or any Shoutcast/Icecast compatible stream.

## Features

- Downloads audio from Shoutcast/Icecast streams
- Supports MP3 and AAC input formats
- Re-encodes to AAC-LC (16kHz mono), Opus (48kHz mono), or WAV (lossless)
- **Automatic file splitting** at configurable intervals with gapless playback
- Configurable recording duration and bitrate
- Automatic timestamped output filenames
- Configuration file support (TOML format)
- Customizable output directory (defaults to `tmp/`)

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

## Usage

```bash
save_audio_stream -c <CONFIG_FILE> [-d <DURATION>]
```

### CLI Options

| Option | Description |
|--------|-------------|
| `-c, --config` | Path to config file (required) |
| `-d, --duration` | Recording duration in seconds (overrides config) |

### Config File Format (Required)

Most settings are specified in a TOML config file:

```toml
# Required
url = 'https://stream.example.com/radio'
name = 'myradio'           # Name prefix for output

# Optional (with defaults)
audio_format = 'opus'      # default: 'opus' (options: aac, opus, wav)
storage_format = 'sqlite'  # default: 'sqlite' (options: file, sqlite)
duration = 3600            # default: 30 (seconds)
bitrate = 24               # default: 32 for AAC, 16 for Opus
output_dir = 'recordings'  # default: 'tmp'
split_interval = 300       # default: 0 (no splitting, in seconds)
```

### Config Options

| Option | Description | Default | Required |
|--------|-------------|---------|----------|
| `url` | URL of the Shoutcast/Icecast stream | - | Yes |
| `name` | Name prefix for output | - | Yes |
| `audio_format` | Audio encoding: `aac`, `opus`, or `wav` | opus | No |
| `storage_format` | Storage format: `file` or `sqlite` | sqlite | No |
| `duration` | Recording duration in seconds | 30 | No |
| `bitrate` | Bitrate in kbps | 32 (AAC), 16 (Opus) | No |
| `output_dir` | Base output directory | tmp | No |
| `split_interval` | Split files every N seconds (0 = no split) | 0 | No |

### Examples

Use a config file with default duration:
```bash
save_audio_stream -c config/am1430.toml
```

Override duration from CLI (record 60 seconds):
```bash
save_audio_stream -c config/am1430.toml -d 60
```

Record for 1 hour:
```bash
save_audio_stream -c config/am1430.toml -d 3600
```

## Output

### SQLite Storage (Default)

When `storage_format = 'sqlite'`, audio segments are stored in a SQLite database:

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

### File Storage

When `storage_format = 'file'`, files are saved to a date-organized directory structure:

```
output_dir/name/yyyy/mm/dd/name_timestamp.ext
```

#### Without Splitting
- `tmp/myradio/2024/11/18/myradio_20241118_143022.opus`

#### With Splitting
When using `split_interval`, files are numbered sequentially:
- `tmp/myradio/2024/11/18/myradio_20241118_143022_000.opus`
- `tmp/myradio/2024/11/18/myradio_20241118_143022_001.opus`
- `tmp/myradio/2024/11/18/myradio_20241118_143022_002.opus`
- ...

### Output Format Specifications

| Format | Sample Rate | Channels | Default Bitrate | Notes |
|--------|-------------|----------|-----------------|-------|
| AAC-LC | 16 kHz | Mono | 32 kbps | Good for speech |
| Opus | 48 kHz | Mono | 16 kbps | Best quality/size ratio |
| WAV | Source rate | Mono | N/A | Lossless, large files |

## Gapless Playback

Split files are designed for **gapless playback** when concatenated:

- **WAV**: Sample-perfect splitting - concatenated files are bit-identical to unsplit recording
- **Opus**: Continuous granule positions across files for seamless playback
- **AAC**: Files split at frame boundaries (note: AAC has inherent encoder priming delay)

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

```bash
# Sync a single show from remote server
save_audio_stream sync -r http://remote:3000 -l ./synced -n myradio

# Sync multiple shows
save_audio_stream sync -r http://remote:3000 -l ./synced -n show1 -n show2
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

### Build Process

The `build.rs` script automatically:
1. Detects release builds (`cargo build --release`)
2. Runs `npm install` in the `app/` directory
3. Runs `npm run build` to compile frontend assets to `app/dist/`
4. Embeds assets into the binary at compile time

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
