use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use crate::sftp::{SftpClient, SftpConfig, UploadOptions};
use fs2::FileExt;
use log::{error, warn};

#[cfg(not(debug_assertions))]
use axum::response::Response;

#[cfg(debug_assertions)]
use axum::response::Response;
use ogg::writing::PacketWriter;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use tower_http::cors::{Any, CorsLayer};

use crate::audio::{create_opus_comment_header_with_duration, create_opus_id_header};
use crate::constants::EXPECTED_DB_VERSION;
use crate::fmp4::{generate_init_segment, generate_media_segment};
use crate::webm::{
    write_ebml_binary, write_ebml_float, write_ebml_id, write_ebml_size, write_ebml_string,
    write_ebml_uint,
};

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
const INDEX_HTML: &[u8] = include_bytes!("../app/dist/index.html");

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
const STYLE_CSS: &[u8] = include_bytes!("../app/dist/assets/style.css");

#[cfg(all(not(debug_assertions), feature = "web-frontend"))]
const MAIN_JS: &[u8] = include_bytes!("../app/dist/assets/main.js");

// State for axum handlers
struct AppState {
    db_path: String,
    output_dir: String,
    audio_format: String,
    immutable: bool,
    sftp_config: Option<crate::config::SftpExportConfig>,
    credentials: Option<crate::credentials::Credentials>,
    show_locks: crate::ShowLocks,
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

/// Serve multiple databases from a directory (for sync endpoints during recording)
pub fn serve_for_sync(
    output_dir: PathBuf,
    port: u16,
    sftp_config: Option<crate::config::SftpExportConfig>,
    export_to_remote_periodically: bool,
    session_names: Vec<String>,
    credentials: Option<crate::credentials::Credentials>,
    show_locks: crate::ShowLocks,
) -> Result<(), Box<dyn std::error::Error>> {
    let output_dir_str = output_dir.to_string_lossy().to_string();

    println!("Starting multi-show API server");
    println!("Output directory: {}", output_dir_str);
    if sftp_config.is_some() {
        println!("SFTP export: ENABLED");
    } else {
        println!("SFTP export: DISABLED");
    }
    println!("Listening on: http://[::]{} (IPv4 + IPv6)", port);
    println!("Endpoints:");
    println!("  GET /health  - Health check");
    println!("  GET /api/sync/shows  - List available shows");
    println!("  GET /api/sync/shows/:name/metadata  - Show metadata");
    println!("  GET /api/sync/shows/:name/sections  - Show sections metadata");
    println!("  GET /api/sync/shows/:name/sections/:id/export  - Export section audio to file");
    println!("  GET /api/sync/shows/:name/segments  - Show segments");

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {

        // Clone sftp_config for periodic export task if needed
        let sftp_config_for_export = sftp_config.clone();

        let app_state = StdArc::new(AppState {
            db_path: String::new(), // Not used for multi-show serving
            output_dir: output_dir_str.clone(),
            audio_format: String::new(), // Not used for multi-show serving
            immutable: false, // Active recording databases - must not use immutable mode
            sftp_config,
            credentials: credentials.clone(),
            show_locks: show_locks.clone(),
        });

        // Spawn periodic export task if enabled
        if export_to_remote_periodically {
            if let Some(sftp_cfg) = sftp_config_for_export {
                println!("Periodic remote export: ENABLED (every hour)");
                spawn_periodic_export_task(
                    output_dir_str.clone(),
                    sftp_cfg,
                    session_names,
                    credentials.clone(),
                    show_locks.clone(),
                );
            } else {
                println!("Warning: export_to_remote_periodically is enabled but SFTP config is missing");
            }
        }

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let api_routes = Router::new()
            .route("/health", get(health_handler))
            .route("/api/sync/shows", get(sync_shows_list_handler))
            .route(
                "/api/sync/shows/{show_name}/metadata",
                get(sync_show_metadata_handler),
            )
            .route(
                "/api/sync/shows/{show_name}/sections",
                get(db_sections_handler),
            )
            .route(
                "/api/sync/shows/{show_name}/sections/{section_id}/export",
                get(export_section_handler),
            )
            .route(
                "/api/sync/shows/{show_name}/segments",
                get(sync_show_segments_handler),
            );

        let app = api_routes.layer(cors).with_state(app_state);

        let listener = tokio::net::TcpListener::bind(format!("[::]:{}", port))
            .await
            .unwrap();
        axum::serve(listener, app).await.unwrap();
    });

    Ok(())
}

/// Serve a single database file (for serve command)
pub fn serve_audio(sqlite_file: PathBuf, port: u16, immutable: bool) -> Result<(), Box<dyn std::error::Error>> {
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

    let db_path = sqlite_file.to_string_lossy().to_string();

    // Extract output_dir from database path (parent directory)
    let output_dir = sqlite_file
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());

    println!("Starting server for: {}", db_path);
    println!("Output directory: {}", output_dir);
    println!("Audio format: {}", audio_format);
    println!("Listening on: http://[::]:{} (IPv4 + IPv6)", port);
    println!("Endpoints:");
    if audio_format == "opus" {
        println!("  GET /manifest.mpd?start_id=<N>&end_id=<N>  - DASH MPD");
        println!("  GET /init.webm  - WebM initialization segment");
        println!("  GET /segment/:id  - WebM audio segment");
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
            db_path,
            output_dir,
            audio_format: audio_format.clone(),
            immutable,
            sftp_config: None, // SFTP not supported in serve command
            credentials: None, // Credentials not needed in serve mode
            show_locks: std::sync::Arc::new(dashmap::DashMap::new()), // Not used in serve mode
        });

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let mut api_routes = Router::new()
            .route("/api/format", get(format_handler))
            .route("/api/segments/range", get(segments_range_handler))
            .route("/api/sessions", get(sessions_handler))
            .route("/api/sync/shows", get(sync_shows_list_handler))
            .route(
                "/api/sync/shows/{show_name}/metadata",
                get(sync_show_metadata_handler),
            )
            .route(
                "/api/sync/shows/{show_name}/sections",
                get(db_sections_handler),
            )
            .route(
                "/api/sync/shows/{show_name}/segments",
                get(sync_show_segments_handler),
            );

        // Add format-specific routes
        if audio_format == "opus" {
            api_routes = api_routes
                .route("/manifest.mpd", get(mpd_handler))
                .route("/init.webm", get(init_handler))
                .route("/segment/{id}", get(segment_handler))
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

fn map_to_io_error<E: std::fmt::Display + Send + Sync + 'static>(err: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
}

