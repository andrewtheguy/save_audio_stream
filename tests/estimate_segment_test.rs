//! # Estimate Segment ID Tests
//!
//! These tests verify the estimate segment API endpoint for the SQLite backend (inspect mode).
//! The API estimates which segment corresponds to a given timestamp within a session.
//!
//! ## Running the Tests
//!
//! ```bash
//! cargo test --test estimate_segment_test
//! ```

use axum::{
    extract::{Path, Query, State},
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
    duration_samples_per_segment: i64,
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

            let sql = segments::insert(
                segment_timestamp_ms,
                is_boundary,
                section_id,
                &audio_data,
                duration_samples_per_segment,
            );
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    }

    (pool, guard)
}

// ============================================================================
// Response types matching the real API
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
struct EstimateSegmentResponse {
    section_id: i64,
    estimated_segment_id: i64,
    timestamp_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct EstimateSegmentError {
    error: String,
    section_start_ms: Option<i64>,
    section_end_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct EstimateSegmentParams {
    timestamp_ms: i64,
}

struct TestApiState {
    pool: sqlx::SqlitePool,
}

/// API handler that mirrors the real estimate_segment_handler behavior
async fn test_estimate_segment_handler(
    State(state): State<Arc<TestApiState>>,
    Path(section_id): Path<i64>,
    Query(params): Query<EstimateSegmentParams>,
) -> impl IntoResponse {
    let pool = &state.pool;

    // Get sample rate for duration calculation
    let sample_rate: f64 =
        sqlx::query_scalar::<_, String>("SELECT value FROM metadata WHERE key = 'sample_rate'")
            .fetch_one(pool)
            .await
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(48000.0);

    // Get section info with segment bounds and total duration
    let sql = segments::select_section_info_by_id(section_id);
    let row = match sqlx::query(&sql).fetch_optional(pool).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [("content-type", "application/json")],
                serde_json::to_string(&EstimateSegmentError {
                    error: format!("Section {} not found", section_id),
                    section_start_ms: None,
                    section_end_ms: None,
                })
                .unwrap(),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [("content-type", "application/json")],
                serde_json::to_string(&EstimateSegmentError {
                    error: format!("Database error: {}", e),
                    section_start_ms: None,
                    section_end_ms: None,
                })
                .unwrap(),
            )
                .into_response();
        }
    };

    let section_start_ms: i64 = row.get(1);
    let start_segment_id: Option<i64> = row.get(2);
    let end_segment_id: Option<i64> = row.get(3);
    let total_duration_samples: Option<i64> = row.get(4);

    // Check if section has any segments
    let (start_id, end_id, total_samples) =
        match (start_segment_id, end_segment_id, total_duration_samples) {
            (Some(s), Some(e), Some(d)) => (s, e, d),
            _ => {
                return (
                    StatusCode::NOT_FOUND,
                    [("content-type", "application/json")],
                    serde_json::to_string(&EstimateSegmentError {
                        error: format!("Section {} has no segments", section_id),
                        section_start_ms: Some(section_start_ms),
                        section_end_ms: None,
                    })
                    .unwrap(),
                )
                    .into_response();
            }
        };

    // Calculate total duration in milliseconds
    let total_duration_ms = (total_samples as f64 / sample_rate * 1000.0) as i64;
    let section_end_ms = section_start_ms + total_duration_ms;

    // Check if timestamp is within bounds
    if params.timestamp_ms < section_start_ms {
        return (
            StatusCode::BAD_REQUEST,
            [("content-type", "application/json")],
            serde_json::to_string(&EstimateSegmentError {
                error: format!(
                    "Timestamp {} is before section start ({})",
                    params.timestamp_ms, section_start_ms
                ),
                section_start_ms: Some(section_start_ms),
                section_end_ms: Some(section_end_ms),
            })
            .unwrap(),
        )
            .into_response();
    }

    if params.timestamp_ms > section_end_ms {
        return (
            StatusCode::BAD_REQUEST,
            [("content-type", "application/json")],
            serde_json::to_string(&EstimateSegmentError {
                error: format!(
                    "Timestamp {} is after section end ({})",
                    params.timestamp_ms, section_end_ms
                ),
                section_start_ms: Some(section_start_ms),
                section_end_ms: Some(section_end_ms),
            })
            .unwrap(),
        )
            .into_response();
    }

    // Calculate estimated segment ID using linear interpolation
    let offset_ms = params.timestamp_ms - section_start_ms;
    let fraction = offset_ms as f64 / total_duration_ms as f64;
    let segment_range = end_id - start_id;
    let estimated_segment_id = start_id + (fraction * segment_range as f64).round() as i64;

    // Clamp to valid range
    let estimated_segment_id = estimated_segment_id.max(start_id).min(end_id);

    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::to_string(&EstimateSegmentResponse {
            section_id,
            estimated_segment_id,
            timestamp_ms: params.timestamp_ms,
        })
        .unwrap(),
    )
        .into_response()
}

