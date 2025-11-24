use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use crate::sftp::{SftpClient, SftpConfig, UploadOptions};
use fs2::FileExt;
use log::{error, warn};
use ogg::writing::PacketWriter;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use tower_http::cors::{Any, CorsLayer};

use crate::audio::{create_opus_comment_header_with_duration, create_opus_id_header};

// Import ShowLocks and get_show_lock from the crate root
// (defined in both lib.rs and main.rs)
use crate::{ShowLocks, get_show_lock};

// State for record mode API handlers
pub struct AppState {
    pub output_dir: PathBuf,
    pub sftp_config: Option<crate::config::SftpExportConfig>,
    pub credentials: Option<crate::credentials::Credentials>,
    pub show_locks: ShowLocks,
    pub db_paths: std::collections::HashMap<String, String>,
}

impl AppState {
    /// Open a readonly connection (always in mutable mode for active recording databases)
    pub fn open_readonly(&self, path: impl AsRef<std::path::Path>) -> Result<rusqlite::Connection, Box<dyn std::error::Error>> {
        crate::db::open_readonly_connection(path)
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
    show_locks: ShowLocks,
    db_paths: std::collections::HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting multi-show API server");
    println!("Output directory: {}", output_dir.display());
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
            output_dir: output_dir.clone(),
            sftp_config,
            credentials: credentials.clone(),
            show_locks: show_locks.clone(),
            db_paths: db_paths.clone(),
        });

        // Spawn periodic export task if enabled
        if export_to_remote_periodically {
            if let Some(sftp_cfg) = sftp_config_for_export {
                println!("Periodic remote export: ENABLED (every hour)");
                spawn_periodic_export_task(
                    output_dir.clone(),
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

// Health check endpoint - returns 200 OK if server is running
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

// Sync handler structs and functions

#[derive(Serialize)]
struct ShowInfo {
    name: String,
    database_file: String,
    min_id: i64,
    max_id: i64,
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

#[derive(Deserialize)]
pub struct SyncSegmentsQuery {
    pub start_id: i64,
    pub end_id: i64,
    pub limit: Option<u64>,
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

pub async fn sync_shows_list_handler(State(state): State<StdArc<AppState>>) -> impl IntoResponse {
    // Scan output directory for .sqlite files
    let dir_path = &state.output_dir;

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
                database_file: path.file_name().unwrap().to_str().unwrap().to_string(),
                min_id,
                max_id,
            });
        }
    }

    (StatusCode::OK, axum::Json(ShowsList { shows })).into_response()
}

pub async fn sync_show_metadata_handler(
    State(state): State<StdArc<AppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    // Get database path from pre-initialized HashMap
    let db_path = match state.db_paths.get(&show_name) {
        Some(path) => path.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": format!("Show '{}' not found", show_name)})),
            ).into_response();
        }
    };
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

pub async fn db_sections_handler(
    State(state): State<StdArc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Get database path from pre-initialized HashMap
    let db_path = match state.db_paths.get(&name) {
        Some(path) => path.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": format!("Show '{}' not found", name)})),
            ).into_response();
        }
    };
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

pub async fn sync_show_segments_handler(
    State(state): State<StdArc<AppState>>,
    Path(show_name): Path<String>,
    Query(query): Query<SyncSegmentsQuery>,
) -> impl IntoResponse {
    // Get database path from pre-initialized HashMap
    let db_path = match state.db_paths.get(&show_name) {
        Some(path) => path.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": format!("Show '{}' not found", show_name)})),
            ).into_response();
        }
    };
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
    // Construct remote path as string (SFTP paths are not local filesystem paths)
    let remote_path_str = format!("{}/{}", config.remote_dir.trim_end_matches('/'), filename);
    let remote_path = std::path::Path::new(&remote_path_str);
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
    output_dir: PathBuf,
    sftp_config: crate::config::SftpExportConfig,
    session_names: Vec<String>,
    credentials: Option<crate::credentials::Credentials>,
    show_locks: ShowLocks,
) {
    tokio::task::spawn_blocking(move || {
        loop {
            println!("Starting periodic export of unexported sections...");

            // Process each session database
            for show_name in &session_names {
                let db_path = output_dir.join(format!("{}.sqlite", show_name));

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
                        let show_lock = get_show_lock(&show_locks, show_name);
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

/// Convert a number to URL-safe base64 encoding
/// Uses A-Za-z0-9-_ character set for compact, URL-safe representation
/// Strips leading 'A's which represent zero bytes in the encoding
fn to_url_safe_base64(num: i64) -> String {
    // Convert i64 to bytes (big-endian for consistent ordering)
    let bytes = num.to_be_bytes();

    // Encode to URL-safe base64 without padding
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bytes)
        .trim_start_matches('A') // Remove leading 'A's (which represent zero bytes)
        .to_string()
}

/// Generate standardized filename for exported section
///
/// Format: {show_name}_{yyyymmdd_hhmmss_fff}_{base64url_section_id}.{extension}
/// Example: am1430_20250122_143000_123_Y0SxI2lA.ogg
fn generate_export_filename(
    show_name: &str,
    start_timestamp_ms: i64,
    section_id: i64,
    audio_format: &str,
) -> Result<String, String> {
    // Format timestamp as yyyymmdd_hhmmss_fff (with milliseconds padded to 3 digits)
    let datetime = chrono::DateTime::from_timestamp_millis(start_timestamp_ms);
    let formatted_time = match datetime {
        Some(dt) => {
            let millis = (start_timestamp_ms % 1000) as u32;
            format!("{}_{:03}", dt.format("%Y%m%d_%H%M%S"), millis)
        }
        None => format!("{}", start_timestamp_ms),
    };

    // Format section_id as URL-safe base64 for compact representation
    let compact_section_id = to_url_safe_base64(section_id);

    // Determine extension
    let extension = match audio_format {
        "opus" => "ogg",
        "aac" => "aac",
        _ => return Err(format!("Unsupported audio format: {}", audio_format)),
    };

    Ok(format!("{}_{}_{}.{}", show_name, formatted_time, compact_section_id, extension))
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
    output_dir: &std::path::Path,
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
    let db_path = output_dir.join(format!("{}.sqlite", show_name));

    if !db_path.exists() {
        return Err(format!("Database '{}' not found", show_name));
    }

    // Open read/write database connection (needed for updating is_exported_to_remote)
    let conn = crate::db::open_database_connection(&db_path)
        .map_err(|e| {
            error!("Failed to open database connection at '{}': {}", db_path.display(), e);
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
        // Generate filename using helper function
        let filename = generate_export_filename(show_name, start_timestamp_ms, section_id, &audio_format)?;

        // Construct remote path
        let sftp_cfg = sftp_config.unwrap();
        let remote_path_str = format!("{}/{}", sftp_cfg.remote_dir.trim_end_matches('/'), filename);

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

    // Generate filename using helper function
    let filename = generate_export_filename(show_name, start_timestamp_ms, section_id, &audio_format)?;

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
    let show_lock = get_show_lock(&state.show_locks, &show_name);
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
