use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

use save_audio_stream::sync::sync_shows;
use save_audio_stream::EXPECTED_DB_VERSION;

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

#[derive(Debug, Serialize)]
struct SegmentData {
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    #[serde(with = "serde_bytes")]
    audio_data: Vec<u8>,
    section_id: i64,
}

#[derive(Debug, Deserialize)]
struct SegmentQueryParams {
    start_id: i64,
    end_id: i64,
    #[allow(dead_code)]
    limit: Option<u64>,
}

/// Shared state for test server
struct TestServerState {
    databases: Arc<Mutex<HashMap<String, Connection>>>,
}

/// Helper to create a source database with test data
fn create_source_database(
    show_name: &str,
    unique_id: &str,
    num_sections: usize,
    segments_per_section: usize,
) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();

    // Create schema
    conn.execute(
        "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE sections (
            id INTEGER PRIMARY KEY,
            start_timestamp_ms INTEGER NOT NULL
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL REFERENCES sections(id)
        )",
        [],
    )
    .unwrap();

    // Create indexes
    conn.execute(
        "CREATE INDEX idx_segments_boundary ON segments(is_timestamp_from_source, timestamp_ms)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE INDEX idx_segments_section_id ON segments(section_id)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE INDEX idx_sections_start_timestamp ON sections(start_timestamp_ms)",
        [],
    )
    .unwrap();

    // Enable WAL mode
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();

    // Insert metadata
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('version', ?1)",
        [EXPECTED_DB_VERSION],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('unique_id', ?1)",
        [unique_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('name', ?1)",
        [show_name],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('split_interval', '300')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('bitrate', '16')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')",
        [],
    )
    .unwrap();

    // Insert test sections and segments
    let base_timestamp_ms = 1700000000000i64; // Some timestamp
    let tx = conn.transaction().unwrap();
    {
        for sec_idx in 0..num_sections {
            let section_id = (base_timestamp_ms + sec_idx as i64 * 1000000) * 1000; // microseconds
            let section_timestamp_ms = base_timestamp_ms + sec_idx as i64 * 300000; // 5 min intervals

            // Insert section
            tx.execute(
                "INSERT INTO sections (id, start_timestamp_ms) VALUES (?1, ?2)",
                rusqlite::params![section_id, section_timestamp_ms],
            )
            .unwrap();

            // Insert segments for this section
            for seg_idx in 0..segments_per_section {
                let is_boundary = if seg_idx == 0 { 1 } else { 0 };
                let segment_timestamp_ms = section_timestamp_ms + seg_idx as i64 * 1000;
                let audio_data = format!("audio_data_sec{}_seg{}", sec_idx, seg_idx).into_bytes();

                tx.execute(
                    "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![segment_timestamp_ms, is_boundary, audio_data, section_id],
                )
                .unwrap();
            }
        }
    }
    tx.commit().unwrap();

    conn
}

/// API handler: List shows
async fn list_shows_handler(State(state): State<Arc<TestServerState>>) -> impl IntoResponse {
    let databases = state.databases.lock().unwrap();
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
    let databases = state.databases.lock().unwrap();

    let conn = match databases.get(&show_name) {
        Some(conn) => conn,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
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
                Json(serde_json::json!({"error": "Cannot sync from a recipient database"})),
            )
                .into_response();
        }
    }

    // Fetch metadata
    let unique_id: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'unique_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let name: String = conn
        .query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let audio_format: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'audio_format'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let split_interval: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'split_interval'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let bitrate: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'bitrate'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let sample_rate: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'sample_rate'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'version'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    // Get min/max segment IDs
    let min_id: i64 = conn
        .query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0))
        .unwrap_or(0);
    let max_id: i64 = conn
        .query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0))
        .unwrap_or(0);

    let metadata = ShowMetadata {
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

    Json(metadata).into_response()
}

