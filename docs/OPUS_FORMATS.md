# Opus Audio Format Options

For Opus-encoded audio, the server provides HLS (HTTP Live Streaming) format for maximum compatibility across different clients and use cases.

## HLS with Opus - `/opus-playlist.m3u8`

- **Best Compatibility**: Works universally across most modern media players and browsers
- **Non-Standard but Practical**: While HLS with Opus is not officially part of the HLS specification, it has excellent real-world compatibility
- **Streaming**: True streaming without temporary files
- **Tested**: Works reliably with hls.js, VLC, and most common players
- **Container**: fMP4 segments for broad player support

## Why HLS with Opus?

Different clients, browsers, and media players have varying support for streaming protocols and codecs. HLS with Opus provides:
- Wide codec support across modern platforms
- Reliable HLS protocol implementation
- fMP4 container format compatibility
- Proven real-world reliability

## Implementation Notes

### Web Player Implementation

The web player uses HLS for Opus (via hls.js). This choice was made because:

- **Universal Browser Support**: HLS with Opus works on all major modern browsers:
  - Chrome (desktop and mobile)
  - Firefox (desktop and mobile)
  - Safari (desktop and iOS)
  - Edge
- **Practical Solution**: Despite not being part of the HLS specification, HLS with Opus has proven to be reliable in practice
- **Library Maturity**: hls.js is a mature, well-maintained library with excellent Opus handling
- **Simplified Frontend**: Using HLS eliminates the need to maintain multiple protocol implementations

### Player Compatibility

**Recommended players for HLS with Opus:**
- **VLC**: Excellent support for HLS with Opus
- **MPV**: Works well with HLS with Opus
- **ffplay**: Supports HLS with Opus
- **IINA (macOS)**: Full HLS with Opus support
- **Modern web browsers**: All major browsers via hls.js
