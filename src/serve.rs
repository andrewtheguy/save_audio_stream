use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use log::{error};

#[cfg(not(debug_assertions))]
use axum::response::Response;

#[cfg(debug_assertions)]
use axum::response::Response;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use tower_http::cors::{Any, CorsLayer};

use crate::constants::EXPECTED_DB_VERSION;
use crate::fmp4::{generate_init_segment, generate_media_segment};

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
const INDEX_HTML: &[u8] = include_bytes!("../app/dist/index.html");

/// Parse Opus packets from audio data for fMP4 generation
/// Opus packets are stored as 2-byte little-endian length followed by packet data
fn parse_opus_packets(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut packets = Vec::new();
    let mut pos = 0;

    while pos + 2 <= data.len() {
        // Read 2-byte little-endian length
        let len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;

        if pos + len > data.len() {
            break;
        }

        packets.push(data[pos..pos + len].to_vec());
        pos += len;
    }

    if packets.is_empty() {
        return Err("No valid Opus packets found".to_string());
    }

    Ok(packets)
}

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
const STYLE_CSS: &[u8] = include_bytes!("../app/dist/assets/style.css");

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
const MAIN_JS: &[u8] = include_bytes!("../app/dist/assets/main.js");

// State for audio serving handlers
pub struct AppState {
    pub db_path: PathBuf,
    pub audio_format: String,
    pub immutable: bool,
}

impl AppState {
    /// Open a readonly connection using the appropriate mode based on the immutable flag
    fn open_readonly(&self, path: impl AsRef<std::path::Path>) -> Result<rusqlite::Connection, Box<dyn std::error::Error>> {
        if self.immutable {
            crate::db::open_readonly_connection_immutable(path)
        } else {
            crate::db::open_readonly_connection(path)
        }
    }
}

// serve_for_sync moved to serve_record.rs

/// Inspect a single database file via HTTP server
pub fn inspect_audio(sqlite_file: PathBuf, port: u16, immutable: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Verify database exists and is Opus format
    if !sqlite_file.exists() {
        return Err(format!("Database file not found: {}", sqlite_file.display()).into());
    }

    // Warn if immutable mode is enabled
    if immutable {
        eprintln!("WARNING: Immutable mode enabled. Only use this for databases on read-only media");
        eprintln!("WARNING: or network filesystems. Using immutable mode on databases that can be");
        eprintln!("WARNING: modified will cause SQLITE_CORRUPT errors or incorrect query results.");
        eprintln!("WARNING: See: https://www.sqlite.org/uri.html#uriimmutable");
    }

    let conn = if immutable {
        crate::db::open_readonly_connection_immutable(&sqlite_file)?
    } else {
        crate::db::open_readonly_connection(&sqlite_file)?
    };

    // Check version first
    let db_version: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'version'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| {
            // Preserve the actual error - this could be locking issues, corruption, etc.
            format!("Failed to read version from metadata: {}", e)
        })?;

    if db_version != EXPECTED_DB_VERSION {
        return Err(format!(
            "Unsupported database version: '{}'. This application only supports version '{}'",
            db_version, EXPECTED_DB_VERSION
        )
        .into());
    }

    // Check audio format
    let audio_format: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'audio_format'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| {
            format!("Failed to read audio_format from metadata: {}", e)
        })?;

    if audio_format != "opus" && audio_format != "aac" {
        return Err(format!(
            "Only Opus and AAC formats are supported for serving, found: {}",
            audio_format
        )
        .into());
    }

    println!("Starting server for: {}", sqlite_file.display());
    println!("Audio format: {}", audio_format);
    println!("Listening on: http://[::]:{} (IPv4 + IPv6)", port);
    println!("Endpoints:");
    if audio_format == "opus" {
        println!("  GET /opus-playlist.m3u8?start_id=<N>&end_id=<N>  - HLS/fMP4 playlist");
        println!("  GET /opus-segment/:id.m4s  - fMP4 audio segment");
    } else if audio_format == "aac" {
        println!("  GET /playlist.m3u8?start_id=<N>&end_id=<N>  - HLS playlist");
        println!("  GET /aac-segment/:id.aac  - AAC audio segment");
    } else {
        return Err("Unsupported audio format in database".into());
    }
    println!("  GET /api/sync/shows  - List available shows for syncing");

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_state = StdArc::new(AppState {
            db_path: sqlite_file.clone(),
            audio_format: audio_format.clone(),
            immutable,
        });

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let mut api_routes = Router::new()
            .route("/api/format", get(format_handler))
            .route("/api/segments/range", get(segments_range_handler))
            .route("/api/metadata", get(metadata_handler))
            .route("/api/sessions", get(sessions_handler))
            .route("/api/session/{section_id}/latest", get(session_latest_handler));

        // Add format-specific routes
        if audio_format == "opus" {
            api_routes = api_routes
                .route("/opus-playlist.m3u8", get(opus_hls_playlist_handler))
                .route("/opus-segment/{filename}", get(opus_segment_handler));
        } else if audio_format == "aac" {
            api_routes = api_routes
                .route("/playlist.m3u8", get(hls_playlist_handler))
                .route("/aac-segment/{filename}", get(aac_segment_handler));
        }

        #[cfg(debug_assertions)]
        let app = api_routes
            .route("/", get(index_handler))
            .route("/assets/{*path}", get(vite_assets_handler))
            .route("/src/{*path}", get(vite_src_handler))
            .route("/@vite/client", get(vite_client_handler))
            .route("/@react-refresh", get(vite_react_refresh_handler))
            .route("/@id/{*path}", get(vite_id_handler))
            .route("/node_modules/{*path}", get(vite_node_modules_handler))
            .layer(cors)
            .with_state(app_state);

        #[cfg(not(debug_assertions))]
        let app = api_routes
            .route("/", get(index_handler_release))
            .route("/assets/{*path}", get(assets_handler_release))
            .layer(cors)
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind(format!("[::]:{}", port))
            .await
            .map_err(|e| format!("Failed to bind to port {}: {}", port, e))?;
        axum::serve(listener, app)
            .await
            .map_err(|e| format!("Server error: {}", e))?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}
