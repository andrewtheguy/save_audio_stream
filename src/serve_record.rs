use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use log::{error, warn};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use tower_http::cors::{Any, CorsLayer};

use crate::queries::{metadata, sections, segments};
use crate::segment_wire::{self, WireSegment};

// Import ShowLocks from the crate root
// (defined in both lib.rs and main.rs)
use crate::ShowLocks;

// State for record mode API handlers
pub struct AppState {
    pub output_dir: PathBuf,
    pub show_locks: ShowLocks,
    pub db_paths: std::collections::HashMap<String, PathBuf>,
}

/// Serve multiple databases from a directory (for sync endpoints during recording)
pub fn serve_for_sync(
    output_dir: PathBuf,
    port: u16,
    show_locks: ShowLocks,
    db_paths: std::collections::HashMap<String, PathBuf>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Starting multi-show API server");
    println!("Output directory: {}", output_dir.display());
    println!("Listening on: http://[::]{} (IPv4 + IPv6)", port);
    println!("Endpoints:");
    println!("  GET /health  - Health check");
    println!("  GET /api/sync/shows  - List available shows");
    println!("  GET /api/sync/shows/:name/metadata  - Show metadata");
    println!("  GET /api/sync/shows/:name/sections  - Show sections metadata");
    println!("  GET /api/sync/shows/:name/segments  - Show segments");

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_state = StdArc::new(AppState {
            output_dir: output_dir.clone(),
            show_locks: show_locks.clone(),
            db_paths: db_paths.clone(),
        });

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
    /// Skip segments with timestamp_ms < cutoff_ts (for retention-based sync)
    pub cutoff_ts: Option<i64>,
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
            }
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
        let pool = match crate::db::open_readonly_connection(&path).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to open database {:?} for show listing: {}", path, e);
                continue;
            }
        };

        // Check is_recipient flag
        let is_recipient: Option<String> =
            sqlx::query_scalar::<_, String>(&metadata::select_by_key("is_recipient"))
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten();

        if let Some(is_recipient) = is_recipient {
            if is_recipient == "true" {
                continue; // Skip recipient databases
            }
        }

        // Get name from metadata
        let name: Option<String> =
            sqlx::query_scalar::<_, String>(&metadata::select_by_key("name"))
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten();

        let name = match name {
            Some(n) => n,
            None => continue,
        };

        // Get min/max segment IDs
        let min_max = sqlx::query(&segments::select_min_max_id())
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

        if let Some(row) = min_max {
            let min_id: Option<i64> = row.get(0);
            let max_id: Option<i64> = row.get(1);
            if let (Some(min_id), Some(max_id)) = (min_id, max_id) {
                shows.push(ShowInfo {
                    name,
                    database_file: path.file_name().unwrap().to_str().unwrap().to_string(),
                    min_id,
                    max_id,
                });
            }
        }
    }

    (StatusCode::OK, axum::Json(ShowsList { shows })).into_response()
}

