use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use log::error;
#[cfg(debug_assertions)]
use log::warn;

#[cfg(not(debug_assertions))]
use axum::response::Response;

#[cfg(debug_assertions)]
use axum::response::Response;
use serde::Serialize;
use sqlx::postgres::PgPool;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use tower_http::cors::{Any, CorsLayer};

use crate::constants::EXPECTED_DB_VERSION;
use crate::fmp4::{generate_init_segment, generate_media_segment};
use crate::queries::{metadata, segments};

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
    pub pool: SqlitePool,
}

// serve_for_sync moved to serve_record.rs

/// Inspect a single database file via HTTP server
pub fn inspect_audio(
    sqlite_file: PathBuf,
    port: u16,
    immutable: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Verify database exists and is Opus format
    if !sqlite_file.exists() {
        return Err(format!("Database file not found: {}", sqlite_file.display()).into());
    }

    // Warn if immutable mode is enabled
    if immutable {
        eprintln!(
            "WARNING: Immutable mode enabled. Only use this for databases on read-only media"
        );
        eprintln!("WARNING: or network filesystems. Using immutable mode on databases that can be");
        eprintln!("WARNING: modified will cause SQLITE_CORRUPT errors or incorrect query results.");
        eprintln!("WARNING: See: https://www.sqlite.org/uri.html#uriimmutable");
    }

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Open the database pool
        let pool = if immutable {
            crate::db::open_readonly_connection_immutable(&sqlite_file).await?
        } else {
            crate::db::open_readonly_connection(&sqlite_file).await?
        };

        // Check version first
        let sql = metadata::select_by_key("version");
        let db_version: String = sqlx::query_scalar(&sql)
            .fetch_one(&pool)
            .await
            .map_err(|e| format!("Failed to read version from metadata: {}", e))?;

        if db_version != EXPECTED_DB_VERSION {
            return Err(format!(
                "Unsupported database version: '{}'. This application only supports version '{}'",
                db_version, EXPECTED_DB_VERSION
            )
            .into());
        }

        // Check audio format
        let sql = metadata::select_by_key("audio_format");
        let audio_format: String = sqlx::query_scalar(&sql)
            .fetch_one(&pool)
            .await
            .map_err(|e| format!("Failed to read audio_format from metadata: {}", e))?;

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

        let app_state = StdArc::new(AppState {
            db_path: sqlite_file.clone(),
            audio_format: audio_format.clone(),
            pool,
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
            .route(
                "/api/session/{section_id}/latest",
                get(session_latest_handler),
            );

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

        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
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
    let pool = &state.pool;

    // Get metadata
    let sql = metadata::select_by_key("sample_rate");
    let sample_rate: u32 = match sqlx::query_scalar::<_, String>(&sql).fetch_one(pool).await {
        Ok(sr) => sr.parse().unwrap_or(16000),
        Err(e) => {
            error!("Failed to query sample_rate metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
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
        let sql = segments::select_max_id();
        match sqlx::query_scalar::<_, i64>(&sql).fetch_one(pool).await {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to query max segment ID: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
                    .into_response();
            }
        }
    };

    // Query segments using duration_samples
    let sql = segments::select_range_for_playlist(start_id, end_id);
    let rows = match sqlx::query(&sql).fetch_all(pool).await {
        Ok(r) => r,
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

    for row in rows {
        let seg_id: i64 = row.get(0);
        let duration_samples: i64 = row.get(1);

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

    let pool = &state.pool;

    let sql = segments::select_audio_by_id(seg_id);
    let audio_data: Vec<u8> = match sqlx::query_scalar(&sql).fetch_one(pool).await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to query segment {}: {}", seg_id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
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
    let pool = &state.pool;

    // Get sample rate (Opus is 48kHz)
    let sql = metadata::select_by_key("sample_rate");
    let sample_rate: u32 = match sqlx::query_scalar::<_, String>(&sql).fetch_one(pool).await {
        Ok(sr) => sr.parse().unwrap_or(48000),
        Err(e) => {
            error!("Failed to query sample_rate metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
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
        let sql = segments::select_max_id();
        match sqlx::query_scalar::<_, i64>(&sql).fetch_one(pool).await {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to query max segment ID: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
                    .into_response();
            }
        }
    };

    // Query segments using duration_samples
    let sql = segments::select_range_for_playlist(start_id, end_id);
    let rows = match sqlx::query(&sql).fetch_all(pool).await {
        Ok(r) => r,
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

    for row in rows {
        let seg_id: i64 = row.get(0);
        let duration_samples: i64 = row.get(1);

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

    let pool = &state.pool;

    let sql = segments::select_audio_by_id(seg_id);
    let audio_data: Vec<u8> = match sqlx::query_scalar(&sql).fetch_one(pool).await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to query segment {}: {}", seg_id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
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
    let pool = &state.pool;

    let sql = segments::select_min_max_id();
    let result = sqlx::query(&sql).fetch_optional(pool).await;

    match result {
        Ok(Some(row)) => {
            let min_id: Option<i64> = row.get(0);
            let max_id: Option<i64> = row.get(1);
            match (min_id, max_id) {
                (Some(min), Some(max)) => {
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
        Ok(None) => (StatusCode::NOT_FOUND, "No segments found in database").into_response(),
        Err(e) => {
            error!("Failed to query segment range: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    }
}

async fn metadata_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let pool = &state.pool;

    // Helper to fetch metadata value
    async fn get_meta(pool: &SqlitePool, key: &str) -> Result<String, sqlx::Error> {
        let sql = metadata::select_by_key(key);
        sqlx::query_scalar(&sql).fetch_one(pool).await
    }

    // Query all metadata fields
    let unique_id = match get_meta(pool, "unique_id").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query unique_id metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let name = match get_meta(pool, "name").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query name metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let audio_format = match get_meta(pool, "audio_format").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query audio_format metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let split_interval = match get_meta(pool, "split_interval").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query split_interval metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let bitrate = match get_meta(pool, "bitrate").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query bitrate metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let sample_rate = match get_meta(pool, "sample_rate").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query sample_rate metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let version = match get_meta(pool, "version").await {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to query version metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    // Get min/max segment IDs
    let sql = segments::select_min_max_id();
    let (min_id, max_id): (i64, i64) = match sqlx::query(&sql).fetch_one(pool).await {
        Ok(row) => {
            let min: Option<i64> = row.get(0);
            let max: Option<i64> = row.get(1);
            (min.unwrap_or(0), max.unwrap_or(0))
        }
        Err(e) => {
            error!("Failed to query min/max segment IDs: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
            )
                .into_response();
        }
    };

    let metadata_resp = Metadata {
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
        serde_json::to_string(&metadata_resp).unwrap(),
    )
        .into_response()
}

async fn sessions_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let pool = &state.pool;

    // Get show name from metadata
    let sql = metadata::select_by_key("name");
    let name: String = match sqlx::query_scalar(&sql).fetch_one(pool).await {
        Ok(n) => n,
        Err(e) => {
            error!("Failed to query name metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
        }
    };

    // Get split interval
    let sql = metadata::select_by_key("split_interval");
    let split_interval: f64 = match sqlx::query_scalar::<_, String>(&sql).fetch_one(pool).await {
        Ok(val) => val.parse().unwrap_or(10.0),
        Err(_) => 10.0,
    };

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
    let sql = segments::select_sessions_with_join();
    let rows = match sqlx::query(&sql).fetch_all(pool).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    // (section_id, start_segment_id, end_segment_id, timestamp_ms)
    let sessions: Vec<SessionInfo> = rows
        .iter()
        .filter_map(|row| {
            let section_id: i64 = row.get(0);
            let timestamp_ms: i64 = row.get(1);
            let start_segment_id: Option<i64> = row.get(2);
            let end_segment_id: Option<i64> = row.get(3);
            match (start_segment_id, end_segment_id) {
                (Some(start_id), Some(end_id)) => {
                    let segment_count = (end_id - start_id + 1) as f64;
                    let duration_seconds = segment_count * split_interval;
                    Some(SessionInfo {
                        section_id,
                        start_id,
                        end_id,
                        timestamp_ms,
                        duration_seconds,
                    })
                }
                _ => None,
            }
        })
        .collect();

    if sessions.is_empty() {
        return (StatusCode::NOT_FOUND, "No recording sessions found").into_response();
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
    let pool = &state.pool;

    // Get the max segment ID and count for this section
    let sql = segments::select_max_and_count_for_section(section_id);
    let result = sqlx::query(&sql).fetch_one(pool).await;

    match result {
        Ok(row) => {
            let max_id: Option<i64> = row.get(0);
            let count: i64 = row.get(1);
            let response = SessionLatestResponse {
                section_id,
                current_end_id: max_id.unwrap_or(0),
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
            error!(
                "Failed to query latest segment for section {}: {}",
                section_id, e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(
                    &serde_json::json!({"error": format!("Database error: {}", e)}),
                )
                .unwrap(),
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
                                    if let Ok(header_name) =
                                        header::HeaderName::from_bytes(name_str.as_bytes())
                                    {
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
                    (
                        StatusCode::BAD_GATEWAY,
                        "Failed to read response from dev server",
                    )
                        .into_response()
                }
            }
        }
        Err(e) => {
            warn!(
                "Failed to connect to dev server at {}: {}",
                VITE_DEV_SERVER, e
            );
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

use axum::routing::post;
use serde::Deserialize;

/// State for receiver mode (multi-show with background sync, using PostgreSQL)
pub struct ReceiverAppState {
    pub config: crate::config::SyncConfig,
    pub password: String,
    /// Connection pool to the global database (save_audio_global) for lease operations
    pub global_pool: PgPool,
}

/// Receiver mode: serve frontend with show selection and background sync (PostgreSQL)
pub fn receiver_audio(
    config: crate::config::SyncConfig,
    password: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port = config.port;

    println!("Starting receiver server (PostgreSQL mode)...");
    println!("PostgreSQL URL: {}", config.postgres_url);
    println!("Remote URL: {}", config.remote_url);
    println!("Sync interval: {} seconds", config.sync_interval_seconds);
    println!("Listening on: http://[::]:{} (IPv4 + IPv6)", port);

    // Create tokio runtime for global pool initialization
    let init_rt = tokio::runtime::Runtime::new()?;

    // Create global database and leases table
    let global_pool = init_rt.block_on(async {
        let pool = crate::db_postgres::open_postgres_connection_create_if_needed(
            &config.postgres_url,
            &password,
            crate::db_postgres::GLOBAL_DATABASE_NAME,
        )
        .await?;
        crate::db_postgres::create_leases_table_pg(&pool).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(pool)
    })?;

    // Clone values for background sync thread
    let sync_config = config.clone();
    let sync_password = password.clone();
    let sync_interval = config.sync_interval_seconds;
    let bg_global_pool = global_pool.clone();

    // Spawn background sync thread (lease handling is inside sync_shows)
    std::thread::spawn(move || loop {
        println!("[Sync] Starting background sync...");
        match crate::sync::sync_shows(&sync_config, &sync_password, &bg_global_pool) {
            Ok(crate::sync::SyncResult::Completed) => {
                println!("[Sync] Background sync completed successfully");
            }
            Ok(crate::sync::SyncResult::Skipped) => {
                println!("[Sync] Background sync skipped (another instance is syncing)");
            }
            Err(e) => {
                eprintln!("[Sync] Background sync error: {}", e);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(sync_interval));
    });

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_state = StdArc::new(ReceiverAppState {
            config: config.clone(),
            password: password.clone(),
            global_pool,
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
            .route(
                "/api/show/{show_name}/format",
                get(receiver_show_format_handler),
            )
            .route(
                "/api/show/{show_name}/sessions",
                get(receiver_show_sessions_handler),
            )
            .route(
                "/api/show/{show_name}/metadata",
                get(receiver_show_metadata_handler),
            )
            .route(
                "/api/show/{show_name}/segments/range",
                get(receiver_show_segments_range_handler),
            )
            // HLS routes for selected show
            .route(
                "/show/{show_name}/opus-playlist.m3u8",
                get(receiver_opus_playlist_handler),
            )
            .route(
                "/show/{show_name}/opus-segment/{filename}",
                get(receiver_opus_segment_handler),
            )
            .route(
                "/show/{show_name}/playlist.m3u8",
                get(receiver_aac_playlist_handler),
            )
            .route(
                "/show/{show_name}/aac-segment/{filename}",
                get(receiver_aac_segment_handler),
            )
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

        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
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
    // Query PostgreSQL databases for available shows
    // Try to connect to each show database and verify it exists
    let mut shows = Vec::new();

    // If we have a whitelist, use it. Otherwise query the remote for available shows
    let show_names: Vec<String> = if let Some(ref filter) = state.config.shows {
        filter.clone()
    } else {
        // Query remote server for available shows (same as sync does)
        let client = reqwest::Client::new();
        let shows_url = format!("{}/api/sync/shows", state.config.remote_url);
        match client.get(&shows_url).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => json
                    .get("shows")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| {
                                v.get("name").and_then(|n| n.as_str()).map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                Err(e) => {
                    error!("Failed to parse shows list: {}", e);
                    Vec::new()
                }
            },
            Err(e) => {
                error!("Failed to fetch shows from remote: {}", e);
                Vec::new()
            }
        }
    };

    for show_name in show_names {
        let database_name = crate::sync::get_pg_database_name(&show_name);
        // Try to connect and get audio format
        let audio_format = match crate::db_postgres::open_postgres_connection(
            &state.config.postgres_url,
            &state.password,
            &database_name,
        )
        .await
        {
            Ok(pool) => {
                let sql = metadata::select_by_key_pg("audio_format");
                sqlx::query_scalar::<_, String>(&sql)
                    .fetch_one(&pool)
                    .await
                    .ok()
            }
            Err(_) => None,
        };

        shows.push(ReceiverShowInfo {
            name: show_name,
            audio_format,
        });
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

/// Open a PostgreSQL connection for a specific show
async fn open_show_pg_pool(state: &ReceiverAppState, show_name: &str) -> Result<PgPool, String> {
    let database_name = crate::sync::get_pg_database_name(show_name);
    crate::db_postgres::open_postgres_connection(
        &state.config.postgres_url,
        &state.password,
        &database_name,
    )
    .await
    .map_err(|e| format!("Failed to connect to database '{}': {}", database_name, e))
}

async fn receiver_show_format_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    let sql = metadata::select_by_key_pg("audio_format");
    let audio_format: String = match sqlx::query_scalar(&sql).fetch_one(&pool).await {
        Ok(f) => f,
        Err(e) => {
            error!(
                "Failed to query audio_format for show '{}': {}",
                show_name, e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
        }
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&FormatResponse {
            format: audio_format,
        })
        .unwrap(),
    )
        .into_response()
}

async fn receiver_show_sessions_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    // Get show name from metadata
    let sql = metadata::select_by_key_pg("name");
    let name: String = match sqlx::query_scalar(&sql).fetch_one(&pool).await {
        Ok(n) => n,
        Err(e) => {
            error!("Failed to query name metadata: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
        }
    };

    // Get split interval
    let sql = metadata::select_by_key_pg("split_interval");
    let split_interval: f64 = match sqlx::query_scalar::<_, String>(&sql).fetch_one(&pool).await {
        Ok(val) => val.parse().unwrap_or(10.0),
        Err(_) => 10.0,
    };

    // Get all sections with their start id and timestamp
    let sql = segments::select_sessions_with_join_pg();
    let rows = match sqlx::query(&sql).fetch_all(&pool).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response();
        }
    };

    // (section_id, start_segment_id, end_segment_id, timestamp_ms)
    let sessions: Vec<SessionInfo> = rows
        .iter()
        .filter_map(|row| {
            let section_id: i64 = row.get(0);
            let timestamp_ms: i64 = row.get(1);
            let start_segment_id: Option<i64> = row.get(2);
            let end_segment_id: Option<i64> = row.get(3);
            match (start_segment_id, end_segment_id) {
                (Some(start_id), Some(end_id)) => {
                    let segment_count = (end_id - start_id + 1) as f64;
                    let duration_seconds = segment_count * split_interval;
                    Some(SessionInfo {
                        section_id,
                        start_id,
                        end_id,
                        timestamp_ms,
                        duration_seconds,
                    })
                }
                _ => None,
            }
        })
        .collect();

    if sessions.is_empty() {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&SessionsResponse {
                name,
                sessions: vec![],
            })
            .unwrap(),
        )
            .into_response();
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
    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    // Query all metadata fields using SeaQuery (PostgreSQL)
    let unique_id: String =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("unique_id"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    let name: String = sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("name"))
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let audio_format: String =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("audio_format"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    let split_interval: String =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("split_interval"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    let bitrate: String = sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("bitrate"))
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let sample_rate: String =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("sample_rate"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    let version: String = sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("version"))
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    let (min_id, max_id) = sqlx::query(&segments::select_min_max_id_pg())
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .map(|row| (row.get::<i64, _>(0), row.get::<i64, _>(1)))
        .unwrap_or((0, 0));

    let metadata_resp = Metadata {
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
        serde_json::to_string(&metadata_resp).unwrap(),
    )
        .into_response()
}

async fn receiver_show_segments_range_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    match sqlx::query(&segments::select_min_max_id_pg())
        .fetch_optional(&pool)
        .await
    {
        Ok(Some(row)) => {
            let min: i64 = row.get(0);
            let max: i64 = row.get(1);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&SegmentRange {
                    start_id: min,
                    end_id: max,
                })
                .unwrap(),
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
    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    let sample_rate: u32 =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("sample_rate"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(48000);

    let start_id = params.start_id.unwrap_or(1);
    let end_id = match params.end_id {
        Some(id) => id,
        None => sqlx::query_scalar::<_, i64>(&segments::select_max_id_pg())
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(i64::MAX),
    };

    let segment_rows = match sqlx::query(&segments::select_range_for_playlist_pg(start_id, end_id))
        .fetch_all(&pool)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let segment_list: Vec<(i64, i64)> = segment_rows
        .iter()
        .map(|row| (row.get::<i64, _>(0), row.get::<i64, _>(1)))
        .collect();

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    let max_duration: f64 = segment_list
        .iter()
        .map(|(_, d)| *d as f64 / sample_rate as f64)
        .fold(0.0, f64::max);

    playlist.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", start_id));
    playlist.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    playlist.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n",
        max_duration.ceil() as u64
    ));
    playlist.push_str(&format!(
        "#EXT-X-MAP:URI=\"/show/{}/opus-segment/init.mp4\"\n",
        show_name
    ));

    for (seg_id, duration_samples) in segment_list {
        let duration = duration_samples as f64 / sample_rate as f64;
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!(
            "/show/{}/opus-segment/{}.m4s\n",
            show_name, seg_id
        ));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        playlist,
    )
        .into_response()
}

async fn receiver_opus_segment_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path((show_name, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Handle init segment first (doesn't need database)
    if filename == "init.mp4" {
        let init_segment = match generate_init_segment(48000, 1, 1, 48000) {
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

    let seg_id: i64 = match filename.strip_suffix(".m4s").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "Invalid segment filename").into_response(),
    };

    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    let audio_data: Vec<u8> =
        match sqlx::query_scalar::<_, Vec<u8>>(&segments::select_audio_by_id_pg(seg_id))
            .fetch_one(&pool)
            .await
        {
            Ok(data) => data,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
                    .into_response()
            }
        };

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

    let base_media_decode_time = ((seg_id - 1) as u64) * (opus_packets.len() as u64 * 960);
    let media_segment = match generate_media_segment(
        seg_id as u32,
        1,
        base_media_decode_time,
        &opus_packets,
        48000,
        960,
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

async fn receiver_aac_playlist_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path(show_name): Path<String>,
    Query(params): Query<ReceiverPlaylistParams>,
) -> impl IntoResponse {
    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    let sample_rate: u32 =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key_pg("sample_rate"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16000);

    let start_id = params.start_id.unwrap_or(1);
    let end_id = match params.end_id {
        Some(id) => id,
        None => sqlx::query_scalar::<_, i64>(&segments::select_max_id_pg())
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(i64::MAX),
    };

    let segment_rows = match sqlx::query(&segments::select_range_for_playlist_pg(start_id, end_id))
        .fetch_all(&pool)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query error: {}", e),
            )
                .into_response()
        }
    };

    let segment_list: Vec<(i64, i64)> = segment_rows
        .iter()
        .map(|row| (row.get::<i64, _>(0), row.get::<i64, _>(1)))
        .collect();

    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    let max_duration: f64 = segment_list
        .iter()
        .map(|(_, d)| *d as f64 / sample_rate as f64)
        .fold(0.0, f64::max);

    playlist.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n",
        max_duration.ceil() as u64
    ));

    for (seg_id, duration_samples) in segment_list {
        let duration = duration_samples as f64 / sample_rate as f64;
        playlist.push_str(&format!("#EXTINF:{:.3},\n", duration));
        playlist.push_str(&format!("/show/{}/aac-segment/{}.aac\n", show_name, seg_id));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        playlist,
    )
        .into_response()
}

async fn receiver_aac_segment_handler(
    State(state): State<StdArc<ReceiverAppState>>,
    Path((show_name, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let seg_id: i64 = match filename.strip_suffix(".aac").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "Invalid segment filename").into_response(),
    };

    let pool = match open_show_pg_pool(&state, &show_name).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to open database for show '{}': {}", show_name, e);
            return (StatusCode::NOT_FOUND, format!("Show not found: {}", e)).into_response();
        }
    };

    let audio_data: Vec<u8> =
        match sqlx::query_scalar::<_, Vec<u8>>(&segments::select_audio_by_id_pg(seg_id))
            .fetch_one(&pool)
            .await
        {
            Ok(data) => data,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                )
                    .into_response()
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

// Sync control handlers

#[derive(Serialize)]
struct SyncStatusResponse {
    in_progress: bool,
}

async fn receiver_sync_status_handler(
    State(state): State<StdArc<ReceiverAppState>>,
) -> impl IntoResponse {
    let in_progress =
        crate::db_postgres::is_lease_held_pg(&state.global_pool, crate::sync::SYNC_LEASE_NAME)
            .await
            .unwrap_or(false);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&SyncStatusResponse { in_progress }).unwrap(),
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
    // Check if sync is already in progress
    let in_progress =
        crate::db_postgres::is_lease_held_pg(&state.global_pool, crate::sync::SYNC_LEASE_NAME)
            .await
            .unwrap_or(false);

    if in_progress {
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

    // Spawn a new sync in a separate thread (lease handling is inside sync_shows)
    let sync_config = state.config.clone();
    let sync_password = state.password.clone();
    let global_pool = state.global_pool.clone();

    std::thread::spawn(move || {
        println!("[Sync] Manual sync triggered...");
        match crate::sync::sync_shows(&sync_config, &sync_password, &global_pool) {
            Ok(crate::sync::SyncResult::Completed) => {
                println!("[Sync] Manual sync completed successfully");
            }
            Ok(crate::sync::SyncResult::Skipped) => {
                println!("[Sync] Manual sync skipped (another instance started syncing)");
            }
            Err(e) => {
                eprintln!("[Sync] Manual sync error: {}", e);
            }
        }
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
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
    response
}

#[cfg(all(not(debug_assertions), not(feature = "web-frontend")))]
async fn receiver_index_handler_release() -> Response {
    (
        StatusCode::NOT_FOUND,
        "Web frontend not available in this build",
    )
        .into_response()
}

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
async fn receiver_assets_handler_release(Path(path): Path<String>) -> Response {
    let (content, mime_type): (&[u8], &str) = match path.as_str() {
        "style.css" => (STYLE_CSS, "text/css"),
        "main.js" => (MAIN_JS, "application/javascript"),
        _ => return (StatusCode::NOT_FOUND, "Asset not found").into_response(),
    };
    let mut response = Response::new(Body::from(content));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime_type));
    response
}

#[cfg(all(not(debug_assertions), not(feature = "web-frontend")))]
async fn receiver_assets_handler_release(Path(_path): Path<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        "Web frontend not available in this build",
    )
        .into_response()
}