// Export-related functions moved to serve_record.rs
// (map_to_io_error, write_ogg_stream, export_opus_section, export_aac_section,
// health_handler, ExportSectionPath, ExportResponse, upload_to_sftp,
// spawn_periodic_export_task, to_url_safe_base64, generate_export_filename,
// export_section, export_section_handler)

#[derive(Serialize)]
struct SegmentRange {
    start_id: i64,
    end_id: i64,
}

#[derive(Serialize)]
struct SessionInfo {
    section_id: i64,
    start_id: i64,
    end_id: i64,
    timestamp_ms: i64,
    duration_seconds: f64,
}

#[derive(Serialize)]
struct SessionsResponse {
    name: String,
    sessions: Vec<SessionInfo>,
}

#[derive(Serialize)]
struct Metadata {
    unique_id: String,
    name: String,
    audio_format: String,
    split_interval: String,
    bitrate: String,
    sample_rate: String,
    version: String,
    min_id: i64,
    max_id: i64,
}

// HLS playlist handler for AAC format
async fn hls_playlist_handler(
    State(state): State<StdArc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get metadata
    let sample_rate: u32 = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'sample_rate'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(sr) => sr.parse().unwrap_or(16000),
        Err(e) => {
            error!("Failed to query sample_rate metadata: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    // Determine segment range
    let start_id: i64 = params
        .get("start_id")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let end_id: i64 = if let Some(end_str) = params.get("end_id") {
        end_str.parse().unwrap_or(i64::MAX)
    } else {
        match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to query max segment ID: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
                    .into_response()
            }
        }
    };

    // Query segments using duration_samples (no need to parse audio_data)
    let mut stmt = match conn
        .prepare("SELECT id, duration_samples FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id")
    {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let segments_iter = match stmt.query_map([start_id, end_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    }) {
        Ok(iter) => iter,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    let mut max_duration = 0.0f64;
    let mut segment_durations = Vec::new();

    for segment_result in segments_iter {
        let (seg_id, duration_samples): (i64, i64) = match segment_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: Failed to fetch segment from database: {}", e);
                continue;
            },
        };

        let duration = duration_samples as f64 / sample_rate as f64;
        if duration > max_duration {
            max_duration = duration;
        }

        segment_durations.push((seg_id, duration));
    }

    playlist.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n",
        max_duration.ceil() as u64
    ));

    for (seg_id, duration) in segment_durations {
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!("/aac-segment/{}.aac\n", seg_id));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        playlist,
    )
        .into_response()
}

// AAC segment handler for HLS
async fn aac_segment_handler(
    State(state): State<StdArc<AppState>>,
    Path(filename): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Parse segment ID from filename (strip .aac extension)
    let seg_id: i64 = match filename.strip_suffix(".aac").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => {
            return (StatusCode::BAD_REQUEST, "Invalid segment filename").into_response();
        }
    };

    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    let audio_data: Vec<u8> = match conn.query_row(
        "SELECT audio_data FROM segments WHERE id = ?1",
        [seg_id],
        |row| row.get(0),
    ) {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to query segment {}: {}", seg_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    let total_len = audio_data.len() as u64;

    // Handle Range requests
    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_header.to_str() {
            if let Some(range) = range_str.strip_prefix("bytes=") {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    let start: u64 = parts[0].parse().unwrap_or(0);
                    let end: u64 = if parts[1].is_empty() {
                        total_len - 1
                    } else {
                        parts[1].parse().unwrap_or(total_len - 1).min(total_len - 1)
                    };

                    if start < total_len {
                        let range_data = audio_data[start as usize..=(end as usize)].to_vec();
                        return (
                            StatusCode::PARTIAL_CONTENT,
                            [
                                (header::CONTENT_TYPE, HeaderValue::from_static("audio/aac")),
                                (
                                    header::CONTENT_RANGE,
                                    HeaderValue::from_str(&format!(
                                        "bytes {}-{}/{}",
                                        start, end, total_len
                                    ))
                                    .unwrap(),
                                ),
                                (
                                    header::CONTENT_LENGTH,
                                    HeaderValue::from_str(&(end - start + 1).to_string()).unwrap(),
                                ),
                            ],
                            range_data,
                        )
                            .into_response();
                    }
                }
            }
        }
    }

    // Return full segment
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("audio/aac")),
            (
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&total_len.to_string()).unwrap(),
            ),
        ],
        audio_data,
    )
        .into_response()
}


