//! # Session Filtering Tests
//!
//! These tests verify the session filtering logic for the SQLite backend.
//! The filtering is used in inspect mode to filter sessions by date range.
//!
//! ## Running the Tests
//!
//! ```bash
//! cargo test --test sessions_filter_test
//! ```

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::sync::Arc;
use tokio::net::TcpListener;

use save_audio_stream::queries::{metadata, sections, segments};
use save_audio_stream::EXPECTED_DB_VERSION;

/// Helper to create a test database with sessions at specific timestamps
async fn create_test_database_with_sessions(
    session_timestamps_ms: &[i64],
    segments_per_session: usize,
) -> (sqlx::SqlitePool, tempfile::TempDir) {
    let (pool, guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
        .await
        .unwrap();

    // Create schema
    save_audio_stream::db::init_database_schema(&pool)
        .await
        .unwrap();

    // Insert metadata
    let sql = metadata::insert("version", EXPECTED_DB_VERSION);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("unique_id", "test_unique_id");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("name", "test_show");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("audio_format", "opus");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let sql = metadata::insert("sample_rate", "48000");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    // Insert sessions at specified timestamps
    for (idx, &timestamp_ms) in session_timestamps_ms.iter().enumerate() {
        // section_id is microseconds (timestamp_ms * 1000 + idx to ensure uniqueness)
        let section_id = timestamp_ms * 1000 + idx as i64;

        // Insert section
        let sql = sections::insert(section_id, timestamp_ms);
        sqlx::query(&sql).execute(&pool).await.unwrap();

        // Insert segments for this section
        for seg_idx in 0..segments_per_session {
            let is_boundary = seg_idx == 0;
            let segment_timestamp_ms = timestamp_ms + seg_idx as i64 * 1000;
            let audio_data = format!("audio_sec{}_seg{}", idx, seg_idx).into_bytes();
            let duration_samples = 48000i64; // 1 second at 48kHz

            let sql = segments::insert(
                segment_timestamp_ms,
                is_boundary,
                section_id,
                &audio_data,
                duration_samples,
            );
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    }

    (pool, guard)
}

/// Helper to query sessions with filtering
async fn query_sessions_filtered(
    pool: &sqlx::SqlitePool,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
) -> Vec<(i64, i64)> {
    // Returns (section_id, timestamp_ms) pairs
    let sql = segments::select_sessions_with_join_filtered(start_ts, end_ts);
    let rows = sqlx::query(&sql).fetch_all(pool).await.unwrap();

    rows.iter()
        .filter_map(|row| {
            let section_id: i64 = row.get(0);
            let timestamp_ms: i64 = row.get(1);
            let start_segment_id: Option<i64> = row.get(2);
            let end_segment_id: Option<i64> = row.get(3);
            // Only return sessions that have segments
            if start_segment_id.is_some() && end_segment_id.is_some() {
                Some((section_id, timestamp_ms))
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn test_no_filter_returns_all_sessions() {
    // Create database with 3 sessions on different days
    let timestamps = vec![
        1730000000000i64, // Oct 27, 2024 ~02:13 UTC
        1730100000000i64, // Oct 28, 2024 ~06:00 UTC
        1730200000000i64, // Oct 29, 2024 ~09:46 UTC
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    // Query without any filter
    let sessions = query_sessions_filtered(&pool, None, None).await;

    assert_eq!(sessions.len(), 3, "Should return all 3 sessions");
}

#[tokio::test]
async fn test_filter_by_start_ts_only() {
    // Create database with 3 sessions
    let timestamps = vec![
        1730000000000i64, // Session 1
        1730100000000i64, // Session 2
        1730200000000i64, // Session 3
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    // Query with start_ts that excludes first session
    let sessions = query_sessions_filtered(&pool, Some(1730050000000), None).await;

    assert_eq!(sessions.len(), 2, "Should return 2 sessions after start_ts");
    assert!(
        sessions.iter().all(|(_, ts)| *ts >= 1730050000000),
        "All sessions should be >= start_ts"
    );
}

#[tokio::test]
async fn test_filter_by_end_ts_only() {
    // Create database with 3 sessions
    let timestamps = vec![
        1730000000000i64, // Session 1
        1730100000000i64, // Session 2
        1730200000000i64, // Session 3
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    // Query with end_ts that excludes last session
    let sessions = query_sessions_filtered(&pool, None, Some(1730150000000)).await;

    assert_eq!(sessions.len(), 2, "Should return 2 sessions before end_ts");
    assert!(
        sessions.iter().all(|(_, ts)| *ts < 1730150000000),
        "All sessions should be < end_ts"
    );
}

#[tokio::test]
async fn test_filter_by_both_start_and_end_ts() {
    // Create database with 5 sessions
    let timestamps = vec![
        1730000000000i64, // Session 1 - before range
        1730100000000i64, // Session 2 - in range
        1730150000000i64, // Session 3 - in range
        1730200000000i64, // Session 4 - in range
        1730300000000i64, // Session 5 - after range
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    // Query with both start_ts and end_ts
    let start_ts = 1730050000000i64;
    let end_ts = 1730250000000i64;
    let sessions = query_sessions_filtered(&pool, Some(start_ts), Some(end_ts)).await;

    assert_eq!(sessions.len(), 3, "Should return 3 sessions in range");
    assert!(
        sessions
            .iter()
            .all(|(_, ts)| *ts >= start_ts && *ts < end_ts),
        "All sessions should be within range"
    );
}

#[tokio::test]
async fn test_filter_returns_empty_when_no_match() {
    // Create database with sessions
    let timestamps = vec![
        1730000000000i64, // Oct 27
        1730100000000i64, // Oct 28
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    // Query with range that has no sessions
    let sessions = query_sessions_filtered(&pool, Some(1731000000000), Some(1732000000000)).await;

    assert_eq!(sessions.len(), 0, "Should return no sessions");
}

#[tokio::test]
async fn test_filter_single_day() {
    // Simulate filtering for a single day (like the date picker does)
    // Create sessions across multiple days
    let timestamps = vec![
        1730000000000i64, // Oct 27, 2024 02:13:20 UTC
        1730073600000i64, // Oct 28, 2024 00:00:00 UTC (midnight)
        1730080000000i64, // Oct 28, 2024 01:46:40 UTC
        1730140000000i64, // Oct 28, 2024 18:26:40 UTC
        1730160000000i64, // Oct 29, 2024 00:00:00 UTC (midnight next day)
        1730200000000i64, // Oct 29, 2024 11:06:40 UTC
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 2).await;

    // Filter for Oct 28 only (midnight to midnight)
    let oct_28_start = 1730073600000i64; // Oct 28 00:00:00 UTC
    let oct_29_start = 1730160000000i64; // Oct 29 00:00:00 UTC

    let sessions = query_sessions_filtered(&pool, Some(oct_28_start), Some(oct_29_start)).await;

    assert_eq!(sessions.len(), 3, "Should return 3 sessions on Oct 28");

    // Verify all returned sessions are within Oct 28
    for (_, ts) in &sessions {
        assert!(
            *ts >= oct_28_start && *ts < oct_29_start,
            "Session timestamp {} should be within Oct 28 range",
            ts
        );
    }
}

#[tokio::test]
async fn test_filter_boundary_conditions() {
    // Test exact boundary matching
    let exact_timestamp = 1730100000000i64;
    let timestamps = vec![
        exact_timestamp - 1, // Just before
        exact_timestamp,     // Exactly at boundary
        exact_timestamp + 1, // Just after
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 2).await;

    // Test start_ts is inclusive (>=)
    let sessions = query_sessions_filtered(&pool, Some(exact_timestamp), None).await;
    assert_eq!(
        sessions.len(),
        2,
        "start_ts should be inclusive (>= boundary)"
    );

    // Test end_ts is exclusive (<)
    let sessions = query_sessions_filtered(&pool, None, Some(exact_timestamp)).await;
    assert_eq!(
        sessions.len(),
        1,
        "end_ts should be exclusive (< boundary)"
    );

    // Test exact match: start_ts <= x < end_ts
    let sessions =
        query_sessions_filtered(&pool, Some(exact_timestamp), Some(exact_timestamp + 1)).await;
    assert_eq!(sessions.len(), 1, "Should return exactly the boundary session");
    assert_eq!(sessions[0].1, exact_timestamp);
}

#[tokio::test]
async fn test_sessions_ordered_by_timestamp() {
    // Sessions inserted out of order
    let timestamps = vec![
        1730200000000i64, // Third chronologically
        1730000000000i64, // First chronologically
        1730100000000i64, // Second chronologically
    ];

    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 2).await;

    let sessions = query_sessions_filtered(&pool, None, None).await;

    assert_eq!(sessions.len(), 3);
    // Verify sessions are ordered by timestamp ascending
    assert!(
        sessions[0].1 < sessions[1].1 && sessions[1].1 < sessions[2].1,
        "Sessions should be ordered by timestamp ascending"
    );
}

#[tokio::test]
async fn test_empty_database() {
    let (pool, _guard) = create_test_database_with_sessions(&[], 0).await;

    let sessions = query_sessions_filtered(&pool, None, None).await;
    assert_eq!(sessions.len(), 0, "Empty database should return no sessions");

    let sessions = query_sessions_filtered(&pool, Some(1730000000000), Some(1730100000000)).await;
    assert_eq!(
        sessions.len(),
        0,
        "Empty database with filter should return no sessions"
    );
}

// ============================================================================
// API-level tests - verify JSON response format
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
struct SessionInfo {
    section_id: i64,
    start_id: i64,
    end_id: i64,
    timestamp_ms: i64,
    duration_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionsResponse {
    name: String,
    sessions: Vec<SessionInfo>,
}

#[derive(Debug, Deserialize)]
struct SessionsQueryParams {
    start_ts: Option<i64>,
    end_ts: Option<i64>,
}

struct TestApiState {
    pool: sqlx::SqlitePool,
}

/// API handler that mirrors the real sessions_handler behavior
async fn test_sessions_handler(
    State(state): State<Arc<TestApiState>>,
    Query(params): Query<SessionsQueryParams>,
) -> impl IntoResponse {
    let pool = &state.pool;

    // Get show name from metadata
    let name: String = sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'name'")
        .fetch_one(pool)
        .await
        .unwrap_or_else(|_| "unknown".to_string());

    // Get sample rate for duration calculation
    let sample_rate: f64 =
        sqlx::query_scalar::<_, String>("SELECT value FROM metadata WHERE key = 'sample_rate'")
            .fetch_one(pool)
            .await
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(48000.0);

    // Get sessions with optional filtering
    let sql = segments::select_sessions_with_join_filtered(params.start_ts, params.end_ts);
    let rows = sqlx::query(&sql).fetch_all(pool).await.unwrap();

    let sessions: Vec<SessionInfo> = rows
        .iter()
        .filter_map(|row| {
            let section_id: i64 = row.get(0);
            let timestamp_ms: i64 = row.get(1);
            let start_segment_id: Option<i64> = row.get(2);
            let end_segment_id: Option<i64> = row.get(3);
            let total_duration_samples: Option<i64> = row.get(4);
            match (start_segment_id, end_segment_id, total_duration_samples) {
                (Some(start_id), Some(end_id), Some(samples)) => {
                    let duration_seconds = samples as f64 / sample_rate;
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

    // Always return valid JSON, even when empty
    let response = SessionsResponse { name, sessions };

    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::to_string(&response).unwrap(),
    )
}

/// Start a test API server
async fn start_test_api_server(pool: sqlx::SqlitePool) -> (String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(TestApiState { pool });

    let app = Router::new()
        .route("/api/sessions", get(test_sessions_handler))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    (url, handle)
}

#[tokio::test]
async fn test_api_returns_valid_json_when_no_sessions() {
    // Create empty database
    let (pool, _guard) = create_test_database_with_sessions(&[], 0).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    // Make API request
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/sessions", server_url))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200, "Should return 200 OK");

    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("application/json"),
        "Content-Type should be application/json"
    );

    // Parse response as JSON
    let body = response.text().await.unwrap();
    let parsed: Result<SessionsResponse, _> = serde_json::from_str(&body);
    assert!(
        parsed.is_ok(),
        "Response should be valid JSON, got: {}",
        body
    );

    let data = parsed.unwrap();
    assert_eq!(data.name, "test_show");
    assert!(data.sessions.is_empty(), "Sessions should be empty");
}

#[tokio::test]
async fn test_api_returns_valid_json_when_filter_excludes_all() {
    // Create database with sessions
    let timestamps = vec![1730000000000i64, 1730100000000i64];
    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    // Make API request with filter that excludes all sessions
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/sessions?start_ts=1731000000000&end_ts=1732000000000",
            server_url
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200, "Should return 200 OK");

    let body = response.text().await.unwrap();
    let parsed: Result<SessionsResponse, _> = serde_json::from_str(&body);
    assert!(
        parsed.is_ok(),
        "Response should be valid JSON when filter excludes all, got: {}",
        body
    );

    let data = parsed.unwrap();
    assert!(
        data.sessions.is_empty(),
        "Sessions should be empty when filter excludes all"
    );
}

#[tokio::test]
async fn test_api_returns_valid_json_with_sessions() {
    // Create database with sessions
    let timestamps = vec![1730000000000i64, 1730100000000i64, 1730200000000i64];
    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 3).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    // Make API request without filter
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/sessions", server_url))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200, "Should return 200 OK");

    let body = response.text().await.unwrap();
    let parsed: Result<SessionsResponse, _> = serde_json::from_str(&body);
    assert!(
        parsed.is_ok(),
        "Response should be valid JSON, got: {}",
        body
    );

    let data = parsed.unwrap();
    assert_eq!(data.name, "test_show");
    assert_eq!(data.sessions.len(), 3, "Should have 3 sessions");

    // Verify session structure
    for session in &data.sessions {
        assert!(session.section_id > 0);
        assert!(session.start_id > 0);
        assert!(session.end_id >= session.start_id);
        assert!(session.timestamp_ms > 0);
        assert!(session.duration_seconds > 0.0);
    }
}

#[tokio::test]
async fn test_api_filter_returns_correct_sessions() {
    // Create database with sessions across different times
    let timestamps = vec![
        1730000000000i64, // Should be excluded
        1730100000000i64, // Should be included
        1730150000000i64, // Should be included
        1730300000000i64, // Should be excluded
    ];
    let (pool, _guard) = create_test_database_with_sessions(&timestamps, 2).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    // Filter to get only middle two sessions
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/sessions?start_ts=1730050000000&end_ts=1730200000000",
            server_url
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let data: SessionsResponse = response.json().await.unwrap();
    assert_eq!(data.sessions.len(), 2, "Should return 2 filtered sessions");

    // Verify timestamps are within range
    for session in &data.sessions {
        assert!(
            session.timestamp_ms >= 1730050000000 && session.timestamp_ms < 1730200000000,
            "Session timestamp {} should be within filter range",
            session.timestamp_ms
        );
    }
}
