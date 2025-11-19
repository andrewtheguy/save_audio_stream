# save_audio_stream

A Rust CLI tool for downloading and re-encoding Shoutcast/Icecast audio streams to AAC or Opus format.

## Purpose

This tool connects to internet radio streams, decodes the audio in real-time, and re-encodes it to a compressed format optimized for voice/speech. Useful for recording radio broadcasts, podcasts, or any Shoutcast/Icecast compatible stream.

## Features

- Downloads audio from Shoutcast/Icecast streams
- Supports MP3 and AAC input formats
- Re-encodes to AAC-LC (16kHz mono) or Opus (48kHz mono)
- Configurable recording duration and bitrate
- Automatic timestamped output filenames

## Installation

### Prerequisites

- Rust toolchain (cargo, rustc)
- System libraries for audio encoding:
  - **macOS**: `brew install fdk-aac opus`
  - **Ubuntu/Debian**: `apt install libfdk-aac-dev libopus-dev`

### Build

```bash
cargo build --release
```

The binary will be at `target/release/save_audio_stream`.

## Usage

```bash
save_audio_stream -u <STREAM_URL> [OPTIONS]
```

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-u, --url <URL>` | URL of the Shoutcast/Icecast stream | Required |
| `-d, --duration <SECONDS>` | Recording duration in seconds | 30 |
| `-f, --format <FORMAT>` | Output format: `aac` or `opus` | aac |
| `-b, --bitrate <KBPS>` | Bitrate in kbps | 32 (AAC), 16 (Opus) |

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

## Output

Files are saved with timestamped names based on the server's response time:
- `recording_20241118_143022.aac`
- `recording_20241118_143022.opus`

### Output Format Specifications

| Format | Sample Rate | Channels | Default Bitrate |
|--------|-------------|----------|-----------------|
| AAC-LC | 16 kHz | Mono | 32 kbps |
| Opus | 48 kHz | Mono | 16 kbps |

## Supported Input Formats

- `audio/mpeg` / `audio/mp3` (MP3)
- `audio/aac` / `audio/aacp` / `audio/x-aac` (AAC)

## License

MIT