// HLS playlist handler for Opus format
async fn opus_hls_playlist_handler(
    State(state): State<StdArc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get sample rate (Opus is 48kHz)
    let sample_rate: u32 = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'sample_rate'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(sr) => sr.parse().unwrap_or(48000),
        Err(e) => {
            error!("Failed to query sample_rate metadata: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    // Determine segment range
    let start_id: i64 = params
        .get("start_id")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let end_id: i64 = if let Some(end_str) = params.get("end_id") {
        end_str.parse().unwrap_or(i64::MAX)
    } else {
        match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to query max segment ID: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
                    .into_response()
            }
        }
    };

    // Query segments using duration_samples (no need to parse audio_data)
    let mut stmt = match conn
        .prepare("SELECT id, duration_samples FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id")
    {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let segments_iter = match stmt.query_map([start_id, end_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    }) {
        Ok(iter) => iter,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    let mut max_duration = 0.0f64;
    let mut segment_durations = Vec::new();

    for segment_result in segments_iter {
        let (seg_id, duration_samples): (i64, i64) = match segment_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: Failed to fetch segment from database: {}", e);
                continue;
            },
        };

        let duration = duration_samples as f64 / sample_rate as f64;
        if duration > max_duration {
            max_duration = duration;
        }

        segment_durations.push((seg_id, duration));
    }

    playlist.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", start_id));
    playlist.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    playlist.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n",
        max_duration.ceil() as u64
    ));
    playlist.push_str("#EXT-X-MAP:URI=\"/opus-segment/init.mp4\"\n");

    for (seg_id, duration) in segment_durations {
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!("/opus-segment/{}.m4s\n", seg_id));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        playlist,
    )
        .into_response()
}

// Opus fMP4 segment handler for HLS
async fn opus_segment_handler(
    State(state): State<StdArc<AppState>>,
    Path(filename): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Handle init segment request
    if filename == "init.mp4" {
        let timescale = 48000u32;
        let track_id = 1u32;
        let channel_count = 1u16; // Mono
        let sample_rate = 48000u32;

        let init_segment =
            match generate_init_segment(timescale, track_id, channel_count, sample_rate) {
                Ok(data) => data,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to generate init segment: {}", e),
                    )
                        .into_response()
                }
            };

        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4")),
                (
                    header::CONTENT_LENGTH,
                    HeaderValue::from_str(&init_segment.len().to_string()).unwrap(),
                ),
            ],
            init_segment,
        )
            .into_response();
    }

    // Parse segment ID from filename (strip .m4s extension)
    let seg_id: i64 = match filename.strip_suffix(".m4s").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => {
            return (StatusCode::BAD_REQUEST, "Invalid segment filename").into_response();
        }
    };

    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    let audio_data: Vec<u8> = match conn.query_row(
        "SELECT audio_data FROM segments WHERE id = ?1",
        [seg_id],
        |row| row.get(0),
    ) {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to query segment {}: {}", seg_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    // Parse Opus packets
    let opus_packets = match parse_opus_packets(&audio_data) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to parse Opus packets: {}", e),
            )
                .into_response()
        }
    };

    // Generate fMP4 media segment
    let timescale = 48000u32;
    let track_id = 1u32;
    let samples_per_packet = 960u32; // 20ms at 48kHz

    // Calculate base media decode time (for simplicity, use segment_id * average_duration)
    // In a real implementation, you might want to track cumulative time
    let base_media_decode_time =
        ((seg_id - 1) as u64) * (opus_packets.len() as u64 * samples_per_packet as u64);

    let media_segment = match generate_media_segment(
        seg_id as u32,
        track_id,
        base_media_decode_time,
        &opus_packets,
        timescale,
        samples_per_packet,
    ) {
        Ok(data) => data,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to generate media segment: {}", e),
            )
                .into_response()
        }
    };

    let total_len = media_segment.len() as u64;

    // Handle Range requests
    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_header.to_str() {
            if let Some(range) = range_str.strip_prefix("bytes=") {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    let start: u64 = parts[0].parse().unwrap_or(0);
                    let end: u64 = if parts[1].is_empty() {
                        total_len - 1
                    } else {
                        parts[1].parse().unwrap_or(total_len - 1).min(total_len - 1)
                    };

                    if start < total_len {
                        let range_data = media_segment[start as usize..=(end as usize)].to_vec();
                        return (
                            StatusCode::PARTIAL_CONTENT,
                            [
                                (header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4")),
                                (
                                    header::CONTENT_RANGE,
                                    HeaderValue::from_str(&format!(
                                        "bytes {}-{}/{}",
                                        start, end, total_len
                                    ))
                                    .unwrap(),
                                ),
                                (
                                    header::CONTENT_LENGTH,
                                    HeaderValue::from_str(&(end - start + 1).to_string()).unwrap(),
                                ),
                            ],
                            range_data,
                        )
                            .into_response();
                    }
                }
            }
        }
    }

    // Return full segment
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4")),
            (
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&total_len.to_string()).unwrap(),
            ),
        ],
        media_segment,
    )
        .into_response()
}

#[derive(Serialize)]
struct FormatResponse {
    format: String,
}

async fn format_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&FormatResponse {
            format: state.audio_format.clone(),
        })
        .unwrap(),
    )
}

async fn segments_range_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    let min_id: Result<i64, _> =
        conn.query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0));
    let max_id: Result<i64, _> =
        conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0));

    match (min_id, max_id) {
        (Ok(min), Ok(max)) => {
            let range = SegmentRange {
                start_id: min,
                end_id: max,
            };
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&range).unwrap(),
            )
                .into_response()
        }
        _ => (StatusCode::NOT_FOUND, "No segments found in database").into_response(),
    }
}