/// Parse ADTS frames in AAC data and return total samples
fn parse_adts_frames(data: &[u8], frame_size: u32) -> Result<u64, String> {
    let mut total_samples = 0u64;
    let mut pos = 0;

    while pos + 7 < data.len() {
        // Find ADTS sync word (0xFFF at start of header)
        if data[pos] != 0xFF || (data[pos + 1] & 0xF0) != 0xF0 {
            pos += 1;
            continue;
        }

        // Extract frame length from ADTS header (13 bits)
        let frame_len = (((data[pos + 3] & 0x03) as usize) << 11)
            | ((data[pos + 4] as usize) << 3)
            | ((data[pos + 5] as usize) >> 5);

        if pos + frame_len > data.len() || frame_len < 7 {
            break;
        }

        total_samples += frame_size as u64;
        pos += frame_len;
    }

    if total_samples == 0 {
        return Err("No valid ADTS frames found".to_string());
    }

    Ok(total_samples)
}

fn write_ogg_stream<W: Write>(
    conn: &Connection,
    start_id: i64,
    end_id: i64,
    sample_rate: u32,
    duration_secs: f64,
    samples_per_packet: u64,
    writer: W,
) -> Result<W, std::io::Error> {
    let mut stmt = conn
        .prepare("SELECT id, audio_data FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id")
        .map_err(map_to_io_error)?;
    let mut rows = stmt.query([start_id, end_id]).map_err(map_to_io_error)?;

    let mut writer = PacketWriter::new(writer);

    writer
        .write_packet(
            create_opus_id_header(1, sample_rate),
            1,
            ogg::writing::PacketWriteEndInfo::EndPage,
            0,
        )
        .map_err(map_to_io_error)?;

    writer
        .write_packet(
            create_opus_comment_header_with_duration(Some(duration_secs)),
            1,
            ogg::writing::PacketWriteEndInfo::EndPage,
            0,
        )
        .map_err(map_to_io_error)?;

    let mut granule_pos: u64 = 0;
    let mut packet_count: u32 = 0;
    const PACKETS_PER_PAGE: u32 = 50;

    while let Some(row) = rows.next().map_err(map_to_io_error)? {
        let id: i64 = row.get(0).map_err(map_to_io_error)?;
        let segment: Vec<u8> = row.get(1).map_err(map_to_io_error)?;
        let is_last_segment = id == end_id;
        let mut offset = 0;

        while offset + 2 <= segment.len() {
            let len = u16::from_le_bytes([segment[offset], segment[offset + 1]]) as usize;
            offset += 2;

            if offset + len > segment.len() {
                break;
            }

            let packet = &segment[offset..offset + len];
            offset += len;

            granule_pos += samples_per_packet;
            packet_count += 1;

            let is_last_packet = is_last_segment && offset >= segment.len();
            let end_info = if is_last_packet {
                ogg::writing::PacketWriteEndInfo::EndStream
            } else if packet_count % PACKETS_PER_PAGE == 0 {
                ogg::writing::PacketWriteEndInfo::EndPage
            } else {
                ogg::writing::PacketWriteEndInfo::NormalPacket
            };

            writer
                .write_packet(packet.to_vec(), 1, end_info, granule_pos)
                .map_err(map_to_io_error)?;
        }
    }

    Ok(writer.into_inner())
}

