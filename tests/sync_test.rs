//! # Sync Integration Tests
//!
//! These tests verify the sync functionality that transfers audio data from a remote
//! SQLite-based recording server to a local PostgreSQL database (receiver mode).
//!
//! ## Prerequisites
//!
//! 1. **PostgreSQL Server**: A running PostgreSQL instance accessible locally
//! 2. **Database User**: A PostgreSQL user with CREATE DATABASE privileges
//!
//! ## Setup
//!
//! ### macOS (Homebrew)
//! ```bash
//! brew install postgresql@15
//! brew services start postgresql@15
//! createuser -s $(whoami)  # Create superuser with your username
//! ```
//!
//! ### Linux (Ubuntu/Debian)
//! ```bash
//! sudo apt install postgresql postgresql-contrib
//! sudo systemctl start postgresql
//! sudo -u postgres createuser -s $(whoami)
//! ```
//!
//! ## Running the Tests
//!
//! Set the required environment variables and run with `--ignored` flag:
//!
//! ```bash
//! TEST_POSTGRES_URL=postgres://your_user@localhost:5432 \
//! TEST_POSTGRES_PASSWORD=your_password \
//! cargo test --test sync_test -- --ignored
//! ```
//!
//! ### Environment Variables
//!
//! | Variable | Description | Example |
//! |----------|-------------|---------|
//! | `TEST_POSTGRES_URL` | PostgreSQL connection URL (without database name) | `postgres://it3@localhost:5432` |
//! | `TEST_POSTGRES_PASSWORD` | Password for the PostgreSQL user | `mypassword` |
//!
//! ## Test Databases
//!
//! The tests automatically create and drop PostgreSQL databases with the naming pattern:
//! `save_audio_test_{show_name}` (e.g., `save_audio_test_test_new_show`, `save_audio_test_test_incremental`)
//!
//! Each test uses a unique show name to allow parallel test execution without conflicts.
//!
//! ## What the Tests Cover
//!
//! - `test_sync_new_show`: Syncing a new show from scratch
//! - `test_sync_incremental`: Re-syncing an already synced show (idempotent)
//! - `test_sync_with_whitelist`: Syncing only specific shows from a multi-show server
//! - `test_sync_metadata_validation`: Detecting metadata mismatches between source and target
//! - `test_sync_rejects_old_version`: Rejecting sync from incompatible schema versions
//! - `test_sync_rejects_recipient_database`: Preventing sync from a recipient (already synced) database

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use save_audio_stream::config::{ConfigType, DatabaseConfig, ShowConfig, SyncConfig};
use save_audio_stream::db_postgres::{self, GLOBAL_DATABASE_NAME};
use save_audio_stream::queries::{metadata, sections, segments};
use save_audio_stream::segment_wire::{self, WireSegment};
use save_audio_stream::sync::{replace_source, sync_shows, ReplaceSourceResult};
use save_audio_stream::EXPECTED_DB_VERSION;
use sqlx::postgres::PgPool;

/// Database prefix used for all test databases
const TEST_DATABASE_PREFIX: &str = "test";

/// Helper to create a SyncConfig for testing
fn create_test_sync_config(
    remote_url: String,
    postgres_url: String,
    shows: Option<Vec<String>>,
    chunk_size: u64,
    lease_name: &str,
) -> SyncConfig {
    SyncConfig {
        config_type: ConfigType::Receiver,
        remote_url,
        database: DatabaseConfig {
            url: postgres_url,
            credential_profile: "test".to_string(),
            prefix: TEST_DATABASE_PREFIX.to_string(),
        },
        shows: shows.map(|names| {
            names
                .into_iter()
                .map(|name| ShowConfig {
                    name,
                    retention_hours: None,
                })
                .collect()
        }),
        chunk_size: Some(chunk_size),
        port: 8080,
        sync_interval_seconds: 60,
        lease_name: Some(lease_name.to_string()),
    }
}

/// Get PostgreSQL test configuration from environment
fn get_test_postgres_config() -> Option<(String, String)> {
    let postgres_url = std::env::var("TEST_POSTGRES_URL").ok()?;
    let password = std::env::var("TEST_POSTGRES_PASSWORD").ok()?;
    Some((postgres_url, password))
}

/// Create the global PostgreSQL pool for lease management
async fn create_global_pool(postgres_url: &str, password: &str) -> PgPool {
    let pool = db_postgres::open_postgres_connection_create_if_needed(
        postgres_url,
        password,
        GLOBAL_DATABASE_NAME,
    )
    .await
    .unwrap();
    db_postgres::create_leases_table_pg(&pool).await.unwrap();
    pool
}