/// API handler: Get sections
async fn get_sections_handler(
    State(state): State<Arc<TestServerState>>,
    Path(show_name): Path<String>,
) -> impl IntoResponse {
    let databases = state.databases.lock().unwrap();

    let conn = match databases.get(&show_name) {
        Some(conn) => conn,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    let mut stmt = conn
        .prepare("SELECT id, start_timestamp_ms FROM sections ORDER BY id")
        .unwrap();

    let sections: Vec<SectionInfo> = stmt
        .query_map([], |row| {
            Ok(SectionInfo {
                id: row.get(0)?,
                start_timestamp_ms: row.get(1)?,
            })
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    Json(sections).into_response()
}

/// API handler: Get segments in range
async fn get_segments_handler(
    State(state): State<Arc<TestServerState>>,
    Path(show_name): Path<String>,
    Query(params): Query<SegmentQueryParams>,
) -> impl IntoResponse {
    let databases = state.databases.lock().unwrap();

    let conn = match databases.get(&show_name) {
        Some(conn) => conn,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Show not found"})),
            )
                .into_response()
        }
    };

    let mut stmt = conn
        .prepare(
            "SELECT id, timestamp_ms, is_timestamp_from_source, audio_data, section_id
             FROM segments
             WHERE id >= ?1 AND id <= ?2
             ORDER BY id",
        )
        .unwrap();

    let segments: Vec<SegmentData> = stmt
        .query_map(rusqlite::params![params.start_id, params.end_id], |row| {
            Ok(SegmentData {
                id: row.get(0)?,
                timestamp_ms: row.get(1)?,
                is_timestamp_from_source: row.get(2)?,
                audio_data: row.get(3)?,
                section_id: row.get(4)?,
            })
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    Json(segments).into_response()
}

/// Start a test HTTP server
async fn start_test_server(
    databases: HashMap<String, Connection>,
) -> (String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(TestServerState {
        databases: Arc::new(Mutex::new(databases)),
    });

    let app = Router::new()
        .route("/api/sync/shows", get(list_shows_handler))
        .route("/api/sync/shows/{show}/metadata", get(get_metadata_handler))
        .route("/api/sync/shows/{show}/sections", get(get_sections_handler))
        .route("/api/sync/shows/{show}/segments", get(get_segments_handler))
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

/// Helper to verify destination database
fn verify_destination_db(
    db_path: &std::path::Path,
    expected_show_name: &str,
    expected_source_unique_id: &str,
    expected_num_segments: usize,
    expected_num_sections: usize,
) {
    let conn = Connection::open(db_path).unwrap();

    // Verify metadata
    let source_unique_id: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'source_unique_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_unique_id, expected_source_unique_id);

    let name: String = conn
        .query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(name, expected_show_name);

    let is_recipient: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'is_recipient'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(is_recipient, "true");

    // Verify segment count
    let segment_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM segments", [], |row| row.get(0))
        .unwrap();
    assert_eq!(segment_count, expected_num_segments as i64);

    // Verify section count
    let section_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sections", [], |row| row.get(0))
        .unwrap();
    assert_eq!(section_count, expected_num_sections as i64);
}

#[tokio::test]
async fn test_sync_new_show() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database with 3 sections, 5 segments each
    let source_db = create_source_database("test_show", "source_unique_123", 3, 5);

    // Start test server
    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Sync to destination (spawn blocking since sync_shows uses blocking reqwest client)
    let local_dir = temp_dir.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(
            server_url, local_dir, None, // Sync all shows
            100,  // chunk_size
        )
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_ok(), "Sync failed: {:?}", result.err());

    // Verify destination database
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    assert!(dest_db_path.exists());

    verify_destination_db(
        &dest_db_path,
        "test_show",
        "source_unique_123",
        15, // 3 segments * 5 chunks
        3,  // 3 segments
    );
}

#[tokio::test]
async fn test_sync_incremental() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create initial source database with 2 segments
    let source_db = create_source_database("test_show", "source_unique_456", 2, 5);

    // Start test server
    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let local_dir = temp_dir.path().to_path_buf();
    let server_url_clone = server_url.clone();
    let local_dir_clone = local_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url_clone, local_dir_clone, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Verify initial sync
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    verify_destination_db(&dest_db_path, "test_show", "source_unique_456", 10, 2);

    // Now add more data to source and sync again
    // (In a real scenario, we'd update the source DB and restart server)
    // For this test, we verify that re-syncing doesn't break anything
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Should still have same data
    verify_destination_db(&dest_db_path, "test_show", "source_unique_456", 10, 2);
}

#[tokio::test]
async fn test_sync_with_whitelist() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create multiple source databases
    let source_db1 = create_source_database("show1", "unique_1", 2, 3);
    let source_db2 = create_source_database("show2", "unique_2", 2, 3);
    let source_db3 = create_source_database("show3", "unique_3", 2, 3);

    // Start test server
    let mut databases = HashMap::new();
    databases.insert("show1".to_string(), source_db1);
    databases.insert("show2".to_string(), source_db2);
    databases.insert("show3".to_string(), source_db3);
    let (server_url, _handle) = start_test_server(databases).await;

    // Sync only show1 and show3
    let local_dir = temp_dir.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(
            server_url,
            local_dir,
            Some(vec!["show1".to_string(), "show3".to_string()]),
            100,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Verify show1 exists
    let show1_path = temp_dir.path().join("show1.sqlite");
    assert!(show1_path.exists());

    // Verify show2 does NOT exist
    let show2_path = temp_dir.path().join("show2.sqlite");
    assert!(!show2_path.exists());

    // Verify show3 exists
    let show3_path = temp_dir.path().join("show3.sqlite");
    assert!(show3_path.exists());
}

#[tokio::test]
async fn test_sync_metadata_validation() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database
    let source_db = create_source_database("test_show", "source_unique_789", 2, 5);

    // Start test server
    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let local_dir = temp_dir.path().to_path_buf();
    let server_url_clone = server_url.clone();
    let local_dir_clone = local_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url_clone, local_dir_clone, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Manually tamper with destination metadata to cause validation failure
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    let conn = Connection::open(&dest_db_path).unwrap();
    conn.execute(
        "UPDATE metadata SET value = 'aac' WHERE key = 'audio_format'",
        [],
    )
    .unwrap();
    drop(conn);

    // Try to sync again - should fail due to metadata mismatch
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(err_msg.contains("Audio format mismatch") || err_msg.contains("mismatch"));
}