/// Export Opus audio for a section to an Ogg file
fn export_opus_section(
    conn: &Connection,
    section_id: i64,
    sample_rate: u32,
    duration_secs: f64,
) -> Result<Vec<u8>, std::io::Error> {
    // Get segment ID range for this section
    let (start_id, end_id): (i64, i64) = conn
        .query_row(
            "SELECT MIN(id), MAX(id) FROM segments WHERE section_id = ?1",
            [section_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_to_io_error)?;

    // Write Ogg stream to memory buffer
    let mut buffer = Vec::new();
    let samples_per_packet = 960; // 48kHz Opus, 20ms packets

    write_ogg_stream(
        conn,
        start_id,
        end_id,
        sample_rate,
        duration_secs,
        samples_per_packet,
        &mut buffer,
    )?;

    Ok(buffer)
}

/// Export AAC audio for a section to a memory buffer (raw ADTS frames)
fn export_aac_section(
    segments: &[(i64, Vec<u8>)],
) -> Result<Vec<u8>, std::io::Error> {
    // Create buffer
    let mut buffer = Vec::new();

    // Write all segment data (AAC ADTS frames) to buffer
    for (_, segment_data) in segments {
        buffer.write_all(segment_data)?;
    }

    Ok(buffer)
}

// Query parameters for MPD
#[derive(Deserialize)]
struct MpdQuery {
    start_id: i64,
    end_id: i64,
}

// DASH MPD manifest handler
async fn mpd_handler(
    State(state): State<StdArc<AppState>>,
    Query(query): Query<MpdQuery>,
) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Validate end_id
    let max_id: i64 = match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to query max segment ID: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    if query.end_id > max_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("end_id {} exceeds max id {}", query.end_id, max_id),
        )
            .into_response();
    }

    if query.start_id > query.end_id {
        return (StatusCode::BAD_REQUEST, "start_id must be <= end_id").into_response();
    }

    // Get split_interval from metadata (in seconds)
    let split_interval: f64 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'split_interval'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(1.0))
            },
        )
        .unwrap_or(1.0);

    // Get sample_rate from metadata
    let sample_rate: u32 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'sample_rate'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(48000))
            },
        )
        .unwrap_or(48000);

    // Get bitrate from metadata
    let bitrate_kbps: u32 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'bitrate'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(16))
            },
        )
        .unwrap_or(16);
    let bandwidth = bitrate_kbps * 1000;

    // Calculate total duration and segment repeat count
    let segment_count = (query.end_id - query.start_id + 1) as u32;
    let total_duration = segment_count as f64 * split_interval;

    let duration_ms = (split_interval * 1000.0) as u32;
    let repeat_count = segment_count.saturating_sub(1);

    // Build DASH MPD with SegmentTemplate
    let mpd = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="static"
     mediaPresentationDuration="PT{:.3}S"
     minBufferTime="PT2S"
     profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period duration="PT{:.3}S">
    <AdaptationSet mimeType="audio/webm" codecs="opus" lang="en">
      <SegmentTemplate
        initialization="init.webm"
        media="segment/$Number$?base={}"
        startNumber="1"
        timescale="1000">
        <SegmentTimeline>
          <S d="{}" r="{}"/>
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="audio" bandwidth="{}" audioSamplingRate="{}">
        <AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="1"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#,
        total_duration,
        total_duration,
        query.start_id,
        duration_ms,
        repeat_count,
        bandwidth,
        sample_rate
    );

    (
        StatusCode::OK,
        [("content-type", "application/dash+xml")],
        mpd,
    )
        .into_response()
}

// Initialization segment handler
async fn init_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get sample_rate from metadata
    let sample_rate: f64 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'sample_rate'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(48000.0))
            },
        )
        .unwrap_or(48000.0);

    // Build WebM initialization segment
    let mut webm = Vec::new();

    // EBML Header
    let mut ebml_header = Vec::new();
    write_ebml_uint(&mut ebml_header, 0x4286, 1);
    write_ebml_uint(&mut ebml_header, 0x42F7, 1);
    write_ebml_uint(&mut ebml_header, 0x42F2, 4);
    write_ebml_uint(&mut ebml_header, 0x42F3, 8);
    write_ebml_string(&mut ebml_header, 0x4282, "webm");
    write_ebml_uint(&mut ebml_header, 0x4287, 4);
    write_ebml_uint(&mut ebml_header, 0x4285, 2);

    write_ebml_id(&mut webm, 0x1A45DFA3);
    write_ebml_size(&mut webm, ebml_header.len() as u64);
    webm.extend_from_slice(&ebml_header);

    // Build Segment content
    let mut segment_content = Vec::new();

    // Info
    let mut info = Vec::new();
    write_ebml_uint(&mut info, 0x2AD7B1, 1_000_000);
    write_ebml_string(&mut info, 0x4D80, "save_audio_stream");
    write_ebml_string(&mut info, 0x5741, "save_audio_stream");

    write_ebml_id(&mut segment_content, 0x1549A966);
    write_ebml_size(&mut segment_content, info.len() as u64);
    segment_content.extend_from_slice(&info);

    // Tracks
    let mut tracks = Vec::new();
    let mut track_entry = Vec::new();
    write_ebml_uint(&mut track_entry, 0xD7, 1);
    write_ebml_uint(&mut track_entry, 0x73C5, 1);
    write_ebml_uint(&mut track_entry, 0x83, 2);
    write_ebml_string(&mut track_entry, 0x86, "A_OPUS");

    // CodecPrivate - OpusHead
    let mut opus_head = Vec::new();
    opus_head.extend_from_slice(b"OpusHead");
    opus_head.push(1);
    opus_head.push(1);
    opus_head.extend_from_slice(&0u16.to_le_bytes());
    opus_head.extend_from_slice(&(sample_rate as u32).to_le_bytes());
    opus_head.extend_from_slice(&0i16.to_le_bytes());
    opus_head.push(0);
    write_ebml_binary(&mut track_entry, 0x63A2, &opus_head);

    // Audio settings
    let mut audio = Vec::new();
    write_ebml_float(&mut audio, 0xB5, sample_rate);
    write_ebml_uint(&mut audio, 0x9F, 1);

    write_ebml_id(&mut track_entry, 0xE1);
    write_ebml_size(&mut track_entry, audio.len() as u64);
    track_entry.extend_from_slice(&audio);

    write_ebml_id(&mut tracks, 0xAE);
    write_ebml_size(&mut tracks, track_entry.len() as u64);
    tracks.extend_from_slice(&track_entry);

    write_ebml_id(&mut segment_content, 0x1654AE6B);
    write_ebml_size(&mut segment_content, tracks.len() as u64);
    segment_content.extend_from_slice(&tracks);

    // Write Segment with unknown size
    write_ebml_id(&mut webm, 0x18538067);
    webm.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    webm.extend_from_slice(&segment_content);

    (StatusCode::OK, [("content-type", "video/webm")], webm).into_response()
}

// Segment handler
#[derive(Deserialize)]
struct SegmentQuery {
    #[serde(default)]
    base: Option<i64>,
}