/// Start a test API server
async fn start_test_api_server(pool: sqlx::SqlitePool) -> (String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(TestApiState { pool });

    let app = Router::new()
        .route(
            "/api/session/{section_id}/estimate_segment",
            get(test_estimate_segment_handler),
        )
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

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_estimate_segment_at_start() {
    // Create database with one session, 10 segments, 1 second each at 48kHz
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    // Section ID is session_start_ms * 1000 (microseconds) + index
    let section_id = session_start_ms * 1000;

    // Request estimate for timestamp at session start
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, session_start_ms
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200, "Should return 200 OK");

    let data: EstimateSegmentResponse = response.json().await.unwrap();
    assert_eq!(data.section_id, section_id);
    assert_eq!(data.timestamp_ms, session_start_ms);
    // At the start, should estimate first segment (id=1)
    assert_eq!(data.estimated_segment_id, 1);
}

#[tokio::test]
async fn test_estimate_segment_at_middle() {
    // Create database with one session, 10 segments, 1 second each at 48kHz
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;

    // Total duration: 10 segments * 1 second = 10 seconds = 10000 ms
    // Request estimate for timestamp at 50% through the session (5 seconds in)
    let timestamp_at_50_percent = session_start_ms + 5000;

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, timestamp_at_50_percent
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200, "Should return 200 OK");

    let data: EstimateSegmentResponse = response.json().await.unwrap();
    assert_eq!(data.section_id, section_id);
    // At 50% through, with segments 1-10, should be around segment 5 or 6
    // Exact calculation: start=1, end=10, range=9, 0.5*9=4.5, round=5, 1+5=6
    assert!(
        data.estimated_segment_id >= 5 && data.estimated_segment_id <= 6,
        "At 50%, should estimate segment 5 or 6, got {}",
        data.estimated_segment_id
    );
}

#[tokio::test]
async fn test_estimate_segment_at_end() {
    // Create database with one session, 10 segments, 1 second each at 48kHz
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;

    // Total duration: 10 seconds = 10000 ms
    // Request estimate for timestamp at the very end
    let timestamp_at_end = session_start_ms + 10000;

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, timestamp_at_end
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200, "Should return 200 OK");

    let data: EstimateSegmentResponse = response.json().await.unwrap();
    assert_eq!(data.section_id, section_id);
    // At the end, should estimate last segment (id=10)
    assert_eq!(data.estimated_segment_id, 10);
}

#[tokio::test]
async fn test_estimate_segment_before_start_returns_error() {
    // Create database with one session
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;

    // Request timestamp before session start
    let timestamp_before_start = session_start_ms - 1000;

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, timestamp_before_start
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400, "Should return 400 Bad Request");

    let error: EstimateSegmentError = response.json().await.unwrap();
    assert!(
        error.error.contains("before section start"),
        "Error should mention 'before section start'"
    );
    assert_eq!(error.section_start_ms, Some(session_start_ms));
    assert!(error.section_end_ms.is_some());
}

#[tokio::test]
async fn test_estimate_segment_after_end_returns_error() {
    // Create database with one session, 10 segments, 1 second each
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;

    // Total duration: 10 seconds
    // Request timestamp after session end
    let timestamp_after_end = session_start_ms + 15000; // 15 seconds > 10 seconds

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, timestamp_after_end
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400, "Should return 400 Bad Request");

    let error: EstimateSegmentError = response.json().await.unwrap();
    assert!(
        error.error.contains("after section end"),
        "Error should mention 'after section end'"
    );
    assert_eq!(error.section_start_ms, Some(session_start_ms));
    // Section end should be session_start + 10000ms
    assert_eq!(error.section_end_ms, Some(session_start_ms + 10000));
}

#[tokio::test]
async fn test_estimate_segment_nonexistent_section_returns_404() {
    // Create database with one session
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    // Use a non-existent section ID
    let nonexistent_section_id = 9999999999i64;

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, nonexistent_section_id, session_start_ms
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 404, "Should return 404 Not Found");

    let error: EstimateSegmentError = response.json().await.unwrap();
    assert!(
        error.error.contains("not found"),
        "Error should mention 'not found'"
    );
    assert_eq!(error.section_start_ms, None);
    assert_eq!(error.section_end_ms, None);
}