/// Test metadata structure
#[derive(Debug, Serialize)]
struct ShowMetadata {
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

#[derive(Debug, Serialize)]
struct ShowInfo {
    name: String,
}

#[derive(Debug, Serialize)]
struct ShowsList {
    shows: Vec<ShowInfo>,
}

#[derive(Debug, Serialize)]
struct SectionInfo {
    id: i64,
    start_timestamp_ms: i64,
}

#[derive(Debug, Deserialize)]
struct SegmentQueryParams {
    start_id: i64,
    end_id: i64,
    #[allow(dead_code)]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FindSectionByTimestampParams {
    timestamp_ms: i64,
}

#[derive(Debug, Serialize)]
struct SectionMatch {
    id: i64,
    start_timestamp_ms: i64,
}

#[derive(Debug, Serialize)]
struct FindSectionResponse {
    after_section: Option<SectionMatch>,
    before_or_equal_section: Option<SectionMatch>,
    source_unique_id: String,
    min_id: i64,
    max_id: i64,
}

#[derive(Debug, Serialize)]
struct SectionSegmentRangeResponse {
    min_id: Option<i64>,
    max_id: Option<i64>,
}

/// Shared state for test server
struct TestServerState {
    databases: Arc<Mutex<HashMap<String, SqlitePool>>>,
}

/// Helper to create a source database with test data
async fn create_source_database(
    show_name: &str,
    unique_id: &str,
    num_sections: usize,
    segments_per_section: usize,
) -> (SqlitePool, tempfile::TempDir) {
    let (pool, guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
        .await
        .unwrap();

    // Create schema using common helper
    save_audio_stream::db::init_database_schema(&pool)
        .await
        .unwrap();

    // Insert metadata
    let sql = metadata::insert("version", EXPECTED_DB_VERSION);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("unique_id", unique_id);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("name", show_name);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("audio_format", "opus");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("split_interval", "300");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("bitrate", "16");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("sample_rate", "48000");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    // Insert test sections and segments
    let base_timestamp_ms = 1700000000000i64; // Some timestamp

    for sec_idx in 0..num_sections {
        let section_id = (base_timestamp_ms + sec_idx as i64 * 1000000) * 1000; // microseconds
        let section_timestamp_ms = base_timestamp_ms + sec_idx as i64 * 300000; // 5 min intervals

        // Insert section
        let sql = sections::insert(section_id, section_timestamp_ms);
        sqlx::query(&sql).execute(&pool).await.unwrap();

        // Insert segments for this section
        for seg_idx in 0..segments_per_section {
            let is_boundary = seg_idx == 0;
            let segment_timestamp_ms = section_timestamp_ms + seg_idx as i64 * 1000;
            let audio_data = format!("audio_data_sec{}_seg{}", sec_idx, seg_idx).into_bytes();

            let sql = segments::insert(
                segment_timestamp_ms,
                is_boundary,
                section_id,
                &audio_data,
                0,
            );
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    }

    (pool, guard)
}

/// API handler: List shows
async fn list_shows_handler(State(state): State<Arc<TestServerState>>) -> impl IntoResponse {
    let databases = state.databases.lock().await;
    let shows: Vec<ShowInfo> = databases
        .keys()
        .map(|name| ShowInfo { name: name.clone() })
        .collect();

    Json(ShowsList { shows })
}

/// API handler: Get show metadata
async fn get_metadata_handler(
    State(state): State<Arc<TestServerState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let databases = state.databases.lock().await;

    let pool = match databases.get(&show_name) {
        Some(pool) => pool,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    // Check is_recipient flag - reject if true
    let is_recipient: Option<String> =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'is_recipient'")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    if let Some(is_recipient) = &is_recipient {
        if is_recipient == "true" {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "Cannot sync from a recipient database"})),
            )
                .into_response();
        }
    }

    // Fetch metadata
    let unique_id: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'unique_id'")
            .fetch_one(pool)
            .await
            .unwrap();
    let name: String = sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'name'")
        .fetch_one(pool)
        .await
        .unwrap();
    let audio_format: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'audio_format'")
            .fetch_one(pool)
            .await
            .unwrap();
    let split_interval: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'split_interval'")
            .fetch_one(pool)
            .await
            .unwrap();
    let bitrate: String = sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'bitrate'")
        .fetch_one(pool)
        .await
        .unwrap();
    let sample_rate: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'sample_rate'")
            .fetch_one(pool)
            .await
            .unwrap();
    let version: String = sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'version'")
        .fetch_one(pool)
        .await
        .unwrap();

    // Get min/max segment IDs
    let min_id: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM segments")
        .fetch_optional(pool)
        .await
        .unwrap();
    let max_id: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM segments")
        .fetch_optional(pool)
        .await
        .unwrap();

    let metadata = ShowMetadata {
        unique_id,
        name,
        audio_format,
        split_interval,
        bitrate,
        sample_rate,
        version,
        min_id: min_id.unwrap_or(0),
        max_id: max_id.unwrap_or(0),
    };

    Json(metadata).into_response()
}

