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

## Supported Input Formats

- `audio/mpeg` / `audio/mp3` (MP3)
- `audio/aac` / `audio/aacp` / `audio/x-aac` (AAC)

## License

MIT
