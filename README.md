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
save_audio_stream -u <STREAM_URL> [OPTIONS]
```

Or with a config file:
```bash
save_audio_stream -c config/mystream.toml
```

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-c, --config <FILE>` | Path to TOML config file | None |
| `-u, --url <URL>` | URL of the Shoutcast/Icecast stream | Required |
| `-d, --duration <SECONDS>` | Recording duration in seconds | 30 |
| `-f, --format <FORMAT>` | Output format: `aac`, `opus`, or `wav` | aac |
| `-b, --bitrate <KBPS>` | Bitrate in kbps | 32 (AAC), 16 (Opus) |
| `-n, --name <NAME>` | Name prefix for output file | recording |
| `-o, --output-dir <DIR>` | Output directory | tmp/ |
| `-s, --split-interval <SECONDS>` | Split files every N seconds (0 = no split) | 0 |

### Config File Format

Create a TOML config file for frequently used streams:

```toml
url = 'https://stream.example.com/radio'
name = 'myradio'
format = 'opus'
duration = 3600
bitrate = 24
output_dir = 'recordings'
split_interval = 300
```

CLI arguments override config file values.

### Examples

Record 60 seconds from a stream to AAC:
```bash
save_audio_stream -u "http://stream.example.com/radio" -d 60
```

Record 5 minutes to Opus format:
```bash
save_audio_stream -u "http://stream.example.com/radio" -d 300 -f opus
```

Record with custom bitrate:
```bash
save_audio_stream -u "http://stream.example.com/radio" -d 120 -f aac -b 48
```

**Record with automatic file splitting** (1 hour, split every 5 minutes):
```bash
save_audio_stream -u "http://stream.example.com/radio" -d 3600 -f opus -s 300
```

Record to WAV format (lossless):
```bash
save_audio_stream -u "http://stream.example.com/radio" -d 60 -f wav
```

Use a config file:
```bash
save_audio_stream -c config/am1430.toml
```

## Output

Files are saved to the output directory (default: `tmp/`) with timestamped names based on the server's response time.

### Without Splitting
- `tmp/recording_20241118_143022.aac`
- `tmp/recording_20241118_143022.opus`
- `tmp/recording_20241118_143022.wav`

### With Splitting
When using `-s/--split-interval`, files are numbered sequentially:
- `tmp/recording_20241118_143022_000.opus`
- `tmp/recording_20241118_143022_001.opus`
- `tmp/recording_20241118_143022_002.opus`
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