async fn segment_handler(
    State(state): State<StdArc<AppState>>,
    Path(id): Path<i64>,
    Query(query): Query<SegmentQuery>,
) -> impl IntoResponse {
    // Calculate actual segment ID
    let actual_id = if let Some(base) = query.base {
        base + (id - 1)
    } else {
        id
    };

    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get the segment
    let segment: Vec<u8> = match conn.query_row(
        "SELECT audio_data FROM segments WHERE id = ?1",
        [actual_id],
        |row| row.get(0),
    ) {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to query segment {}: {}", actual_id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error querying segment {}: {}", actual_id, e),
            )
                .into_response()
        }
    };

    // Get split_interval and calculate timecode
    let split_interval: u64 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'split_interval'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(1))
            },
        )
        .unwrap_or(1);

    // Calculate timecode relative to the base (session start)
    // If base is provided, use it; otherwise use global minimum
    let base_id = if let Some(base) = query.base {
        base
    } else {
        conn.query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0))
            .unwrap_or(1)
    };

    let relative_pos = (actual_id - base_id) as u64;
    let timecode_ms = relative_pos * split_interval * 1000;

    // Build Cluster
    let mut cluster_content = Vec::new();
    write_ebml_uint(&mut cluster_content, 0xE7, timecode_ms);

    // Parse and write SimpleBlocks
    let mut offset = 0;
    let mut block_time: i16 = 0;
    while offset + 2 <= segment.len() {
        let len = u16::from_le_bytes([segment[offset], segment[offset + 1]]) as usize;
        offset += 2;

        if offset + len > segment.len() {
            break;
        }

        let packet = &segment[offset..offset + len];
        offset += len;

        let mut simple_block = Vec::new();
        simple_block.push(0x81);
        simple_block.extend_from_slice(&block_time.to_be_bytes());
        simple_block.push(0x80);
        simple_block.extend_from_slice(packet);

        write_ebml_binary(&mut cluster_content, 0xA3, &simple_block);

        block_time += 20;
    }

    // Write Cluster element
    let mut webm = Vec::new();
    write_ebml_id(&mut webm, 0x1F43B675);
    write_ebml_size(&mut webm, cluster_content.len() as u64);
    webm.extend_from_slice(&cluster_content);

    (StatusCode::OK, [("content-type", "video/webm")], webm).into_response()
}

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

// HLS playlist handler for AAC format
async fn hls_playlist_handler(
    State(state): State<StdArc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
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

    let frame_size: u32 = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'aac_frame_size'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(fs) => fs.parse().unwrap_or(1024),
        Err(e) => {
            error!("Failed to query aac_frame_size metadata: {}", e);
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

    // Query segments and calculate durations
    let mut stmt = match conn
        .prepare("SELECT id, audio_data FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id")
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
        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
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
        let (seg_id, audio_data): (i64, Vec<u8>) = match segment_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: Failed to fetch segment from database: {}", e);
                continue;
            },
        };

        let samples = match parse_adts_frames(&audio_data, frame_size) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: Failed to parse ADTS frames for segment {}: {}", seg_id, e);
                continue;
            },
        };

        let duration = samples as f64 / sample_rate as f64;
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
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
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

/// Parse Opus packets from audio data and return both packet count and packet data
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

// HLS playlist handler for Opus format
async fn opus_hls_playlist_handler(
    State(state): State<StdArc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
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

    // Opus frame size is always 960 samples at 48kHz (20ms)
    let samples_per_packet = 960u32;

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

    // Query segments and calculate durations
    let mut stmt = match conn
        .prepare("SELECT id, audio_data FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id")
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
        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
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
        let (seg_id, audio_data): (i64, Vec<u8>) = match segment_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: Failed to fetch segment from database: {}", e);
                continue;
            },
        };

        let packets = match parse_opus_packets(&audio_data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Warning: Failed to parse Opus packets for segment {}: {}", seg_id, e);
                continue;
            },
        };

        // Each Opus packet is 20ms (960 samples at 48kHz)
        let duration = (packets.len() as f64 * samples_per_packet as f64) / sample_rate as f64;
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
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
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
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
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

async fn sessions_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    let conn = match state.open_readonly(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", state.db_path, e);
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

// Sync API Handlers

#[derive(Serialize)]
struct ShowInfo {
    name: String,
    database_file: String,
    min_id: i64,
    max_id: i64,
}

// Health check endpoint - returns 200 OK if server is running
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

#[derive(Serialize)]
struct SectionInfo {
    id: i64,
    start_timestamp_ms: i64,
}

#[derive(Serialize)]
struct ShowsList {
    shows: Vec<ShowInfo>,
}

async fn db_sections_handler(
    State(state): State<StdArc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Construct database path
    let db_path = crate::db::get_db_path(&state.output_dir, &name);
    let path = std::path::Path::new(&db_path);

    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("Database '{}' not found", name)})),
        )
            .into_response();
    }

    // Open database
    let conn = match state.open_readonly(path) {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", db_path, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to open database: {}", e)})),
            )
                .into_response();
        }
    };

    // Fetch all sections
    let mut stmt = match conn.prepare("SELECT id, start_timestamp_ms FROM sections ORDER BY id") {
        Ok(stmt) => stmt,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to prepare query: {}", e)})),
            )
                .into_response();
        }
    };

    let sections: Result<Vec<SectionInfo>, _> = stmt
        .query_map([], |row| {
            Ok(SectionInfo {
                id: row.get(0)?,
                start_timestamp_ms: row.get(1)?,
            })
        })
        .and_then(|rows| rows.collect());

    match sections {
        Ok(sections) => axum::Json::<Vec<SectionInfo>>(sections).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"error": format!("Failed to fetch sections: {}", e)})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ExportSectionPath {
    show_name: String,
    section_id: String,
}