#[tokio::test]
async fn test_estimate_segment_section_with_no_segments_returns_404() {
    // Create database manually with a section but no segments
    let (pool, guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
        .await
        .unwrap();

    save_audio_stream::db::init_database_schema(&pool)
        .await
        .unwrap();

    // Insert metadata
    let sql = metadata::insert("version", EXPECTED_DB_VERSION);
    sqlx::query(&sql).execute(&pool).await.unwrap();
    let sql = metadata::insert("sample_rate", "48000");
    sqlx::query(&sql).execute(&pool).await.unwrap();

    // Insert a section with no segments
    let section_id = 1730000000000000i64;
    let section_start_ms = 1730000000000i64;
    let sql = sections::insert(section_id, section_start_ms);
    sqlx::query(&sql).execute(&pool).await.unwrap();

    let (server_url, _handle) = start_test_api_server(pool).await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, section_start_ms
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 404, "Should return 404 Not Found");

    let error: EstimateSegmentError = response.json().await.unwrap();
    assert!(
        error.error.contains("no segments"),
        "Error should mention 'no segments'"
    );
    assert_eq!(error.section_start_ms, Some(section_start_ms));

    drop(guard); // Ensure temp directory cleanup
}

#[tokio::test]
async fn test_estimate_segment_linear_interpolation_accuracy() {
    // Create database with 100 segments to test interpolation accuracy
    let session_start_ms = 1730000000000i64;
    let segments_count = 100;
    let samples_per_segment = 48000; // 1 second each

    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], segments_count, samples_per_segment)
            .await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;
    let total_duration_ms = segments_count as i64 * 1000; // 100 seconds

    let client = reqwest::Client::new();

    // Test at various points: 0%, 25%, 50%, 75%, 100%
    let test_points = vec![
        (0, 1),    // 0% -> segment 1
        (25, 26),  // 25% -> around segment 25-26
        (50, 51),  // 50% -> around segment 50-51
        (75, 76),  // 75% -> around segment 75-76
        (100, 100), // 100% -> segment 100
    ];

    for (percentage, expected_approx) in test_points {
        let timestamp = session_start_ms + (total_duration_ms * percentage / 100);

        let response = client
            .get(format!(
                "{}/api/session/{}/estimate_segment?timestamp_ms={}",
                server_url, section_id, timestamp
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        let data: EstimateSegmentResponse = response.json().await.unwrap();

        // Allow for rounding differences (±2 segments)
        let diff = (data.estimated_segment_id - expected_approx).abs();
        assert!(
            diff <= 2,
            "At {}%, expected segment ~{}, got {} (diff={})",
            percentage,
            expected_approx,
            data.estimated_segment_id,
            diff
        );
    }
}

#[tokio::test]
async fn test_estimate_segment_multiple_sessions() {
    // Create database with multiple sessions
    let session_timestamps = vec![
        1730000000000i64, // Session 1
        1730100000000i64, // Session 2
        1730200000000i64, // Session 3
    ];

    let (pool, _guard) =
        create_test_database_with_sessions(&session_timestamps, 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let client = reqwest::Client::new();

    // Test each session independently
    for (idx, &session_start) in session_timestamps.iter().enumerate() {
        let section_id = session_start * 1000 + idx as i64;

        // Query at session start
        let response = client
            .get(format!(
                "{}/api/session/{}/estimate_segment?timestamp_ms={}",
                server_url, section_id, session_start
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            200,
            "Session {} should return 200",
            idx + 1
        );

        let data: EstimateSegmentResponse = response.json().await.unwrap();
        assert_eq!(data.section_id, section_id);
    }
}

#[tokio::test]
async fn test_estimate_segment_boundary_at_exact_end() {
    // Test edge case: timestamp exactly at session end boundary
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;
    let session_end_ms = session_start_ms + 10000; // Exactly at end

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, session_end_ms
        ))
        .send()
        .await
        .unwrap();

    // Timestamp exactly at end should be valid
    assert_eq!(response.status(), 200, "Timestamp at exact end should be valid");

    let data: EstimateSegmentResponse = response.json().await.unwrap();
    assert_eq!(data.estimated_segment_id, 10, "Should return last segment");
}

#[tokio::test]
async fn test_estimate_segment_just_past_end() {
    // Test edge case: timestamp 1ms past session end
    let session_start_ms = 1730000000000i64;
    let (pool, _guard) =
        create_test_database_with_sessions(&[session_start_ms], 10, 48000).await;

    let (server_url, _handle) = start_test_api_server(pool).await;

    let section_id = session_start_ms * 1000;
    let timestamp_past_end = session_start_ms + 10001; // 1ms past end

    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/session/{}/estimate_segment?timestamp_ms={}",
            server_url, section_id, timestamp_past_end
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400, "1ms past end should return error");
}