pub async fn sync_show_metadata_handler(
    State(state): State<StdArc<AppState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    // Get database path from pre-initialized HashMap
    let path = match state.db_paths.get(&show_name) {
        Some(p) => p.as_path(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": format!("Show '{}' not found", show_name)})),
            )
                .into_response();
        }
    };

    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("Show '{}' not found", show_name)})),
        )
            .into_response();
    }

    // Open database
    let pool = match crate::db::open_readonly_connection(path).await {
        Ok(p) => p,
        Err(e) => {
            error!(
                "Failed to open readonly database connection at '{}': {}",
                path.display(),
                e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to open database: {}", e)})),
            )
                .into_response();
        }
    };

    // Check is_recipient flag - reject if true
    let is_recipient: Option<String> =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key("is_recipient"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

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
    let unique_id: String =
        match sqlx::query_scalar::<_, String>(&metadata::select_by_key("unique_id"))
            .fetch_one(&pool)
            .await
        {
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

    let name: String = match sqlx::query_scalar::<_, String>(&metadata::select_by_key("name"))
        .fetch_one(&pool)
        .await
    {
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

    let audio_format: String =
        match sqlx::query_scalar::<_, String>(&metadata::select_by_key("audio_format"))
            .fetch_one(&pool)
            .await
        {
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

    let bitrate: String = match sqlx::query_scalar::<_, String>(&metadata::select_by_key("bitrate"))
        .fetch_one(&pool)
        .await
    {
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

    let sample_rate: String =
        match sqlx::query_scalar::<_, String>(&metadata::select_by_key("sample_rate"))
            .fetch_one(&pool)
            .await
        {
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

    let version: String = match sqlx::query_scalar::<_, String>(&metadata::select_by_key("version"))
        .fetch_one(&pool)
        .await
    {
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
    let (min_id, max_id): (i64, i64) = match sqlx::query(&segments::select_min_max_id())
        .fetch_one(&pool)
        .await
    {
        Ok(row) => (row.get(0), row.get(1)),
        Err(e) => {
            error!("Failed to query min/max segment IDs: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Database error: {}", e)})),
            )
                .into_response();
        }
    };

    let metadata_response = ShowMetadata {
        unique_id,
        name,
        audio_format,
        bitrate,
        sample_rate,
        version,
        is_recipient: is_recipient.map(|v| v == "true").unwrap_or(false),
        min_id,
        max_id,
    };

    (StatusCode::OK, axum::Json(metadata_response)).into_response()
}

#[derive(Deserialize)]
pub struct SyncSectionsQuery {
    /// Only return sections with start_timestamp_ms >= cutoff_ts
    pub cutoff_ts: Option<i64>,
}

pub async fn db_sections_handler(
    State(state): State<StdArc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<SyncSectionsQuery>,
) -> impl IntoResponse {
    // Get database path from pre-initialized HashMap
    let path = match state.db_paths.get(&name) {
        Some(p) => p.as_path(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": format!("Show '{}' not found", name)})),
            )
                .into_response();
        }
    };

    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("Database '{}' not found", name)})),
        )
            .into_response();
    }

    // Open database
    let pool = match crate::db::open_readonly_connection(path).await {
        Ok(p) => p,
        Err(e) => {
            error!(
                "Failed to open readonly database connection at '{}': {}",
                path.display(),
                e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to open database: {}", e)})),
            )
                .into_response();
        }
    };

    // Fetch sections (optionally filtered by cutoff timestamp)
    let sql = match query.cutoff_ts {
        Some(cutoff) => sections::select_all_after_cutoff(cutoff),
        None => sections::select_all(),
    };
    let rows = match sqlx::query(&sql).fetch_all(&pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(
                    serde_json::json!({"error": format!("Failed to fetch sections: {}", e)}),
                ),
            )
                .into_response();
        }
    };

    let section_list: Vec<SectionInfo> = rows
        .iter()
        .map(|row| SectionInfo {
            id: row.get(0),
            start_timestamp_ms: row.get(1),
        })
        .collect();

    axum::Json::<Vec<SectionInfo>>(section_list).into_response()
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
            )
                .into_response();
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
    let pool = match crate::db::open_readonly_connection(path).await {
        Ok(p) => p,
        Err(e) => {
            error!(
                "Failed to open readonly database connection at '{}': {}",
                path.display(),
                e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("Failed to open database: {}", e)})),
            )
                .into_response();
        }
    };

    // Check is_recipient flag - reject if true
    let is_recipient: Option<String> =
        sqlx::query_scalar::<_, String>(&metadata::select_by_key("is_recipient"))
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

    if let Some(is_recipient) = is_recipient {
        if is_recipient == "true" {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": "Cannot sync from a recipient database"})),
            )
                .into_response();
        }
    }

    // Fetch segments (optionally filtered by cutoff timestamp)
    let limit = query.limit.unwrap_or(100);
    let sql = match query.cutoff_ts {
        Some(cutoff) => {
            segments::select_range_with_limit_and_cutoff(query.start_id, query.end_id, limit, cutoff)
        }
        None => segments::select_range_with_limit(query.start_id, query.end_id, limit),
    };
    let rows = match sqlx::query(&sql).fetch_all(&pool).await {
        Ok(rows) => rows,
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

    let segments: Vec<WireSegment> = rows
        .iter()
        .map(|row| WireSegment {
            id: row.get(0),
            timestamp_ms: row.get(1),
            is_timestamp_from_source: row.get(2),
            audio_data: row.get(3),
            section_id: row.get(4),
            duration_samples: row.get(5),
        })
        .collect();

    let body = segment_wire::encode_segments(&segments);

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            segment_wire::CONTENT_TYPE,
        )],
        body,
    )
        .into_response()
}