#[derive(Serialize)]
pub struct ExportResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sftp_path: Option<String>,
    pub section_id: i64,
    pub format: String,
    pub duration_seconds: f64,
}

/// Upload data directly to SFTP using streaming
///
/// This function uploads data from memory to the configured SFTP server
/// using atomic upload (temp file + rename). No local file is created.
///
/// Returns the remote SFTP path if successful.
fn upload_to_sftp(
    data: &[u8],
    filename: &str,
    config: &crate::config::SftpExportConfig,
    credentials: &Option<crate::credentials::Credentials>,
) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::Cursor;

    // Upload to SFTP
    let sftp_config = SftpConfig::from_export_config(config, credentials)
        .map_err(|e| format!("Failed to resolve SFTP credentials: {}", e))?;

    let client = SftpClient::connect(&sftp_config)?;
    let remote_path = std::path::Path::new(&config.remote_dir).join(filename);
    let remote_path_str = remote_path.to_string_lossy().to_string();
    let options = UploadOptions::default();

    // Upload from memory using streaming
    let mut cursor = Cursor::new(data);
    let upload_result = client.upload_stream(
        &mut cursor,
        &remote_path,
        data.len() as u64,
        &options,
    );

    // Disconnect from SFTP
    let _ = client.disconnect();

    // Return the remote path if upload succeeded
    upload_result.map(|_| remote_path_str).map_err(|e| e.into())
}

/// Spawn a periodic task to export unexported sections to SFTP
///
/// This function spawns a background task that runs every hour and exports all
/// sections that have not been exported to remote SFTP yet, excluding the pending
/// (currently recording) section. Only processes databases for the specified session names.
fn spawn_periodic_export_task(
    output_dir: String,
    sftp_config: crate::config::SftpExportConfig,
    session_names: Vec<String>,
    credentials: Option<crate::credentials::Credentials>,
    show_locks: crate::ShowLocks,
) {
    tokio::task::spawn_blocking(move || {
        loop {
            println!("Starting periodic export of unexported sections...");

            // Process each session database
            for show_name in &session_names {
                let db_path = std::path::Path::new(&output_dir).join(format!("{}.sqlite", show_name));

                // Open database to query for unexported sections
                let conn = match crate::db::open_database_connection(&db_path) {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("Failed to open database {}: {}", db_path.display(), e);
                        continue;
                    }
                };

                // Get pending section ID (to exclude from export)
                let pending_section_id: Option<i64> = conn
                    .query_row(
                        "SELECT value FROM metadata WHERE key = 'pending_section_id'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|s| s.parse().ok());

                // Query for unexported sections
                let unexported_sections: Vec<i64> = {
                    let query = if let Some(_pending_id) = pending_section_id {
                        "SELECT id FROM sections
                         WHERE (is_exported_to_remote IS NULL OR is_exported_to_remote = 0)
                           AND id != ?1"
                    } else {
                        "SELECT id FROM sections
                         WHERE (is_exported_to_remote IS NULL OR is_exported_to_remote = 0)"
                    };

                    let mut stmt = match conn.prepare(query) {
                        Ok(stmt) => stmt,
                        Err(e) => {
                            error!("Failed to prepare query for {}: {}", show_name, e);
                            continue;
                        }
                    };

                    let sections_result: Result<Vec<i64>, _> = if let Some(pending_id) = pending_section_id {
                        stmt.query_map([pending_id], |row| row.get(0))
                            .and_then(|rows| rows.collect())
                    } else {
                        stmt.query_map([], |row| row.get(0))
                            .and_then(|rows| rows.collect())
                    };

                    match sections_result {
                        Ok(sections) => sections,
                        Err(e) => {
                            error!("Failed to query unexported sections for {}: {}", show_name, e);
                            continue;
                        }
                    }
                };

                // Export each unexported section
                if unexported_sections.is_empty() {
                    println!("No unexported sections found for show: {}", show_name);
                } else {
                    println!(
                        "Found {} unexported section(s) for show: {}",
                        unexported_sections.len(),
                        show_name
                    );

                    for section_id in unexported_sections {
                        // Acquire lock before export to prevent concurrent cleanup
                        let show_lock = crate::get_show_lock(&show_locks, show_name);
                        println!("[{}] Acquiring export lock for section {}...", show_name, section_id);
                        let _export_guard = show_lock.lock().unwrap();  // BLOCKS if cleanup is running
                        println!("[{}] Export lock acquired for section {}", show_name, section_id);

                        match export_section(
                            &output_dir,
                            show_name,
                            section_id,
                            Some(&sftp_config),
                            &credentials,
                        ) {
                            Ok(response) => {
                                println!(
                                    "Successfully exported section {} of show {} to: {:?}",
                                    section_id, show_name, response.sftp_path
                                );
                            }
                            Err(e) => {
                                error!(
                                    "Failed to export section {} of show {}: {}",
                                    section_id, show_name, e
                                );
                            }
                        }

                        // Lock automatically released when _export_guard drops
                        drop(_export_guard);
                        println!("[{}] Export lock released for section {}", show_name, section_id);
                    }
                }
            }

            println!("Periodic export completed. Sleeping for 1 hour until next run...");

            // Sleep for 1 hour before next export cycle
            std::thread::sleep(std::time::Duration::from_secs(60 * 60));
        }
    });
}

