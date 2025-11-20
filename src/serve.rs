use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect},
    routing::get,
    Router,
};

#[cfg(not(debug_assertions))]
use axum::{
    http::Uri,
    response::Response,
};

#[cfg(debug_assertions)]
use axum::{
    http::Uri,
    response::Response,
};
use ogg::writing::PacketWriter;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use std::sync::Mutex;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::audio::{create_opus_comment_header_with_duration, create_opus_id_header};
use crate::webm::{write_ebml_binary, write_ebml_float, write_ebml_id, write_ebml_size, write_ebml_string, write_ebml_uint};

#[cfg(not(debug_assertions))]
#[derive(rust_embed::RustEmbed)]
#[folder = "app/dist/"]
struct Asset;

// State for axum handlers
struct AudioSession {
    temp_path: PathBuf,
    file_size: u64,
    expires_at: Instant,
}

struct AppState {
    db_path: String,
    sessions: Mutex<HashMap<String, AudioSession>>,
}

pub fn serve(sqlite_file: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // Verify database exists and is Opus format
    if !sqlite_file.exists() {
        return Err(format!("Database file not found: {}", sqlite_file.display()).into());
    }

    let conn = Connection::open(&sqlite_file)?;

    // Check audio format
    let audio_format: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'audio_format'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "Database missing audio_format metadata")?;

    if audio_format != "opus" {
        return Err(format!(
            "Only Opus format is supported for serving, found: {}",
            audio_format
        )
        .into());
    }

    let db_path = sqlite_file.to_string_lossy().to_string();
    println!("Starting server for: {}", db_path);
    println!("Listening on: http://0.0.0.0:{}", port);
    println!("Endpoints:");
    println!("  GET /audio?start_id=<N>&end_id=<N>  - Ogg/Opus stream");
    println!("  GET /manifest.mpd?start_id=<N>&end_id=<N>  - DASH MPD");
    println!("  GET /init.webm  - WebM initialization segment");
    println!("  GET /segment/:id  - WebM audio segment");

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_state = StdArc::new(AppState {
            db_path,
            sessions: Mutex::new(HashMap::new()),
        });

        // Spawn cleanup task for expired sessions
        let cleanup_state = app_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;

                let expired: Vec<(String, PathBuf)> = {
                    let sessions = cleanup_state.sessions.lock().unwrap();
                    let now = Instant::now();
                    sessions
                        .iter()
                        .filter(|(_, session)| now > session.expires_at)
                        .map(|(id, session)| (id.clone(), session.temp_path.clone()))
                        .collect()
                };

                if !expired.is_empty() {
                    let mut sessions = cleanup_state.sessions.lock().unwrap();
                    for (id, path) in expired {
                        sessions.remove(&id);
                        let _ = std::fs::remove_file(&path);
                        println!("Cleaned up expired session: {}", id);
                    }
                }
            }
        });

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let api_routes = Router::new()
            .route("/audio", get(audio_handler))
            .route("/audio/session/{id}", get(session_handler))
            .route("/manifest.mpd", get(mpd_handler))
            .route("/init.webm", get(init_handler))
            .route("/segment/{id}", get(segment_handler))
            .route("/api/segments/range", get(segments_range_handler))
            .route("/api/sessions", get(sessions_handler));

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

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
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

// Query parameters for audio endpoint
#[derive(Deserialize)]
struct AudioQuery {
    start_id: i64,
    end_id: i64,
}

