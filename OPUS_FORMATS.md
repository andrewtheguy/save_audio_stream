# Opus Audio Format Options

For Opus-encoded audio, the server provides three different URL formats to maximize compatibility across different clients and use cases. There is no one-size-fits-all solution, so multiple formats are offered.

## Format Priority and Recommendations

### 1. DASH (Recommended) - `/manifest.mpd`
- **Standard**: MPEG-DASH is the industry standard for adaptive streaming
- **Compatibility**: Works with most modern media players and browsers that support DASH
- **Streaming**: True streaming without temporary files
- **Use Case**: Primary choice for most applications

### 2. HLS - `/opus-playlist.m3u8`
- **Fallback Option**: Use if DASH doesn't work with your client
- **Non-Standard**: HLS with Opus is not officially part of the HLS specification
- **Streaming**: Still allows streaming without temporary files
- **Compatibility**: Works with hls.js and some players, but not universally supported
- **Use Case**: Good fallback when DASH support is unavailable

### 3. Direct Audio - `/audio`
- **Last Resort**: Use only if both DASH and HLS fail
- **Temporary Files**: Requires the server to create temporary concatenated audio files
- **Duration Limits**: Should have max duration limits to prevent excessive disk usage
- **No Streaming**: Client must download the entire file or server must generate it
- **Use Case**: Emergency fallback for clients with no adaptive streaming support

## Why Multiple Formats?

Different clients, browsers, and media players have varying support for:
- Opus codec support
- DASH protocol implementation
- HLS protocol implementation
- Container format compatibility

By providing all three formats, we ensure maximum compatibility while recommending the most efficient standard-compliant option first.

## Implementation Notes

### Web Player Implementation

The web player uses HLS for Opus (via hls.js) even though HLS with Opus is not a standard format. This choice was made because:

- **Browser Support**: HLS with Opus works on major modern browsers:
  - Chrome (desktop and mobile)
  - Safari (desktop)
  - Safari (iOS)
- **Practical Solution**: While DASH is the standard, hls.js helps to consolidate endpoints for browser to be HLS only without having to maintain multiple extra protocol support for frontend
- **Library Maturity**: hls.js is a mature, well-maintained library with good Opus handling

### For Non-Web Use Cases

- DASH URL is provided in the UI for external players and reference
- External media players should try DASH first, then HLS, then fall back to `/audio`
- The `/audio` endpoint exists but should be used sparingly due to resource constraints (temporary file creation and duration limits)