/// Export a section to file or SFTP with locking
///
/// This function handles the entire export process including:
/// - Acquiring an exclusive lock to prevent concurrent exports
/// - Reading section data from the database
/// - Checking if the section was already exported to SFTP
/// - Encoding audio data
/// - Uploading to SFTP or saving to local file
/// - Updating the is_exported_to_remote flag
///
/// Returns ExportResponse on success or an error message on failure.
pub fn export_section(
    output_dir: &str,
    show_name: &str,
    section_id: i64,
    sftp_config: Option<&crate::config::SftpExportConfig>,
    credentials: &Option<crate::credentials::Credentials>,
) -> Result<ExportResponse, String> {
    // Create tmp directory for lock files if it doesn't exist
    std::fs::create_dir_all("tmp")
        .map_err(|e| format!("Failed to create tmp directory: {}", e))?;

    // Acquire exclusive lock to prevent concurrent exports of the same show
    let lock_path = format!("tmp/export_{}.lock", show_name);
    let _lock_file = File::create(&lock_path)
        .map_err(|e| format!("Failed to create lock file '{}': {}", lock_path, e))?;

    _lock_file.try_lock_exclusive()
        .map_err(|e| format!("Export already in progress for show '{}'. Lock file: {}. Error: {}", show_name, lock_path, e))?;
    // Lock will be held until _lock_file is dropped (when function exits)

    // Construct database path
    let db_path = crate::db::get_db_path(output_dir, show_name);
    let path = std::path::Path::new(&db_path);

    if !path.exists() {
        return Err(format!("Database '{}' not found", show_name));
    }

    // Open read/write database connection (needed for updating is_exported_to_remote)
    let conn = crate::db::open_database_connection(path)
        .map_err(|e| {
            error!("Failed to open database connection at '{}': {}", db_path, e);
            format!("Failed to open database: {}", e)
        })?;

    // Get section info including is_exported_to_remote
    let section_info: Result<(i64, i64, Option<i64>), _> = conn.query_row(
        "SELECT id, start_timestamp_ms, is_exported_to_remote FROM sections WHERE id = ?1",
        [section_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    );

    let (section_id, start_timestamp_ms, is_exported_to_remote) = section_info
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => format!("Section {} not found", section_id),
            _ => format!("Failed to fetch section: {}", e),
        })?;

    // Get metadata
    let audio_format: String = conn.query_row(
        "SELECT value FROM metadata WHERE key = 'audio_format'",
        [],
        |row| row.get(0),
    )
    .map_err(|e| format!("Failed to read audio_format: {}", e))?;

    let sample_rate: u32 = conn.query_row(
        "SELECT value FROM metadata WHERE key = 'sample_rate'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|rate| rate.parse().ok())
    .unwrap_or(48000);

    // If section has already been exported to remote and SFTP is configured, return the SFTP path directly
    if is_exported_to_remote == Some(1) && sftp_config.is_some() {
        // Format timestamp as yyyymmdd_hhmmss
        let datetime = chrono::DateTime::from_timestamp_millis(start_timestamp_ms);
        let formatted_time = match datetime {
            Some(dt) => dt.format("%Y%m%d_%H%M%S").to_string(),
            None => format!("{}", start_timestamp_ms),
        };

        // Format section_id as hex
        let hex_section_id = format!("{:x}", section_id);

        // Determine extension
        let extension = match audio_format.as_str() {
            "opus" => "ogg",
            "aac" => "aac",
            _ => return Err(format!("Unsupported audio format: {}", audio_format)),
        };

        // Generate filename
        let filename = format!("{}_{}_{}.{}", show_name, formatted_time, hex_section_id, extension);

        // Construct remote path
        let sftp_cfg = sftp_config.unwrap();
        let remote_path = std::path::Path::new(&sftp_cfg.remote_dir).join(&filename);
        let remote_path_str = remote_path.to_string_lossy().to_string();

        // Query segments to calculate duration
        let mut stmt = conn.prepare(
            "SELECT id, audio_data FROM segments WHERE section_id = ?1 ORDER BY id",
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let segments: Result<Vec<(i64, Vec<u8>)>, _> = stmt
            .query_map([section_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .and_then(|rows| rows.collect());

        let segments = segments
            .map_err(|e| format!("Failed to fetch segments: {}", e))?;

        if segments.is_empty() {
            return Err(format!("No segments found for section {}", section_id));
        }

        // Calculate duration
        let total_samples = if audio_format == "opus" {
            let samples_per_packet = 960u64; // 48kHz Opus, 20ms packets
            let mut total_packets = 0u64;
            for (_, segment) in &segments {
                let mut offset = 0;
                while offset + 2 <= segment.len() {
                    let len = u16::from_le_bytes([segment[offset], segment[offset + 1]]) as usize;
                    offset += 2;
                    if offset + len > segment.len() {
                        break;
                    }
                    offset += len;
                    total_packets += 1;
                }
            }
            total_packets * samples_per_packet
        } else {
            // AAC - approximate based on frame size
            let frame_samples = 1024u64; // AAC frame size
            let mut total_frames = 0u64;
            for (_, segment) in &segments {
                // Count ADTS frames (rough estimate)
                total_frames += segment.len() as u64 / 200; // Approximate
            }
            total_frames * frame_samples
        };

        let duration_secs = total_samples as f64 / sample_rate as f64;

        // Return cached export response
        return Ok(ExportResponse {
            file_path: None,
            sftp_path: Some(remote_path_str),
            section_id,
            format: audio_format,
            duration_seconds: duration_secs,
        });
    }

    // Get all segments for this section
    let mut stmt = conn.prepare(
        "SELECT id, audio_data FROM segments WHERE section_id = ?1 ORDER BY id",
    )
    .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let segments: Result<Vec<(i64, Vec<u8>)>, _> = stmt
        .query_map([section_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .and_then(|rows| rows.collect());

    let segments = segments
        .map_err(|e| format!("Failed to fetch segments: {}", e))?;

    if segments.is_empty() {
        return Err(format!("No segments found for section {}", section_id));
    }

    // Format timestamp as yyyymmdd_hhmmss
    let datetime = chrono::DateTime::from_timestamp_millis(start_timestamp_ms);
    let formatted_time = match datetime {
        Some(dt) => dt.format("%Y%m%d_%H%M%S").to_string(),
        None => format!("{}", start_timestamp_ms),
    };

    // Format section_id as hex
    let hex_section_id = format!("{:x}", section_id);

    // Determine extension
    let extension = match audio_format.as_str() {
        "opus" => "ogg",
        "aac" => "aac",
        _ => return Err(format!("Unsupported audio format: {}", audio_format)),
    };

    // Generate filename
    let filename = format!("{}_{}._{}.{}", show_name, formatted_time, hex_section_id, extension);

    // Calculate duration in seconds
    let total_samples = if audio_format == "opus" {
        let samples_per_packet = 960u64; // 48kHz Opus, 20ms packets
        let mut total_packets = 0u64;
        for (_, segment) in &segments {
            let mut offset = 0;
            while offset + 2 <= segment.len() {
                let len = u16::from_le_bytes([segment[offset], segment[offset + 1]]) as usize;
                offset += 2;
                if offset + len > segment.len() {
                    break;
                }
                offset += len;
                total_packets += 1;
            }
        }
        total_packets * samples_per_packet
    } else {
        // AAC - approximate based on frame size
        let frame_samples = 1024u64; // AAC frame size
        let mut total_frames = 0u64;
        for (_, segment) in &segments {
            // Count ADTS frames (rough estimate)
            total_frames += segment.len() as u64 / 200; // Approximate
        }
        total_frames * frame_samples
    };

    let duration_secs = total_samples as f64 / sample_rate as f64;

    // Export audio to memory buffer
    let audio_data = match audio_format.as_str() {
        "opus" => export_opus_section(&conn, section_id, sample_rate, duration_secs),
        "aac" => export_aac_section(&segments),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Unsupported format",
        )),
    }
    .map_err(|e| format!("Failed to export audio: {}", e))?;

    // Upload to SFTP if configured, otherwise save to file
    let (response_file_path, response_sftp_path) = if let Some(sftp_cfg) = sftp_config {
        // Upload directly from memory to SFTP (atomic, no disk write)
        let remote_path = upload_to_sftp(&audio_data, &filename, sftp_cfg, credentials)
            .map_err(|e| format!("SFTP upload failed: {}", e))?;

        // SFTP upload succeeded, update is_exported_to_remote column
        if let Err(e) = conn.execute(
            "UPDATE sections SET is_exported_to_remote = 1 WHERE id = ?1",
            [section_id],
        ) {
            error!("Failed to update is_exported_to_remote for section {}: {}", section_id, e);
            // Don't fail the request, just log the error
        }

        // Return remote path
        (None, Some(remote_path))
    } else {
        // No SFTP configured, save to local file
        // Create tmp directory if it doesn't exist
        std::fs::create_dir_all("tmp")
            .map_err(|e| format!("Failed to create tmp directory: {}", e))?;

        let file_path = format!("tmp/{}", filename);
        std::fs::write(&file_path, &audio_data)
            .map_err(|e| format!("Failed to write file: {}", e))?;

        (Some(file_path), None)
    };

    Ok(ExportResponse {
        file_path: response_file_path,
        sftp_path: response_sftp_path,
        section_id,
        format: audio_format,
        duration_seconds: duration_secs,
    })
}

async fn export_section_handler(
    State(state): State<StdArc<AppState>>,
    Path(params): Path<ExportSectionPath>,
) -> impl IntoResponse {
    let show_name = params.show_name;
    let section_id: i64 = match params.section_id.parse() {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"error": "Invalid section_id"})),
            )
                .into_response();
        }
    };

    // Acquire lock before export to prevent concurrent cleanup
    let show_lock = crate::get_show_lock(&state.show_locks, &show_name);
    println!("[{}] Acquiring on-demand export lock for section {}...", show_name, section_id);
    let _export_guard = show_lock.lock().unwrap();  // BLOCKS if cleanup is running
    println!("[{}] On-demand export lock acquired for section {}", show_name, section_id);

    // Call the export_section function
    let result = match export_section(
        &state.output_dir,
        &show_name,
        section_id,
        state.sftp_config.as_ref(),
        &state.credentials,
    ) {
        Ok(response) => axum::Json(response).into_response(),
        Err(error_msg) => {
            // Determine appropriate status code based on error message
            let status_code = if error_msg.contains("not found") || error_msg.contains("No segments") {
                StatusCode::NOT_FOUND
            } else if error_msg.contains("already in progress") {
                StatusCode::CONFLICT
            } else if error_msg.contains("Invalid") || error_msg.contains("Unsupported") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };

            (
                status_code,
                axum::Json(serde_json::json!({"error": error_msg})),
            )
                .into_response()
        }
    };

    // Lock automatically released when _export_guard drops
    drop(_export_guard);
    println!("[{}] On-demand export lock released for section {}", show_name, section_id);

    result
}