// Audio endpoint handler - creates session and redirects
async fn audio_handler(
    State(state): State<StdArc<AppState>>,
    Query(query): Query<AudioQuery>,
) -> impl IntoResponse {
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get max id
    let max_id: i64 = match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
        Ok(id) => id,
        Err(_) => return (StatusCode::NOT_FOUND, "No segments in database").into_response(),
    };

    // Validate end_id
    if query.end_id > max_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("end_id {} exceeds max id {}", query.end_id, max_id),
        )
            .into_response();
    }

    if query.start_id > query.end_id {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "start_id {} cannot be greater than end_id {}",
                query.start_id, query.end_id
            ),
        )
            .into_response();
    }

    // Get split_interval to calculate duration limit
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

    let sample_rate: u32 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'sample_rate'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(48_000))
            },
        )
        .unwrap_or(48_000);

    // Check 6 hour limit
    let segment_count = query.end_id - query.start_id + 1;
    let estimated_duration = segment_count as f64 * split_interval;
    const MAX_DURATION_SECS: f64 = 6.0 * 60.0 * 60.0;

    if estimated_duration > MAX_DURATION_SECS {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Requested duration {:.0}s exceeds maximum of {:.0}s",
                estimated_duration, MAX_DURATION_SECS
            ),
        )
            .into_response();
    }

    // Calculate duration from segment count
    let duration_secs = segment_count as f64 * split_interval;
    let samples_per_packet = (sample_rate / 50) as u64;

    // Create temporary file and write Ogg stream
    let temp_file = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create temp file: {}", e),
            )
                .into_response()
        }
    };

    let file = match temp_file.reopen() {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to reopen temp file: {}", e),
            )
                .into_response()
        }
    };

    // Write Ogg stream to file with buffering
    let buf_writer = std::io::BufWriter::new(file);
    let mut buf_writer = match write_ogg_stream(
        &conn,
        query.start_id,
        query.end_id,
        sample_rate,
        duration_secs,
        samples_per_packet,
        buf_writer,
    ) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write audio: {}", e),
            )
                .into_response()
        }
    };

    // Flush buffer and sync to disk
    if let Err(e) = buf_writer.flush() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to flush buffer: {}", e),
        )
            .into_response();
    }
    let file = buf_writer.into_inner().unwrap();
    if let Err(e) = file.sync_all() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to sync file: {}", e),
        )
            .into_response();
    }
    drop(file);

    // Get file size
    let file_size = match std::fs::metadata(temp_file.path()) {
        Ok(m) => m.len(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get file size: {}", e),
            )
                .into_response()
        }
    };

    // Create session and persist temp file
    let session_id = Uuid::new_v4().to_string();
    let temp_path = temp_file.into_temp_path();
    let persisted_path = temp_path.keep().unwrap();

    let session = AudioSession {
        temp_path: persisted_path.clone(),
        file_size,
        expires_at: Instant::now() + std::time::Duration::from_secs(24 * 60 * 60),
    };

    // Store session
    {
        let mut sessions = state.sessions.lock().unwrap();
        sessions.insert(session_id.clone(), session);
    }

    // Redirect to session endpoint
    Redirect::temporary(&format!("/audio/session/{}", session_id)).into_response()
}

// Session handler - serves cached audio file with Range support
async fn session_handler(
    State(state): State<StdArc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    // Look up session
    let (temp_path, file_size) = {
        let sessions = state.sessions.lock().unwrap();
        match sessions.get(&session_id) {
            Some(session) => {
                if Instant::now() > session.expires_at {
                    return (StatusCode::GONE, "Session expired").into_response();
                }
                (session.temp_path.clone(), session.file_size)
            }
            None => {
                return (StatusCode::NOT_FOUND, "Session not found").into_response();
            }
        }
    };

    // Check for Range header
    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_header.to_str() {
            if let Some(range) = range_str.strip_prefix("bytes=") {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    let start = if parts[0].is_empty() {
                        let suffix_len: u64 = parts[1].parse().unwrap_or(0);
                        file_size.saturating_sub(suffix_len)
                    } else {
                        parts[0].parse().unwrap_or(0)
                    };

                    let end = if parts[1].is_empty() || parts[0].is_empty() {
                        file_size - 1
                    } else {
                        parts[1].parse().unwrap_or(file_size - 1).min(file_size - 1)
                    };

                    if start <= end && start < file_size {
                        let mut file = match tokio::fs::File::open(&temp_path).await {
                            Ok(f) => f,
                            Err(e) => {
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("Failed to open file: {}", e),
                                )
                                    .into_response()
                            }
                        };

                        use tokio::io::AsyncSeekExt;
                        if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Failed to seek: {}", e),
                            )
                                .into_response();
                        }

                        let length = end - start + 1;
                        let limited = file.take(length);
                        let stream = ReaderStream::new(limited);
                        let body = Body::from_stream(stream);

                        let content_range = format!("bytes {}-{}/{}", start, end, file_size);

                        let mut response = (StatusCode::PARTIAL_CONTENT, body).into_response();
                        {
                            let headers = response.headers_mut();
                            let _ = headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/ogg"));
                            let _ = headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
                            if let Ok(val) = HeaderValue::from_str(&content_range) {
                                let _ = headers.insert(header::CONTENT_RANGE, val);
                            }
                            if let Ok(val) = HeaderValue::from_str(&length.to_string()) {
                                let _ = headers.insert(header::CONTENT_LENGTH, val);
                            }
                        }
                        return response;
                    }
                }
            }
        }
    }

    // Full file response with streaming
    let file = match tokio::fs::File::open(&temp_path).await {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open file: {}", e),
            )
                .into_response()
        }
    };

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response = (StatusCode::OK, body).into_response();
    {
        let headers = response.headers_mut();
        let _ = headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/ogg"));
        let _ = headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        if let Ok(val) = HeaderValue::from_str(&file_size.to_string()) {
            let _ = headers.insert(header::CONTENT_LENGTH, val);
        }
    }

    response
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
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
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
        Err(_) => return (StatusCode::NOT_FOUND, "No segments in database").into_response(),
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

    // Build DASH MPD
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
        media="segment/$Number$"
        startNumber="{}"
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
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
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
async fn segment_handler(
    State(state): State<StdArc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
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
        [id],
        |row| row.get(0),
    ) {
        Ok(data) => data,
        Err(_) => {
            return (StatusCode::NOT_FOUND, format!("Segment {} not found", id)).into_response()
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

    // Get the first segment ID
    let first_id: i64 = conn
        .query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0))
        .unwrap_or(1);

    let relative_pos = (id - first_id) as u64;
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