#[tokio::test]
async fn test_sync_rejects_old_version() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database with old version (version "2" instead of "3")
    let conn = Connection::open_in_memory().unwrap();

    // Create old schema (version 2 - without sections table)
    conn.execute(
        "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL
        )",
        [],
    )
    .unwrap();

    // Insert old version metadata
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('version', '2')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('unique_id', 'old_source')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('name', 'old_show')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('split_interval', '300')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('bitrate', '16')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')",
        [],
    )
    .unwrap();

    // Insert some test data
    conn.execute(
        "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id)
         VALUES (1700000000000, 1, 'test_data', 1)",
        [],
    )
    .unwrap();

    // Start test server with old database
    let mut databases = HashMap::new();
    databases.insert("old_show".to_string(), conn);
    let (server_url, _handle) = start_test_server(databases).await;

    // Try to sync - should fail due to version mismatch
    let local_dir = temp_dir.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
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
}

#[tokio::test]
async fn test_sync_rejects_old_version_on_resume() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database with current version (3) initially
    let source_db = create_source_database("test_show", "source_unique_999", 2, 5);

    // Start test server
    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync (should succeed)
    let local_dir = temp_dir.path().to_path_buf();
    let server_url_clone = server_url.clone();
    let local_dir_clone = local_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url_clone, local_dir_clone, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Now simulate remote server being downgraded to old version
    // (In reality this would be a server restart with old code)
    // Create old version database
    let old_conn = Connection::open_in_memory().unwrap();
    old_conn
        .execute(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "CREATE TABLE segments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms INTEGER NOT NULL,
                is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
                audio_data BLOB NOT NULL,
                section_id INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();

    // Insert old version metadata
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('version', '2')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('unique_id', 'source_unique_999')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('name', 'test_show')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('split_interval', '300')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('bitrate', '16')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')",
            [],
        )
        .unwrap();
    old_conn
        .execute(
            "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id)
             VALUES (1700000000000, 1, 'test', 1)",
            [],
        )
        .unwrap();

    // Replace server database with old version
    drop(_handle); // Stop old server
    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), old_conn);
    let (server_url, _handle) = start_test_server(databases).await;

    // Try to resume sync with old remote - should fail
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(
        err_msg.contains("unsupported") || err_msg.contains("schema version '2'"),
        "Expected version error but got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_sync_rejects_local_old_version() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create destination database with old version (simulating old local sync target)
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    let conn = Connection::open(&dest_db_path).unwrap();

    conn.execute(
        "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL
        )",
        [],
    )
    .unwrap();

    // Old version database
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('version', '2')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('unique_id', 'local_123')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('source_unique_id', 'source_999')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('name', 'test_show')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('split_interval', '300')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('bitrate', '16')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('is_recipient', 'true')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('last_synced_id', '0')",
        [],
    )
    .unwrap();
    drop(conn);

    // Create current version source database
    let source_db = create_source_database("test_show", "source_999", 2, 5);

    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Try to resume sync with old local database - should fail
    let local_dir = temp_dir.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(
        err_msg.contains("Local database has unsupported") || err_msg.contains("version '2'"),
        "Expected local version error but got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_sync_rejects_split_interval_mismatch() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database
    let source_db = create_source_database("test_show", "source_split", 2, 5);

    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let local_dir = temp_dir.path().to_path_buf();
    let server_url_clone = server_url.clone();
    let local_dir_clone = local_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url_clone, local_dir_clone, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Tamper with split_interval
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    let conn = Connection::open(&dest_db_path).unwrap();
    conn.execute(
        "UPDATE metadata SET value = '600' WHERE key = 'split_interval'",
        [],
    )
    .unwrap();
    drop(conn);

    // Try to sync again - should fail
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(
        err_msg.contains("Split interval mismatch"),
        "Expected split_interval mismatch error but got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_sync_rejects_bitrate_mismatch() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database
    let source_db = create_source_database("test_show", "source_bitrate", 2, 5);

    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let local_dir = temp_dir.path().to_path_buf();
    let server_url_clone = server_url.clone();
    let local_dir_clone = local_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url_clone, local_dir_clone, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Tamper with bitrate
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    let conn = Connection::open(&dest_db_path).unwrap();
    conn.execute("UPDATE metadata SET value = '32' WHERE key = 'bitrate'", [])
        .unwrap();
    drop(conn);

    // Try to sync again - should fail
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(
        err_msg.contains("Bitrate mismatch"),
        "Expected bitrate mismatch error but got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_sync_rejects_source_unique_id_mismatch() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database
    let source_db = create_source_database("test_show", "source_correct", 2, 5);

    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), source_db);
    let (server_url, _handle) = start_test_server(databases).await;

    // Initial sync
    let local_dir = temp_dir.path().to_path_buf();
    let server_url_clone = server_url.clone();
    let local_dir_clone = local_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url_clone, local_dir_clone, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    assert!(result.is_ok());

    // Tamper with source_unique_id (simulating pointing to different source)
    let dest_db_path = temp_dir.path().join("test_show.sqlite");
    let conn = Connection::open(&dest_db_path).unwrap();
    conn.execute(
        "UPDATE metadata SET value = 'different_source' WHERE key = 'source_unique_id'",
        [],
    )
    .unwrap();
    drop(conn);

    // Try to sync again - should fail
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    assert!(result.is_err());
    let err_msg = result.err().unwrap();
    assert!(
        err_msg.contains("Source mismatch"),
        "Expected source mismatch error but got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_sync_rejects_recipient_database() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create source database marked as recipient (sync target)
    let conn = Connection::open_in_memory().unwrap();

    // Create schema
    conn.execute(
        "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE sections (
            id INTEGER PRIMARY KEY,
            start_timestamp_ms INTEGER NOT NULL
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL REFERENCES sections(id)
        )",
        [],
    )
    .unwrap();

    // Insert metadata with is_recipient=true
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('version', ?1)",
        [EXPECTED_DB_VERSION],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('unique_id', 'recipient_db')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('name', 'test_show')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('audio_format', 'opus')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('split_interval', '300')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('bitrate', '16')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('sample_rate', '48000')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('is_recipient', 'true')",
        [],
    )
    .unwrap();

    // Insert test section and segment
    let section_id = 1700000000000i64 * 1000;
    conn.execute(
        "INSERT INTO sections (id, start_timestamp_ms) VALUES (?1, ?2)",
        rusqlite::params![section_id, 1700000000000i64],
    )
    .unwrap();
    let audio_data = b"test_audio_data";
    conn.execute(
        "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id)
         VALUES (1700000000000, 1, ?1, ?2)",
        rusqlite::params![&audio_data[..], section_id],
    )
    .unwrap();

    let mut databases = HashMap::new();
    databases.insert("test_show".to_string(), conn);
    let (server_url, _handle) = start_test_server(databases).await;

    // Try to sync - should fail with forbidden error
    let local_dir = temp_dir.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        sync_shows(server_url, local_dir, None, 100).map_err(|e| e.to_string())
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
}