async fn sync_shows_list_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    // Scan output directory for .sqlite files
    let output_dir = &state.output_dir;
    let dir_path = std::path::Path::new(output_dir);

    if !dir_path.exists() || !dir_path.is_dir() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"error": "Output directory not found"})),
        )
            .into_response();
    }

    let mut shows = Vec::new();

    // Read directory entries
    let entries = match std::fs::read_dir(dir_path) {
        Ok(entries) => entries,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(
                    serde_json::json!({"error": format!("Failed to read directory: {}", e)}),
                ),
            )
                .into_response();
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Warning: Failed to read directory entry: {}", e);
                continue;
            },
        };

        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Check if it's a .sqlite file
        if let Some(extension) = path.extension() {
            if extension != "sqlite" {
                continue;
            }
        } else {
            continue;
        }

        // Open database and check if it's a recording database (not recipient)
        let conn = match state.open_readonly(&path) {
            Ok(conn) => conn,
            Err(e) => {
                warn!("Failed to open database {:?} for show listing: {}", path, e);
                continue;
            },
        };

        // Check is_recipient flag
        let is_recipient: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'is_recipient'",
                [],
                |row| row.get(0),
            )
            .ok();

        if let Some(is_recipient) = is_recipient {
            if is_recipient == "true" {
                continue; // Skip recipient databases
            }
        }

        // Get name from metadata
        let name: Option<String> = conn
            .query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
                row.get(0)
            })
            .ok();

        let name = match name {
            Some(n) => n,
            None => continue,
        };

        // Get min/max segment IDs
        let (min_id, max_id): (Option<i64>, Option<i64>) = conn
            .query_row("SELECT MIN(id), MAX(id) FROM segments", [], |row| {
                Ok((row.get(0).ok(), row.get(1).ok()))
            })
            .unwrap_or((None, None));

        if let (Some(min_id), Some(max_id)) = (min_id, max_id) {
            shows.push(ShowInfo {
                name,
                database_file: path.file_name().unwrap().to_string_lossy().to_string(),
                min_id,
                max_id,
            });
        }
    }

    (StatusCode::OK, axum::Json(ShowsList { shows })).into_response()
}

