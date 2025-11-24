# Opus Audio Format Options

For Opus-encoded audio, the server provides two different URL formats to maximize compatibility across different clients and use cases.

## Format Priority and Recommendations

### 1. HLS (Recommended) - `/opus-playlist.m3u8`
- **Best Compatibility**: Works universally across most modern media players and browsers
- **Non-Standard but Practical**: While HLS with Opus is not officially part of the HLS specification, it has better real-world compatibility than DASH
- **Streaming**: True streaming without temporary files
- **Tested**: Works reliably with hls.js, VLC, and most common players
- **Use Case**: Primary choice for most applications

### 2. DASH (Fallback) - `/manifest.mpd`
- **Industry Standard**: MPEG-DASH is the official standard for adaptive streaming
- **Limited Compatibility**: In practice, many players struggle with DASH playback despite being the standard
- **Streaming**: True streaming without temporary files
- **Use Case**: Use if your specific player requires DASH or doesn't support HLS with Opus

## Why Multiple Formats?

Different clients, browsers, and media players have varying support for:
- Opus codec support
- DASH protocol implementation
- HLS protocol implementation
- Container format compatibility

By providing both HLS and DASH formats, we ensure maximum compatibility while recommending the most practical option first based on real-world testing.

## Implementation Notes

### Web Player Implementation

The web player uses HLS for Opus (via hls.js). This choice was made because:

- **Universal Browser Support**: HLS with Opus works on all major modern browsers:
  - Chrome (desktop and mobile)
  - Firefox (desktop and mobile)
  - Safari (desktop and iOS)
  - Edge
- **Practical Solution**: Despite not being part of the HLS specification, HLS with Opus has proven to be more reliable than DASH in practice
- **Library Maturity**: hls.js is a mature, well-maintained library with excellent Opus handling
- **Simplified Frontend**: Using HLS for all browsers eliminates the need to maintain multiple protocol implementations

### For Non-Web Use Cases

**Recommended playback order:**
1. Try HLS (`/opus-playlist.m3u8`) first - works with most players
2. Fall back to DASH (`/manifest.mpd`) if your player specifically requires it

**Player compatibility:**
- **VLC**: Excellent support for HLS with Opus
- **MPV**: Works well with HLS with Opus
- **ffplay**: Supports HLS with Opus
- **IINA (macOS)**: Full HLS with Opus support
- **Most DASH players**: May have issues despite DASH being the standard