async fn metadata_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response()
        }
    };

    // Query all metadata fields
    let unique_id: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'unique_id'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query unique_id metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let name: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'name'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query name metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let audio_format: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'audio_format'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query audio_format metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let split_interval: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'split_interval'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query split_interval metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let bitrate: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'bitrate'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query bitrate metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let sample_rate: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'sample_rate'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query sample_rate metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let version: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'version'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query version metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    // Get min/max segment IDs
    let (min_id, max_id): (i64, i64) = match conn.query_row(
        "SELECT MIN(id), MAX(id) FROM segments",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query min/max segment IDs: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    let metadata = Metadata {
        unique_id,
        name,
        audio_format,
        split_interval,
        bitrate,
        sample_rate,
        version,
        min_id,
        max_id,
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&metadata).unwrap(),
    )
        .into_response()
}

async fn sessions_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get show name from metadata
    let name: String =
        match conn.query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
            row.get(0)
        }) {
            Ok(n) => n,
            Err(e) => {
                error!("Failed to query name metadata: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
            }
        };

    // Get split interval
    let split_interval: f64 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'split_interval'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(10.0))
            },
        )
        .unwrap_or(10.0);

    // Session Boundary Detection using is_timestamp_from_source
    //
    // The is_timestamp_from_source flag (set to 1) marks the first segment of each
    // HTTP connection. Each connection gets its timestamp from the HTTP Date header,
    // creating natural boundaries between recording sessions.
    //
    // This enables accurate detection of which segments are contiguous (from the same
    // connection) vs. which come from different recording attempts after reconnection
    // or schedule breaks.

    // Get all sections with their start id and timestamp from sections table
    let mut stmt = match conn.prepare(
        "SELECT s.id, s.start_timestamp_ms, MIN(seg.id) as start_segment_id
         FROM sections s
         JOIN segments seg ON seg.section_id = s.id
         GROUP BY s.id
         ORDER BY s.id",
    ) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let boundaries: Vec<(i64, i64, i64)> =
        match stmt.query_map([], |row| Ok((row.get(0)?, row.get(2)?, row.get(1)?))) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Query error: {}", e),
                )
                    .into_response()
            }
        };

    if boundaries.is_empty() {
        return (StatusCode::NOT_FOUND, "No recording sessions found").into_response();
    }

    // Get max segment ID to handle the last session
    let max_id: i64 = match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to query max segment ID: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Build sessions by grouping segments between boundaries
    let mut sessions = Vec::new();
    for i in 0..boundaries.len() {
        let (section_id, start_id, timestamp_ms) = boundaries[i];
        let end_id = if i + 1 < boundaries.len() {
            boundaries[i + 1].1 - 1
        } else {
            max_id
        };

        let segment_count = (end_id - start_id + 1) as f64;
        let duration_seconds = segment_count * split_interval;

        sessions.push(SessionInfo {
            section_id,
            start_id,
            end_id,
            timestamp_ms,
            duration_seconds,
        });
    }

    let response = SessionsResponse { name, sessions };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&response).unwrap(),
    )
        .into_response()
}

#[derive(Serialize)]
struct SessionLatestResponse {
    section_id: i64,
    current_end_id: i64,
    segment_count: i64,
}

async fn session_latest_handler(
    State(state): State<StdArc<AppState>>,
    Path(section_id): Path<i64>,
) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response();
        }
    };

    // Get the max segment ID for this section
    let result: Result<(i64, i64), _> = conn.query_row(
        "SELECT MAX(id), COUNT(id) FROM segments WHERE section_id = ?1",
        [section_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    );

    match result {
        Ok((max_id, count)) => {
            let response = SessionLatestResponse {
                section_id,
                current_end_id: max_id,
                segment_count: count,
            };
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&response).unwrap(),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to query latest segment for section {}: {}", section_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Database error: {}", e)})).unwrap(),
            )
                .into_response()
        }
    }
}

#[cfg(debug_assertions)]
async fn proxy_to_vite(path: &str) -> Response {
    const VITE_DEV_SERVER: &str = "http://localhost:21173";
    let vite_url = format!("{}{}", VITE_DEV_SERVER, path);

    match reqwest::get(&vite_url).await {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let headers = resp.headers().clone();

            match resp.bytes().await {
                Ok(body) => {
                    let mut response = Response::new(Body::from(body));

                    if let Ok(status) = StatusCode::from_u16(status_code) {
                        *response.status_mut() = status;
                    }

                    for (name, value) in headers.iter() {
                        let name_str = name.as_str();
                        if name_str != "transfer-encoding" {
                            if let Ok(value_str) = value.to_str() {
                                if let Ok(header_value) = HeaderValue::from_str(value_str) {
                                    if let Ok(header_name) = header::HeaderName::from_bytes(name_str.as_bytes()) {
                                        response.headers_mut().insert(header_name, header_value);
                                    }
                                }
                            }
                        }
                    }

                    response
                }
                Err(e) => {
                    warn!("Failed to read response from dev server: {}", e);
                    (StatusCode::BAD_GATEWAY, "Failed to read response from dev server").into_response()
                }
            }
        }
        Err(e) => {
            warn!("Failed to connect to dev server at {}: {}", VITE_DEV_SERVER, e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to connect to dev server at {}. Make sure to run 'deno task dev' in the app/ directory.", VITE_DEV_SERVER)
            ).into_response()
        }
    }
}

#[cfg(debug_assertions)]
async fn index_handler() -> Response {
    proxy_to_vite("/").await
}

#[cfg(debug_assertions)]
async fn vite_assets_handler(Path(path): Path<String>) -> Response {
    proxy_to_vite(&format!("/assets/{}", path)).await
}

#[cfg(debug_assertions)]
async fn vite_src_handler(Path(path): Path<String>) -> Response {
    proxy_to_vite(&format!("/src/{}", path)).await
}

#[cfg(debug_assertions)]
async fn vite_client_handler() -> Response {
    proxy_to_vite("/@vite/client").await
}