#[derive(Serialize)]
struct ShowMetadata {
    unique_id: String,
    name: String,
    audio_format: String,
    split_interval: String,
    bitrate: String,
    sample_rate: String,
    version: String,
    is_recipient: bool,
    min_id: i64,
    max_id: i64,
}

async fn sync_show_metadata_handler(
    State(state): State<StdArc<AppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    // Construct database path
    let db_path = crate::db::get_db_path(&state.output_dir, &show_name);
    let path = std::path::Path::new(&db_path);

    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("Show '{}' not found", show_name)})),
        )
            .into_response();
    }

    // Open database
    let conn = match state.open_readonly(path) {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", db_path, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to open database: {}", e)})),
            )
                .into_response();
        }
    };

    // Check is_recipient flag - reject if true
    let is_recipient: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'is_recipient'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(is_recipient) = &is_recipient {
        if is_recipient == "true" {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": "Cannot sync from a recipient database"})),
            )
                .into_response();
        }
    }

    // Fetch all required metadata
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
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
            )
                .into_response();
        }
    };

    let name: String =
        match conn.query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
            row.get(0)
        }) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to query name metadata: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
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
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
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
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
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
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
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
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
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
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
            )
                .into_response();
        }
    };

    // Get min/max segment IDs
    let (min_id, max_id): (i64, i64) =
        match conn.query_row("SELECT MIN(id), MAX(id) FROM segments", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        }) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to query min/max segment IDs: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
                )
                    .into_response();
            }
        };

    let metadata = ShowMetadata {
        unique_id,
        name,
        audio_format,
        split_interval,
        bitrate,
        sample_rate,
        version,
        is_recipient: is_recipient.map(|v| v == "true").unwrap_or(false),
        min_id,
        max_id,
    };

    (StatusCode::OK, axum::Json(metadata)).into_response()
}

#[derive(Deserialize)]
struct SyncSegmentsQuery {
    start_id: i64,
    end_id: i64,
    limit: Option<u64>,
}

#[derive(Serialize)]
struct SegmentData {
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    #[serde(with = "serde_bytes")]
    audio_data: Vec<u8>,
    section_id: i64,
}

async fn sync_show_segments_handler(
    State(state): State<StdArc<AppState>>,
    Path(show_name): Path<String>,
    Query(query): Query<SyncSegmentsQuery>,
) -> impl IntoResponse {
    // Construct database path
    let db_path = crate::db::get_db_path(&state.output_dir, &show_name);
    let path = std::path::Path::new(&db_path);

    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("Show '{}' not found", show_name)})),
        )
            .into_response();
    }

    // Open database
    let conn = match state.open_readonly(path) {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to open readonly database connection at '{}': {}", db_path, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to open database: {}", e)})),
            )
                .into_response();
        }
    };

    // Check is_recipient flag - reject if true
    let is_recipient: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'is_recipient'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(is_recipient) = is_recipient {
        if is_recipient == "true" {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": "Cannot sync from a recipient database"})),
            )
                .into_response();
        }
    }

    // Fetch segments
    let limit = query.limit.unwrap_or(100);
    let mut stmt = match conn.prepare(
        "SELECT id, timestamp_ms, is_timestamp_from_source, audio_data, section_id FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id LIMIT ?3"
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to prepare query: {}", e)})),
            )
                .into_response();
        }
    };

    let segments_iter = match stmt.query_map(
        rusqlite::params![query.start_id, query.end_id, limit],
        |row| {
            Ok(SegmentData {
                id: row.get(0)?,
                timestamp_ms: row.get(1)?,
                is_timestamp_from_source: row.get(2)?,
                audio_data: row.get(3)?,
                section_id: row.get(4)?,
            })
        },
    ) {
        Ok(iter) => iter,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(
                    serde_json::json!({"error": format!("Failed to query segments: {}", e)}),
                ),
            )
                .into_response();
        }
    };

    let mut segments = Vec::new();
    for segment in segments_iter {
        match segment {
            Ok(seg) => segments.push(seg),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(
                        serde_json::json!({"error": format!("Failed to fetch segment: {}", e)}),
                    ),
                )
                    .into_response();
            }
        }
    }

    (StatusCode::OK, axum::Json(segments)).into_response()
}