/// API handler: Get sections
async fn get_sections_handler(
    State(state): State<Arc<TestServerState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let databases = state.databases.lock().await;

    let pool = match databases.get(&show_name) {
        Some(pool) => pool,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    let rows = sqlx::query("SELECT id, start_timestamp_ms FROM sections ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap();

    let sections: Vec<SectionInfo> = rows
        .iter()
        .map(|row| SectionInfo {
            id: row.get(0),
            start_timestamp_ms: row.get(1),
        })
        .collect();

    Json(sections).into_response()
}

/// API handler: Get segments in range
async fn get_segments_handler(
    State(state): State<Arc<TestServerState>>,
    Path(show_name): Path<String>,
    Query(params): Query<SegmentQueryParams>,
) -> impl IntoResponse {
    let databases = state.databases.lock().await;

    let pool = match databases.get(&show_name) {
        Some(pool) => pool,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    let rows = sqlx::query(
        "SELECT id, timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples
         FROM segments
         WHERE id >= ? AND id <= ?
         ORDER BY id",
    )
    .bind(params.start_id)
    .bind(params.end_id)
    .fetch_all(pool)
    .await
    .unwrap();

    let segments: Vec<WireSegment> = rows
        .iter()
        .map(|row| WireSegment {
            id: row.get(0),
            timestamp_ms: row.get(1),
            is_timestamp_from_source: row.get(2),
            audio_data: row.get(3),
            section_id: row.get(4),
            duration_samples: row.get::<Option<i64>, _>(5).unwrap_or(0),
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

/// API handler: Find section by timestamp
async fn find_section_by_timestamp_handler(
    State(state): State<Arc<TestServerState>>,
    Path(show_name): Path<String>,
    Query(params): Query<FindSectionByTimestampParams>,
) -> impl IntoResponse {
    let databases = state.databases.lock().await;

    let pool = match databases.get(&show_name) {
        Some(pool) => pool,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    // Get source unique_id
    let unique_id: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'unique_id'")
            .fetch_one(pool)
            .await
            .unwrap();

    // Get min/max segment IDs
    let min_id: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM segments")
        .fetch_optional(pool)
        .await
        .unwrap();
    let max_id: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM segments")
        .fetch_optional(pool)
        .await
        .unwrap();

    // Find section after timestamp
    let after_section: Option<SectionMatch> = sqlx::query(
        "SELECT id, start_timestamp_ms FROM sections WHERE start_timestamp_ms > ? ORDER BY start_timestamp_ms ASC LIMIT 1",
    )
    .bind(params.timestamp_ms)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|row| SectionMatch {
        id: row.get(0),
        start_timestamp_ms: row.get(1),
    });

    // Find section before or equal to timestamp
    let before_or_equal_section: Option<SectionMatch> = sqlx::query(
        "SELECT id, start_timestamp_ms FROM sections WHERE start_timestamp_ms <= ? ORDER BY start_timestamp_ms DESC LIMIT 1",
    )
    .bind(params.timestamp_ms)
    .fetch_optional(pool)
    .await
    .unwrap()
    .map(|row| SectionMatch {
        id: row.get(0),
        start_timestamp_ms: row.get(1),
    });

    let response = FindSectionResponse {
        after_section,
        before_or_equal_section,
        source_unique_id: unique_id,
        min_id: min_id.unwrap_or(0),
        max_id: max_id.unwrap_or(-1),
    };

    Json(response).into_response()
}

/// API handler: Get segment range for a section
async fn section_segment_range_handler(
    State(state): State<Arc<TestServerState>>,
    Path((show_name, section_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let databases = state.databases.lock().await;

    let pool = match databases.get(&show_name) {
        Some(pool) => pool,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    let row = sqlx::query("SELECT MIN(id), MAX(id) FROM segments WHERE section_id = ?")
        .bind(section_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    let response = match row {
        Some(row) => SectionSegmentRangeResponse {
            min_id: row.get(0),
            max_id: row.get(1),
        },
        None => SectionSegmentRangeResponse {
            min_id: None,
            max_id: None,
        },
    };

    Json(response).into_response()
}

/// Start a test HTTP server
async fn start_test_server(
    databases: HashMap<String, SqlitePool>,
) -> (String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(TestServerState {
        databases: Arc::new(Mutex::new(databases)),
    });

    let app = Router::new()
        .route("/api/sync/shows", get(list_shows_handler))
        .route("/api/sync/shows/{show}/metadata", get(get_metadata_handler))
        .route("/api/sync/shows/{show}/sections", get(get_sections_handler))
        .route("/api/sync/shows/{show}/segments", get(get_segments_handler))
        .route(
            "/api/sync/shows/{show}/sections/find_by_timestamp",
            get(find_section_by_timestamp_handler),
        )
        .route(
            "/api/sync/shows/{show}/sections/{section_id}/segment_range",
            get(section_segment_range_handler),
        )
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    (url, handle)
}

/// Helper to verify destination database in PostgreSQL
async fn verify_destination_db_pg(
    postgres_url: &str,
    password: &str,
    show_name: &str,
    expected_source_unique_id: &str,
    expected_num_segments: usize,
    expected_num_sections: usize,
) {
    let database_name = save_audio_stream::sync::get_pg_database_name(TEST_DATABASE_PREFIX, show_name);
    let pool = save_audio_stream::db_postgres::open_postgres_connection(
        postgres_url,
        password,
        &database_name,
    )
    .await
    .unwrap();

    // Verify metadata
    let source_unique_id: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'source_unique_id'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(source_unique_id, expected_source_unique_id);

    let name: String = sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'name'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, show_name);

    let is_recipient: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'is_recipient'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(is_recipient, "true");

    // Verify segment count
    let segment_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM segments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(segment_count, expected_num_segments as i64);

    // Verify section count
    let section_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sections")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(section_count, expected_num_sections as i64);
}

/// Helper to drop a test database
async fn drop_test_database(postgres_url: &str, password: &str, show_name: &str) {
    let database_name = save_audio_stream::sync::get_pg_database_name(TEST_DATABASE_PREFIX, show_name);
    let _ = save_audio_stream::db_postgres::drop_database_if_exists(
        postgres_url,
        password,
        &database_name,
    )
    .await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_sync_new_show() {
    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_new_show";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create source database with 3 sections, 5 segments each
    let (source_db, _db_guard) = create_source_database(show_name, "source_unique_123", 3, 5).await;

    // Start test server
    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Sync to destination (spawn blocking since sync_shows uses blocking reqwest client)
    let config = create_test_sync_config(server_url, postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_ok(), "Sync failed: {:?}", result.err());

    // Verify destination database in PostgreSQL
    verify_destination_db_pg(
        &postgres_url,
        &password,
        show_name,
        "source_unique_123",
        15, // 3 sections * 5 segments
        3,  // 3 sections
    )
    .await;

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_sync_incremental() {
    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_incremental";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create initial source database with 2 sections
    let (source_db, _db_guard) = create_source_database(show_name, "source_unique_456", 2, 5).await;

    // Start test server
    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let config = create_test_sync_config(server_url.clone(), postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool_clone).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Verify initial sync
    verify_destination_db_pg(
        &postgres_url,
        &password,
        show_name,
        "source_unique_456",
        10,
        2,
    )
    .await;

    // Now add more data to source and sync again
    // (In a real scenario, we'd update the source DB and restart server)
    // For this test, we verify that re-syncing doesn't break anything
    let config = create_test_sync_config(server_url, postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Should still have same data
    verify_destination_db_pg(
        &postgres_url,
        &password,
        show_name,
        "source_unique_456",
        10,
        2,
    )
    .await;

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_sync_with_whitelist() {
    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    // Clean up any existing test databases
    drop_test_database(&postgres_url, &password, "show1").await;
    drop_test_database(&postgres_url, &password, "show2").await;
    drop_test_database(&postgres_url, &password, "show3").await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create multiple source databases
    let (source_db1, _guard1) = create_source_database("show1", "unique_1", 2, 3).await;
    let (source_db2, _guard2) = create_source_database("show2", "unique_2", 2, 3).await;
    let (source_db3, _guard3) = create_source_database("show3", "unique_3", 2, 3).await;

    // Start test server
    let mut databases = HashMap::new();
    databases.insert("show1".to_string(), source_db1);
    databases.insert("show2".to_string(), source_db2);
    databases.insert("show3".to_string(), source_db3);
    let (server_url, _handle) = start_test_server(databases).await;

    // Sync only show1 and show3
    let config = create_test_sync_config(
        server_url,
        postgres_url.clone(),
        Some(vec!["show1".to_string(), "show3".to_string()]),
        100,
        "test_whitelist",
    );
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Verify show1 exists in PostgreSQL
    verify_destination_db_pg(&postgres_url, &password, "show1", "unique_1", 6, 2).await;

    // Verify show2 does NOT exist (connection should fail)
    let show2_db_name = save_audio_stream::sync::get_pg_database_name(TEST_DATABASE_PREFIX, "show2");
    let show2_result = save_audio_stream::db_postgres::open_postgres_connection(
        &postgres_url,
        &password,
        &show2_db_name,
    )
    .await;
    assert!(show2_result.is_err(), "show2 database should not exist");

    // Verify show3 exists in PostgreSQL
    verify_destination_db_pg(&postgres_url, &password, "show3", "unique_3", 6, 2).await;

    // Cleanup
    drop_test_database(&postgres_url, &password, "show1").await;
    drop_test_database(&postgres_url, &password, "show3").await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_sync_metadata_validation() {
    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_metadata_validation";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create source database
    let (source_db, _db_guard) = create_source_database(show_name, "source_unique_789", 2, 5).await;

    // Start test server
    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let config = create_test_sync_config(server_url.clone(), postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool_clone).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Manually tamper with destination metadata in PostgreSQL to cause validation failure
    let database_name = save_audio_stream::sync::get_pg_database_name(TEST_DATABASE_PREFIX, show_name);
    let pool = save_audio_stream::db_postgres::open_postgres_connection(
        &postgres_url,
        &password,
        &database_name,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE metadata SET value = 'aac' WHERE key = 'audio_format'")
        .execute(&pool)
        .await
        .unwrap();
    drop(pool);

    // Try to sync again - should fail due to metadata mismatch
    let config = create_test_sync_config(server_url, postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(err_msg.contains("Audio format mismatch") || err_msg.contains("mismatch"));

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_sync_rejects_old_version() {
    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_old_version";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create source database with old version (version "2" instead of "3")
    let (pool, _db_guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
        .await
        .unwrap();

    // Create old schema (version 2)
    sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sections (
            id INTEGER PRIMARY KEY,
            start_timestamp_ms INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE,
            duration_samples INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert old version metadata
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('version', '2')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('unique_id', 'old_source')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO metadata (key, value) VALUES ('name', '{}')",
        show_name
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('split_interval', '300')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('bitrate', '16')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert section first (required for foreign key)
    sqlx::query("INSERT INTO sections (id, start_timestamp_ms) VALUES (1, 1700000000000)")
        .execute(&pool)
        .await
        .unwrap();

    // Insert some test data
    sqlx::query(
        "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples)
         VALUES (1700000000000, 1, X'746573745f64617461', 1, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Start test server with old database
    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), pool);
    let (server_url, _handle) = start_test_server(databases).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Try to sync - should fail due to version mismatch
    let config = create_test_sync_config(server_url, postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(
        err_msg.contains("unsupported") || err_msg.contains("schema version"),
        "Expected version error but got: {}",
        err_msg
    );

    // Cleanup (database may not have been created due to version rejection)
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_sync_rejects_recipient_database() {
    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_recipient_reject";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create source database marked as recipient (sync target)
    let (pool, _db_guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
        .await
        .unwrap();

    // Create schema
    sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sections (
            id INTEGER PRIMARY KEY,
            start_timestamp_ms INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL REFERENCES sections(id),
            duration_samples INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert metadata with is_recipient=true
    let sql = metadata::insert("version", EXPECTED_DB_VERSION);
    sqlx::query(&sql).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('unique_id', 'recipient_db')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO metadata (key, value) VALUES ('name', '{}')",
        show_name
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('split_interval', '300')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('bitrate', '16')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('is_recipient', 'true')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert test section and segment
    let section_id = 1700000000000i64 * 1000;
    sqlx::query("INSERT INTO sections (id, start_timestamp_ms) VALUES (?, ?)")
        .bind(section_id)
        .bind(1700000000000i64)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples)
         VALUES (1700000000000, 1, X'746573745f617564696f5f64617461', ?, 0)",
    )
    .bind(section_id)
    .execute(&pool)
    .await
    .unwrap();

    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), pool);
    let (server_url, _handle) = start_test_server(databases).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Try to sync - should fail with forbidden error
    let config = create_test_sync_config(server_url, postgres_url.clone(), None, 100, show_name);
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    // The error could be "Cannot sync from a recipient database" (from server)
    // or a network/parsing error if the server returned 403 status
    assert!(
        err_msg.contains("recipient")
            || err_msg.contains("error")
            || err_msg.contains("Failed to parse")
            || err_msg.contains("status"),
        "Expected error when syncing from recipient database but got: {}",
        err_msg
    );

    // Cleanup (database may not have been created due to recipient rejection)
    drop_test_database(&postgres_url, &password, show_name).await;
}

// ============================================================================
// Replace Source Tests
// ============================================================================

/// Helper to create a source database with specific unique_id and configurable section timestamps
async fn create_source_database_with_timestamps(
    show_name: &str,
    unique_id: &str,
    section_timestamps_ms: &[i64],
    segments_per_section: usize,
) -> (SqlitePool, tempfile::TempDir) {
    let (pool, guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
        .await
        .unwrap();

    // Create schema using common helper
    save_audio_stream::db::init_database_schema(&pool)
        .await
        .unwrap();

    // Insert metadata
    let sql = metadata::insert("version", EXPECTED_DB_VERSION);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("unique_id", unique_id);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("name", show_name);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("audio_format", "opus");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("split_interval", "300");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("bitrate", "16");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("sample_rate", "48000");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    // Insert test sections and segments with specific timestamps
    for (sec_idx, &section_timestamp_ms) in section_timestamps_ms.iter().enumerate() {
        let section_id = section_timestamp_ms * 1000; // microseconds

        // Insert section
        let sql = sections::insert(section_id, section_timestamp_ms);
        sqlx::query(&sql).execute(&pool).await.unwrap();

        // Insert segments for this section
        for seg_idx in 0..segments_per_section {
            let is_boundary = seg_idx == 0;
            let segment_timestamp_ms = section_timestamp_ms + seg_idx as i64 * 1000;
            let audio_data = format!("audio_data_sec{}_seg{}", sec_idx, seg_idx).into_bytes();

            let sql = segments::insert(
                segment_timestamp_ms,
                is_boundary,
                section_id,
                &audio_data,
                0,
            );
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    }

    (pool, guard)
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_replace_source_with_forward_match() {
    // Test scenario: new source has a section AFTER receiver's max timestamp
    // Expected: should match the section after

    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_replace_forward";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create initial source and sync
    // Sections at: 1000, 2000, 3000 (ms)
    let (source_db, _guard1) = create_source_database_with_timestamps(
        show_name,
        "old_source_123",
        &[1000, 2000, 3000],
        5,
    )
    .await;

    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync to populate receiver
    let config = create_test_sync_config(
        server_url,
        postgres_url.clone(),
        None,
        100,
        "test_replace_forward",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool_clone).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok(), "Initial sync failed: {:?}", result.err());

    // Now create NEW source with sections at: 2500, 3500, 4000
    // Receiver's max timestamp is 3000, so section at 3500 should be matched (first after 3000)
    let (new_source_db, _guard2) = create_source_database_with_timestamps(
        show_name,
        "new_source_456",
        &[2500, 3500, 4000],
        5,
    )
    .await;

    let mut new_databases = HashMap::new();
    new_databases.insert(show_name.to_string(), new_source_db);
    let (new_server_url, _handle2) = start_test_server(new_databases).await;

    // Replace source
    let config = create_test_sync_config(
        new_server_url,
        postgres_url.clone(),
        Some(vec![show_name.to_string()]),
        100,
        "test_replace_forward",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let show_name_clone = show_name.to_string();
    let result = tokio::task::spawn_blocking(move || {
        replace_source(&config, &password_clone, &global_pool_clone, &show_name_clone)
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    match result {
        Ok(ReplaceSourceResult::Replaced {
            old_source_id,
            new_source_id,
            matched_section_timestamp_ms,
            ..
        }) => {
            assert_eq!(old_source_id, "old_source_123");
            assert_eq!(new_source_id, "new_source_456");
            // Should match section at 3500 (first after 3000)
            assert_eq!(matched_section_timestamp_ms, 3500);
        }
        other => panic!("Expected Replaced result, got: {:?}", other),
    }

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_replace_source_with_backward_match() {
    // Test scenario: new source has NO section after receiver's max timestamp
    // but has sections before it
    // Expected: should match the closest previous section

    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_replace_backward";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create initial source and sync
    // Sections at: 1000, 2000, 3000 (ms) - receiver max will be 3000
    let (source_db, _guard1) = create_source_database_with_timestamps(
        show_name,
        "old_source_abc",
        &[1000, 2000, 3000],
        5,
    )
    .await;

    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let config = create_test_sync_config(
        server_url,
        postgres_url.clone(),
        None,
        100,
        "test_replace_backward",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool_clone).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Create NEW source with sections BEFORE receiver's max timestamp
    // Sections at: 1500, 2500, 2800 - all before 3000
    // Should match 2800 (closest before/equal to 3000)
    let (new_source_db, _guard2) = create_source_database_with_timestamps(
        show_name,
        "new_source_def",
        &[1500, 2500, 2800],
        5,
    )
    .await;

    let mut new_databases = HashMap::new();
    new_databases.insert(show_name.to_string(), new_source_db);
    let (new_server_url, _handle2) = start_test_server(new_databases).await;

    // Replace source
    let config = create_test_sync_config(
        new_server_url,
        postgres_url.clone(),
        Some(vec![show_name.to_string()]),
        100,
        "test_replace_backward",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let show_name_clone = show_name.to_string();
    let result = tokio::task::spawn_blocking(move || {
        replace_source(&config, &password_clone, &global_pool_clone, &show_name_clone)
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    match result {
        Ok(ReplaceSourceResult::Replaced {
            new_source_id,
            matched_section_timestamp_ms,
            ..
        }) => {
            assert_eq!(new_source_id, "new_source_def");
            // Should match section at 2800 (latest before/equal to 3000)
            assert_eq!(matched_section_timestamp_ms, 2800);
        }
        other => panic!("Expected Replaced result, got: {:?}", other),
    }

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_replace_source_empty_receiver() {
    // Test scenario: receiver has no data yet
    // Expected: should return FreshStart

    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_replace_empty_recv";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create empty receiver database (without syncing any data)
    // We need to create the database structure but with no segments/sections
    let database_name =
        save_audio_stream::sync::get_pg_database_name(TEST_DATABASE_PREFIX, show_name);
    let pool = db_postgres::open_postgres_connection_create_if_needed(
        &postgres_url,
        &password,
        &database_name,
    )
    .await
    .unwrap();

    // Initialize schema
    db_postgres::init_database_schema_pg(&pool).await.unwrap();

    // Insert minimal metadata (as a receiver would have)
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('source_unique_id', 'old_source')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO metadata (key, value) VALUES ('name', '{}')",
        show_name
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('is_recipient', 'true')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO metadata (key, value) VALUES ('last_synced_id', '0')")
        .execute(&pool)
        .await
        .unwrap();
    drop(pool);

    // Create new source with some data
    let (new_source_db, _guard) = create_source_database_with_timestamps(
        show_name,
        "new_source_xyz",
        &[1000, 2000, 3000],
        5,
    )
    .await;

    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), new_source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Replace source on empty receiver
    let config = create_test_sync_config(
        server_url,
        postgres_url.clone(),
        Some(vec![show_name.to_string()]),
        100,
        "test_replace_empty_recv",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let show_name_clone = show_name.to_string();
    let result = tokio::task::spawn_blocking(move || {
        replace_source(&config, &password_clone, &global_pool_clone, &show_name_clone)
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    match result {
        Ok(ReplaceSourceResult::FreshStart { new_source_id }) => {
            assert_eq!(new_source_id, "new_source_xyz");
        }
        other => panic!("Expected FreshStart result, got: {:?}", other),
    }

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}

#[tokio::test]
#[ignore] // Requires PostgreSQL: TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD
async fn test_replace_source_then_sync() {
    // Test scenario: replace source and then sync from the new source
    // Expected: sync should continue from the matched section

    let (postgres_url, password) = get_test_postgres_config()
        .expect("TEST_POSTGRES_URL and TEST_POSTGRES_PASSWORD must be set");

    let show_name = "test_replace_then_sync";

    // Clean up any existing test database
    drop_test_database(&postgres_url, &password, show_name).await;

    // Create global pool for lease management
    let global_pool = create_global_pool(&postgres_url, &password).await;

    // Create initial source and sync
    // Sections at: 1000, 2000 (ms)
    let (source_db, _guard1) =
        create_source_database_with_timestamps(show_name, "old_source_111", &[1000, 2000], 5).await;

    let mut databases = HashMap::new();
    databases.insert(show_name.to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let config = create_test_sync_config(
        server_url,
        postgres_url.clone(),
        None,
        100,
        "test_replace_then_sync",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool_clone).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Verify initial sync
    verify_destination_db_pg(
        &postgres_url,
        &password,
        show_name,
        "old_source_111",
        10, // 2 sections * 5 segments
        2,
    )
    .await;

    // Create NEW source with sections at: 1500, 2500, 3000
    // Receiver's max timestamp is 2000
    // Section at 2500 should match (first after 2000)
    // Note: We use higher timestamps to ensure segment IDs from the new source don't overlap
    // with existing segments in the receiver (this simulates a real scenario where the new
    // source would have different segment IDs). The sync will insert new segments with IDs
    // from the new source, so they must not conflict with existing receiver segment IDs.
    // Since receiver has segments 1-10 from old source, new source needs IDs > 10.
    let (new_source_db, _guard2) = create_source_database_with_timestamps(
        show_name,
        "new_source_222",
        &[1500, 2500, 3000],
        5,
    )
    .await;

    // Offset segment IDs on new source to avoid conflicts with receiver's existing segments
    // In a real scenario, the new source would have entirely different segment IDs
    sqlx::query("UPDATE segments SET id = id + 100")
        .execute(&new_source_db)
        .await
        .unwrap();

    let mut new_databases = HashMap::new();
    new_databases.insert(show_name.to_string(), new_source_db);
    let (new_server_url, _handle2) = start_test_server(new_databases).await;

    // Replace source
    let config = create_test_sync_config(
        new_server_url.clone(),
        postgres_url.clone(),
        Some(vec![show_name.to_string()]),
        100,
        "test_replace_then_sync",
    );
    let password_clone = password.clone();
    let global_pool_clone = global_pool.clone();
    let show_name_clone = show_name.to_string();
    let result = tokio::task::spawn_blocking(move || {
        replace_source(&config, &password_clone, &global_pool_clone, &show_name_clone)
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    match result {
        Ok(ReplaceSourceResult::Replaced {
            matched_section_timestamp_ms,
            ..
        }) => {
            assert_eq!(matched_section_timestamp_ms, 2500);
        }
        other => panic!("Expected Replaced result, got: {:?}", other),
    }

    // Now sync from new source
    let config = create_test_sync_config(
        new_server_url,
        postgres_url.clone(),
        Some(vec![show_name.to_string()]),
        100,
        "test_replace_then_sync",
    );
    let password_clone = password.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(&config, &password_clone, &global_pool).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok(), "Sync after replace failed: {:?}", result.err());

    // Verify: should have original data PLUS new synced data from sections 2500 and 3000
    // Original: 2 sections (1000, 2000) * 5 segments = 10 segments
    // New: 2 sections (2500, 3000) * 5 segments = 10 segments (only syncing from 2500 onwards)
    // Total: 20 segments
    //
    // For sections:
    // - Original: 2 sections (1000, 2000) from old source
    // - New source has 3 sections (1500, 2500, 3000), all get inserted via ON CONFLICT DO NOTHING
    // Total: 5 sections (1000, 1500, 2000, 2500, 3000)
    let database_name =
        save_audio_stream::sync::get_pg_database_name(TEST_DATABASE_PREFIX, show_name);
    let pool =
        db_postgres::open_postgres_connection(&postgres_url, &password, &database_name)
            .await
            .unwrap();

    // Check source_unique_id was updated
    let source_unique_id: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'source_unique_id'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(source_unique_id, "new_source_222");

    // Check segment count - should have old + new data
    let segment_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM segments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(segment_count, 20); // 10 original + 10 new

    // Check section count
    let section_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sections")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(section_count, 5); // 2 original (1000, 2000) + 3 new (1500, 2500, 3000)

    // Cleanup
    drop_test_database(&postgres_url, &password, show_name).await;
}