#[cfg(debug_assertions)]
async fn vite_react_refresh_handler() -> Response {
    proxy_to_vite("/@react-refresh").await
}

#[cfg(debug_assertions)]
async fn vite_id_handler(Path(path): Path<String>) -> Response {
    proxy_to_vite(&format!("/@id/{}", path)).await
}

#[cfg(debug_assertions)]
async fn vite_node_modules_handler(Path(path): Path<String>) -> Response {
    proxy_to_vite(&format!("/node_modules/{}", path)).await
}

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
async fn index_handler_release() -> Response {
    let mut response = Response::new(Body::from(INDEX_HTML));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
    response
}

#[cfg(all(not(debug_assertions), not(feature = "web-frontend")))]
async fn index_handler_release() -> Response {
    (
        StatusCode::NOT_FOUND,
        "Web frontend not available in this build",
    )
        .into_response()
}

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
async fn assets_handler_release(Path(path): Path<String>) -> Response {
    let (content, mime_type): (&[u8], &str) = match path.as_str() {
        "style.css" => (STYLE_CSS, "text/css"),
        "main.js" => (MAIN_JS, "application/javascript"),
        _ => {
            return (StatusCode::NOT_FOUND, "Asset not found").into_response();
        }
    };

    let mut response = Response::new(Body::from(content));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime_type));
    response
}

#[cfg(all(not(debug_assertions), not(feature = "web-frontend")))]
async fn assets_handler_release(Path(_path): Path<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        "Web frontend not available in this build",
    )
        .into_response()
}

// Sync handlers moved to serve_record.rs

// ============================================================================
// Receiver Mode - Multi-show frontend with background sync
// ============================================================================

use std::sync::atomic::{AtomicBool, Ordering};
use axum::routing::post;
use serde::Deserialize;

/// State for receiver mode (multi-show with background sync)
pub struct ReceiverAppState {
    pub local_dir: PathBuf,
    pub remote_url: String,
    pub shows_filter: Option<Vec<String>>,
    pub chunk_size: u64,
    pub sync_interval_seconds: u64,
    pub sync_in_progress: StdArc<AtomicBool>,
}

/// Receiver mode: serve frontend with show selection and background sync
pub fn receiver_audio(config: crate::config::SyncConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Create local_dir if it doesn't exist
    if !config.local_dir.exists() {
        std::fs::create_dir_all(&config.local_dir)?;
    }

    let port = config.port;
    let sync_in_progress = StdArc::new(AtomicBool::new(false));

    println!("Starting receiver server...");
    println!("Local directory: {}", config.local_dir.display());
    println!("Remote URL: {}", config.remote_url);
    println!("Sync interval: {} seconds", config.sync_interval_seconds);
    println!("Listening on: http://[::]:{} (IPv4 + IPv6)", port);

    // Clone values for background sync thread
    let sync_remote_url = config.remote_url.clone();
    let sync_local_dir = config.local_dir.clone();
    let sync_shows = config.shows.clone();
    let sync_chunk_size = config.chunk_size.unwrap_or(100);
    let sync_interval = config.sync_interval_seconds;
    let sync_flag = sync_in_progress.clone();

    // Spawn background sync thread with continuous polling
    std::thread::spawn(move || {
        loop {
            if !sync_flag.swap(true, Ordering::SeqCst) {
                println!("[Sync] Starting background sync...");
                match crate::sync::sync_shows(
                    sync_remote_url.clone(),
                    sync_local_dir.clone(),
                    sync_shows.clone(),
                    sync_chunk_size,
                ) {
                    Ok(_) => println!("[Sync] Background sync completed successfully"),
                    Err(e) => eprintln!("[Sync] Background sync error: {}", e),
                }
                sync_flag.store(false, Ordering::SeqCst);
            } else {
                println!("[Sync] Sync already in progress, skipping this interval");
            }
            std::thread::sleep(std::time::Duration::from_secs(sync_interval));
        }
    });

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_state = StdArc::new(ReceiverAppState {
            local_dir: config.local_dir.clone(),
            remote_url: config.remote_url.clone(),
            shows_filter: config.shows.clone(),
            chunk_size: config.chunk_size.unwrap_or(100),
            sync_interval_seconds: config.sync_interval_seconds,
            sync_in_progress,
        });

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        // API routes for receiver mode
        let api_routes = Router::new()
            // Show listing and selection
            .route("/api/shows", get(receiver_shows_handler))
            .route("/api/mode", get(receiver_mode_handler))
            // Per-show routes
            .route("/api/show/{show_name}/format", get(receiver_show_format_handler))
            .route("/api/show/{show_name}/sessions", get(receiver_show_sessions_handler))
            .route("/api/show/{show_name}/metadata", get(receiver_show_metadata_handler))
            .route("/api/show/{show_name}/segments/range", get(receiver_show_segments_range_handler))
            // HLS routes for selected show
            .route("/show/{show_name}/opus-playlist.m3u8", get(receiver_opus_playlist_handler))
            .route("/show/{show_name}/opus-segment/{filename}", get(receiver_opus_segment_handler))
            .route("/show/{show_name}/playlist.m3u8", get(receiver_aac_playlist_handler))
            .route("/show/{show_name}/aac-segment/{filename}", get(receiver_aac_segment_handler))
            // Sync control
            .route("/api/sync", post(receiver_trigger_sync_handler))
            .route("/api/sync/status", get(receiver_sync_status_handler));

        #[cfg(debug_assertions)]
        let app = api_routes
            .route("/", get(index_handler))
            .route("/assets/{*path}", get(vite_assets_handler))
            .route("/src/{*path}", get(vite_src_handler))
            .route("/@vite/client", get(vite_client_handler))
            .route("/@react-refresh", get(vite_react_refresh_handler))
            .route("/@id/{*path}", get(vite_id_handler))
            .route("/node_modules/{*path}", get(vite_node_modules_handler))
            .layer(cors)
            .with_state(app_state);

        #[cfg(not(debug_assertions))]
        let app = api_routes
            .route("/", get(receiver_index_handler_release))
            .route("/assets/{*path}", get(receiver_assets_handler_release))
            .layer(cors)
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind(format!("[::]:{}", port))
            .await
            .map_err(|e| format!("Failed to bind to port {}: {}", port, e))?;
        axum::serve(listener, app)
            .await
            .map_err(|e| format!("Server error: {}", e))?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

// ============================================================================
// Receiver Mode Handlers
// ============================================================================

#[derive(Serialize)]
struct ReceiverShowInfo {
    name: String,
    audio_format: Option<String>,
}

#[derive(Serialize)]
struct ReceiverShowsResponse {
    shows: Vec<ReceiverShowInfo>,
}

#[derive(Serialize)]
struct ReceiverModeResponse {
    mode: String,
}

async fn receiver_mode_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&ReceiverModeResponse {
            mode: "receiver".to_string(),
        })
        .unwrap(),
    )
}