async fn segments_range_handler(
    State(state): State<StdArc<AppState>>,
) -> impl IntoResponse {
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    let min_id: Result<i64, _> = conn.query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0));
    let max_id: Result<i64, _> = conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0));

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
        _ => (
            StatusCode::NOT_FOUND,
            "No segments found in database",
        )
            .into_response(),
    }
}

async fn sessions_handler(
    State(state): State<StdArc<AppState>>,
) -> impl IntoResponse {
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response()
        }
    };

    // Get show name from metadata
    let name: String = match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'name'",
        [],
        |row| row.get(0),
    ) {
        Ok(n) => n,
        Err(_) => "Unknown".to_string(),
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

    // Get all boundary segments (is_timestamp_from_source = 1)
    let mut stmt = match conn.prepare(
        "SELECT id, timestamp_ms FROM segments WHERE is_timestamp_from_source = 1 ORDER BY id"
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

    let boundaries: Vec<(i64, i64)> = match stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
    {
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
        return (
            StatusCode::NOT_FOUND,
            "No recording sessions found",
        )
            .into_response();
    }

    // Get max segment ID to handle the last session
    let max_id: i64 = match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get max segment ID",
            )
                .into_response()
        }
    };

    // Build sessions by grouping segments between boundaries
    let mut sessions = Vec::new();
    for i in 0..boundaries.len() {
        let (start_id, timestamp_ms) = boundaries[i];
        let end_id = if i + 1 < boundaries.len() {
            boundaries[i + 1].0 - 1
        } else {
            max_id
        };

        let segment_count = (end_id - start_id + 1) as f64;
        let duration_seconds = segment_count * split_interval;

        sessions.push(SessionInfo {
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
                Err(_) => {
                    (StatusCode::BAD_GATEWAY, "Failed to read response from Vite").into_response()
                }
            }
        }
        Err(_) => {
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to connect to Vite dev server at {}. Make sure to run 'npm run dev' in the app/ directory.", VITE_DEV_SERVER)
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

#[cfg(not(debug_assertions))]
async fn index_handler_release() -> Response {
    match Asset::get("index.html") {
        Some(content) => {
            let mut response = Response::new(Body::from(content.data.into_owned()));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html"),
            );
            response
        }
        None => {
            (StatusCode::NOT_FOUND, "index.html not found").into_response()
        }
    }
}

#[cfg(not(debug_assertions))]
async fn assets_handler_release(Path(path): Path<String>) -> Response {
    let file_path = format!("assets/{}", path);

    match Asset::get(&file_path) {
        Some(content) => {
            let mime_type = mime_guess::from_path(&file_path)
                .first_or_octet_stream()
                .to_string();

            let mut response = Response::new(Body::from(content.data.into_owned()));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime_type).unwrap(),
            );
            response
        }
        None => {
            (StatusCode::NOT_FOUND, "Asset not found").into_response()
        }
    }
}