async fn receiver_shows_handler(
    State(state): State<StdArc<ReceiverAppState>>,
) -> impl IntoResponse {
    // Scan local_dir for .sqlite files
    let mut shows = Vec::new();

    let entries = match std::fs::read_dir(&state.local_dir) {
        Ok(e) => e,
        Err(e) => {
            error!("Failed to read local directory: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&serde_json::json!({"error": format!("Failed to read directory: {}", e)})).unwrap(),
            ).into_response();
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "sqlite") {
            let show_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Filter by shows_filter if present
            if let Some(ref filter) = state.shows_filter {
                if !filter.contains(&show_name) {
                    continue;
                }
            }

            // Try to get audio format from database
            let audio_format = if let Ok(conn) = crate::db::open_readonly_connection(&path) {
                conn.query_row(
                    "SELECT value FROM metadata WHERE key = 'audio_format'",
                    [],
                    |row| row.get(0),
                )
                .ok()
            } else {
                None
            };

            shows.push(ReceiverShowInfo {
                name: show_name,
                audio_format,
            });
        }
    }

    // Sort shows by name
    shows.sort_by(|a, b| a.name.cmp(&b.name));

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&ReceiverShowsResponse { shows }).unwrap(),
    )
        .into_response()
}

fn get_show_db_path(state: &ReceiverAppState, show_name: &str) -> PathBuf {
    state.local_dir.join(format!("{}.sqlite", show_name))
}

async fn receiver_show_format_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    let audio_format: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'audio_format'",
        [],
        |row| row.get(0),
    ) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to query audio_format for show '{}': {}", show_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&FormatResponse { format: audio_format }).unwrap(),
    )
        .into_response()
}

async fn receiver_show_sessions_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    // Get show name from metadata
    let name: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'name'",
        [],
        |row| row.get(0),
    ) {
        Ok(n) => n,
        Err(e) => {
            error!("Failed to query name metadata: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    // Get split interval
    let split_interval: f64 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'split_interval'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(10.0))
            },
        )
        .unwrap_or(10.0);

    // Get all sections with their start id and timestamp
    let mut stmt = match conn.prepare(
        "SELECT s.id, s.start_timestamp_ms, MIN(seg.id) as start_segment_id
         FROM sections s
         JOIN segments seg ON seg.section_id = s.id
         GROUP BY s.id
         ORDER BY s.id",
    ) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response();
        }
    };

    let boundaries: Vec<(i64, i64, i64)> =
        match stmt.query_map([], |row| Ok((row.get(0)?, row.get(2)?, row.get(1)?))) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response();
            }
        };

    if boundaries.is_empty() {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&SessionsResponse { name, sessions: vec![] }).unwrap(),
        ).into_response();
    }

    let max_id: i64 = match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to query max segment ID: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    let mut sessions = Vec::new();
    for i in 0..boundaries.len() {
        let (section_id, start_id, timestamp_ms) = boundaries[i];
        let end_id = if i + 1 < boundaries.len() {
            boundaries[i + 1].1 - 1
        } else {
            max_id
        };

        let segment_count = (end_id - start_id + 1) as f64;
        let duration_seconds = segment_count * split_interval;

        sessions.push(SessionInfo {
            section_id,
            start_id,
            end_id,
            timestamp_ms,
            duration_seconds,
        });
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&SessionsResponse { name, sessions }).unwrap(),
    )
        .into_response()
}

async fn receiver_show_metadata_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    // Query all metadata fields
    let unique_id: String = conn.query_row("SELECT value FROM metadata WHERE key = 'unique_id'", [], |row| row.get(0)).unwrap_or_default();
    let name: String = conn.query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| row.get(0)).unwrap_or_default();
    let audio_format: String = conn.query_row("SELECT value FROM metadata WHERE key = 'audio_format'", [], |row| row.get(0)).unwrap_or_default();
    let split_interval: String = conn.query_row("SELECT value FROM metadata WHERE key = 'split_interval'", [], |row| row.get(0)).unwrap_or_default();
    let bitrate: String = conn.query_row("SELECT value FROM metadata WHERE key = 'bitrate'", [], |row| row.get(0)).unwrap_or_default();
    let sample_rate: String = conn.query_row("SELECT value FROM metadata WHERE key = 'sample_rate'", [], |row| row.get(0)).unwrap_or_default();
    let version: String = conn.query_row("SELECT value FROM metadata WHERE key = 'version'", [], |row| row.get(0)).unwrap_or_default();

    let (min_id, max_id): (i64, i64) = conn
        .query_row("SELECT MIN(id), MAX(id) FROM segments", [], |row| Ok((row.get(0).unwrap_or(0), row.get(1).unwrap_or(0))))
        .unwrap_or((0, 0));

    let metadata = Metadata {
        unique_id,
        name,
        audio_format,
        split_interval,
        bitrate,
        sample_rate,
        version,
        min_id,
        max_id,
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&metadata).unwrap(),
    )
        .into_response()
}

async fn receiver_show_segments_range_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    let min_id: Result<i64, _> = conn.query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0));
    let max_id: Result<i64, _> = conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0));

    match (min_id, max_id) {
        (Ok(min), Ok(max)) => {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&SegmentRange { start_id: min, end_id: max }).unwrap(),
            )
                .into_response()
        }
        _ => (StatusCode::NOT_FOUND, "No segments found").into_response(),
    }
}

// HLS handlers for receiver mode

#[derive(Deserialize)]
struct ReceiverPlaylistParams {
    start_id: Option<i64>,
    end_id: Option<i64>,
}

async fn receiver_opus_playlist_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
    Query(params): Query<ReceiverPlaylistParams>,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response();
        }
    };

    let sample_rate: u32 = conn
        .query_row("SELECT value FROM metadata WHERE key = 'sample_rate'", [], |row| row.get::<_, String>(0))
        .map(|s| s.parse().unwrap_or(48000))
        .unwrap_or(48000);

    let start_id = params.start_id.unwrap_or(1);
    let end_id = params.end_id.unwrap_or_else(|| {
        conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)).unwrap_or(i64::MAX)
    });

    let mut stmt = match conn.prepare("SELECT id, duration_samples FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id") {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response(),
    };

    let segments: Vec<(i64, i64)> = match stmt.query_map([start_id, end_id], |row| Ok((row.get(0)?, row.get(1)?))) {
        Ok(iter) => iter.filter_map(Result::ok).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response(),
    };

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    let max_duration: f64 = segments.iter().map(|(_, d)| *d as f64 / sample_rate as f64).fold(0.0, f64::max);

    playlist.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", start_id));
    playlist.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", max_duration.ceil() as u64));
    playlist.push_str(&format!("#EXT-X-MAP:URI=\"/show/{}/opus-segment/init.mp4\"\n", show_name));

    for (seg_id, duration_samples) in segments {
        let duration = duration_samples as f64 / sample_rate as f64;
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!("/show/{}/opus-segment/{}.m4s\n", show_name, seg_id));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");

    (StatusCode::OK, [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")], playlist).into_response()
}

async fn receiver_opus_segment_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path((show_name, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    // Handle init segment
    if filename == "init.mp4" {
        let init_segment = match generate_init_segment(48000, 1, 1, 48000) {
            Ok(data) => data,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate init segment: {}", e)).into_response(),
        };
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4")),
             (header::CONTENT_LENGTH, HeaderValue::from_str(&init_segment.len().to_string()).unwrap())],
            init_segment,
        ).into_response();
    }

    let seg_id: i64 = match filename.strip_suffix(".m4s").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "Invalid segment filename").into_response(),
    };

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response(),
    };

    let audio_data: Vec<u8> = match conn.query_row("SELECT audio_data FROM segments WHERE id = ?1", [seg_id], |row| row.get(0)) {
        Ok(data) => data,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response(),
    };

    let opus_packets = match parse_opus_packets(&audio_data) {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to parse Opus packets: {}", e)).into_response(),
    };

    let base_media_decode_time = ((seg_id - 1) as u64) * (opus_packets.len() as u64 * 960);
    let media_segment = match generate_media_segment(seg_id as u32, 1, base_media_decode_time, &opus_packets, 48000, 960) {
        Ok(data) => data,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate media segment: {}", e)).into_response(),
    };

    let total_len = media_segment.len() as u64;

    // Handle Range requests
    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_header.to_str() {
            if let Some(range) = range_str.strip_prefix("bytes=") {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    let start: u64 = parts[0].parse().unwrap_or(0);
                    let end: u64 = if parts[1].is_empty() { total_len - 1 } else { parts[1].parse().unwrap_or(total_len - 1).min(total_len - 1) };
                    if start < total_len {
                        let range_data = media_segment[start as usize..=(end as usize)].to_vec();
                        return (
                            StatusCode::PARTIAL_CONTENT,
                            [(header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4")),
                             (header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, total_len)).unwrap()),
                             (header::CONTENT_LENGTH, HeaderValue::from_str(&(end - start + 1).to_string()).unwrap())],
                            range_data,
                        ).into_response();
                    }
                }
            }
        }
    }

    (StatusCode::OK, [(header::CONTENT_TYPE, HeaderValue::from_static("audio/mp4")), (header::CONTENT_LENGTH, HeaderValue::from_str(&total_len.to_string()).unwrap())], media_segment).into_response()
}

async fn receiver_aac_playlist_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
    Query(params): Query<ReceiverPlaylistParams>,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response(),
    };

    let sample_rate: u32 = conn
        .query_row("SELECT value FROM metadata WHERE key = 'sample_rate'", [], |row| row.get::<_, String>(0))
        .map(|s| s.parse().unwrap_or(16000))
        .unwrap_or(16000);

    let start_id = params.start_id.unwrap_or(1);
    let end_id = params.end_id.unwrap_or_else(|| {
        conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)).unwrap_or(i64::MAX)
    });

    let mut stmt = match conn.prepare("SELECT id, duration_samples FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id") {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response(),
    };

    let segments: Vec<(i64, i64)> = match stmt.query_map([start_id, end_id], |row| Ok((row.get(0)?, row.get(1)?))) {
        Ok(iter) => iter.filter_map(Result::ok).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response(),
    };

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    let max_duration: f64 = segments.iter().map(|(_, d)| *d as f64 / sample_rate as f64).fold(0.0, f64::max);

    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", max_duration.ceil() as u64));

    for (seg_id, duration_samples) in segments {
        let duration = duration_samples as f64 / sample_rate as f64;
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!("/show/{}/aac-segment/{}.aac\n", show_name, seg_id));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");

    (StatusCode::OK, [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")], playlist).into_response()
}

async fn receiver_aac_segment_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path((show_name, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let db_path = get_show_db_path(&state, &show_name);

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, "Show not found").into_response();
    }

    let seg_id: i64 = match filename.strip_suffix(".aac").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "Invalid segment filename").into_response(),
    };

    let conn = match crate::db::open_readonly_connection(&db_path) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response(),
    };

    let audio_data: Vec<u8> = match conn.query_row("SELECT audio_data FROM segments WHERE id = ?1", [seg_id], |row| row.get(0)) {
        Ok(data) => data,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response(),
    };

    let total_len = audio_data.len() as u64;

    // Handle Range requests
    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_header.to_str() {
            if let Some(range) = range_str.strip_prefix("bytes=") {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    let start: u64 = parts[0].parse().unwrap_or(0);
                    let end: u64 = if parts[1].is_empty() { total_len - 1 } else { parts[1].parse().unwrap_or(total_len - 1).min(total_len - 1) };
                    if start < total_len {
                        let range_data = audio_data[start as usize..=(end as usize)].to_vec();
                        return (
                            StatusCode::PARTIAL_CONTENT,
                            [(header::CONTENT_TYPE, HeaderValue::from_static("audio/aac")),
                             (header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, total_len)).unwrap()),
                             (header::CONTENT_LENGTH, HeaderValue::from_str(&(end - start + 1).to_string()).unwrap())],
                            range_data,
                        ).into_response();
                    }
                }
            }
        }
    }

    (StatusCode::OK, [(header::CONTENT_TYPE, HeaderValue::from_static("audio/aac")), (header::CONTENT_LENGTH, HeaderValue::from_str(&total_len.to_string()).unwrap())], audio_data).into_response()
}

// Sync control handlers

#[derive(Serialize)]
struct SyncStatusResponse {
    in_progress: bool,
}

async fn receiver_sync_status_handler(
    State(state): State<StdArc<ReceiverAppState>>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&SyncStatusResponse {
            in_progress: state.sync_in_progress.load(Ordering::SeqCst),
        })
        .unwrap(),
    )
}

#[derive(Serialize)]
struct SyncTriggerResponse {
    message: String,
    already_in_progress: bool,
}

async fn receiver_trigger_sync_handler(
    State(state): State<StdArc<ReceiverAppState>>,
) -> impl IntoResponse {
    if state.sync_in_progress.load(Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&SyncTriggerResponse {
                message: "Sync already in progress".to_string(),
                already_in_progress: true,
            })
            .unwrap(),
        )
            .into_response();
    }

    // Spawn a new sync in a separate thread
    let remote_url = state.remote_url.clone();
    let local_dir = state.local_dir.clone();
    let shows = state.shows_filter.clone();
    let chunk_size = state.chunk_size;
    let sync_flag = state.sync_in_progress.clone();

    std::thread::spawn(move || {
        sync_flag.store(true, Ordering::SeqCst);
        println!("[Sync] Manual sync triggered...");
        match crate::sync::sync_shows(remote_url, local_dir, shows, chunk_size) {
            Ok(_) => println!("[Sync] Manual sync completed successfully"),
            Err(e) => eprintln!("[Sync] Manual sync error: {}", e),
        }
        sync_flag.store(false, Ordering::SeqCst);
    });

    (
        StatusCode::ACCEPTED,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&SyncTriggerResponse {
            message: "Sync started".to_string(),
            already_in_progress: false,
        })
        .unwrap(),
    )
        .into_response()
}

// Release mode handlers for receiver (reuse the same embedded assets)
#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
async fn receiver_index_handler_release() -> Response {
    let mut response = Response::new(Body::from(INDEX_HTML));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
    response
}

#[cfg(all(not(debug_assertions), not(feature = "web-frontend")))]
async fn receiver_index_handler_release() -> Response {
    (StatusCode::NOT_FOUND, "Web frontend not available in this build").into_response()
}

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
async fn receiver_assets_handler_release(Path(path): Path<String>) -> Response {
    let (content, mime_type): (&[u8], &str) = match path.as_str() {
        "style.css" => (STYLE_CSS, "text/css"),
        "main.js" => (MAIN_JS, "application/javascript"),
        _ => return (StatusCode::NOT_FOUND, "Asset not found").into_response(),
    };
    let mut response = Response::new(Body::from(content));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime_type));
    response
}

#[cfg(all(not(debug_assertions), not(feature = "web-frontend")))]
async fn receiver_assets_handler_release(Path(_path): Path<String>) -> Response {
    (StatusCode::NOT_FOUND, "Web frontend not available in this build").into_response()
}
