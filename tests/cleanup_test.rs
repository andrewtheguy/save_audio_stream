use chrono::{DateTime, Utc};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use tokio::runtime::Runtime;

// Import the cleanup functions and SyncDb from the library
use save_audio_stream::db::SyncDb;
use save_audio_stream::queries::{metadata, sections, segments};
use save_audio_stream::record::{
    cleanup_old_sections_with_params, cleanup_old_sections_with_retention,
};

/// Helper function to create a test database with segments
/// Returns (pool, db, _guard) - keep _guard alive to prevent temp file deletion
fn create_test_database() -> (SqlitePool, SyncDb, tempfile::TempDir) {
    let rt = Runtime::new().unwrap();
    let (pool, guard) = rt.block_on(async {
        let (pool, guard) = save_audio_stream::db::create_test_connection_in_temporary_file()
            .await
            .unwrap();
        save_audio_stream::db::init_database_schema(&pool)
            .await
            .unwrap();
        (pool, guard)
    });
    // Create a SyncDb from the same path (need the temp_dir path)
    let db_path = guard.path().join("test.sqlite");
    let db = SyncDb::connect(&db_path).unwrap();
    (pool, db, guard)
}

/// Helper to insert a segment with explicit timestamp (milliseconds)
fn insert_segment_with_timestamp(
    pool: &SqlitePool,
    timestamp_ms: i64,
    is_boundary: bool,
    data: &[u8],
) -> i64 {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        // Generate section_id based on boundaries:
        // - If is_boundary=true, create a new section_id (using timestamp_ms for uniqueness)
        // - If is_boundary=false, use the most recent section_id from the database
        let section_id = if is_boundary {
            // New section: use timestamp_ms as base (convert to microseconds range)
            let new_section_id = timestamp_ms * 1000;

            // Insert into sections table
            let sql = sections::insert(new_section_id, timestamp_ms);
            sqlx::query(&sql).execute(pool).await.unwrap();

            new_section_id
        } else {
            // Continuation: get the most recent section_id
            let sql = "SELECT section_id FROM segments ORDER BY id DESC LIMIT 1";
            let result: Option<i64> = sqlx::query_scalar(sql).fetch_optional(pool).await.unwrap();

            match result {
                Some(id) => id,
                None => {
                    // No existing segments - create a default section
                    let default_section_id = timestamp_ms * 1000;
                    let sql = sections::insert(default_section_id, timestamp_ms);
                    sqlx::query(&sql).execute(pool).await.unwrap();
                    default_section_id
                }
            }
        };

        let sql = segments::insert(timestamp_ms, is_boundary, section_id, data, 0);
        sqlx::query(&sql).execute(pool).await.unwrap();

        let row_id: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();
        row_id
    })
}

/// Helper to insert a segment relative to current time
fn insert_segment(pool: &SqlitePool, hours_ago: i64, is_boundary: bool, data: &[u8]) -> i64 {
    let now = Utc::now();
    let timestamp = now - chrono::Duration::try_hours(hours_ago).expect("Valid hours");
    let timestamp_ms = timestamp.timestamp_millis();
    insert_segment_with_timestamp(pool, timestamp_ms, is_boundary, data)
}

/// Helper to insert a segment relative to a fixed reference time
fn insert_segment_relative_to(
    pool: &SqlitePool,
    reference_time: DateTime<Utc>,
    hours_ago: i64,
    is_boundary: bool,
    data: &[u8],
) -> i64 {
    let timestamp = reference_time - chrono::Duration::try_hours(hours_ago).expect("Valid hours");
    let timestamp_ms = timestamp.timestamp_millis();
    insert_segment_with_timestamp(pool, timestamp_ms, is_boundary, data)
}

/// Helper to count segments
fn count_segments(pool: &SqlitePool) -> i64 {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM segments")
            .fetch_one(pool)
            .await
            .unwrap();
        count
    })
}

/// Helper to get min and max segment IDs
fn get_segment_range(pool: &SqlitePool) -> (Option<i64>, Option<i64>) {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let min: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM segments")
            .fetch_optional(pool)
            .await
            .unwrap();
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM segments")
            .fetch_optional(pool)
            .await
            .unwrap();
        (min, max)
    })
}

/// Helper to check if a segment exists
fn segment_exists(pool: &SqlitePool, id: i64) -> bool {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let result: Option<i64> = sqlx::query_scalar("SELECT 1 FROM segments WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap();
        result.is_some()
    })
}

/// Helper to dump all segments for debugging
#[allow(dead_code)]
fn dump_segments(pool: &SqlitePool) {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let rows = sqlx::query(
            "SELECT id, timestamp_ms, is_timestamp_from_source FROM segments ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();

        println!("=== Segments in database ===");
        for row in rows {
            let id: i64 = row.get(0);
            let ts: i64 = row.get(1);
            let is_boundary: i32 = row.get(2);
            let dt = chrono::DateTime::from_timestamp_millis(ts).unwrap();
            println!(
                "id={}, timestamp={} ({}), is_boundary={}",
                id,
                ts,
                dt.format("%Y-%m-%d %H:%M:%S UTC"),
                is_boundary
            );
        }
        println!("============================");
    });
}

/// Helper to insert metadata
fn insert_metadata(pool: &SqlitePool, key: &str, value: &str) {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let sql = metadata::insert(key, value);
        sqlx::query(&sql).execute(pool).await.unwrap();
    });
}

/// Helper to get section_id for a segment
fn get_section_id_for_segment(pool: &SqlitePool, segment_id: i64) -> i64 {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let section_id: i64 = sqlx::query_scalar("SELECT section_id FROM segments WHERE id = ?")
            .bind(segment_id)
            .fetch_one(pool)
            .await
            .unwrap();
        section_id
    })
}

/// Helper to insert a section directly
fn insert_section(pool: &SqlitePool, section_id: i64, timestamp_ms: i64) {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let sql = sections::insert(section_id, timestamp_ms);
        sqlx::query(&sql).execute(pool).await.unwrap();
    });
}

#[test]
fn test_cleanup_deletes_old_segments_before_boundary() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Use a fixed reference time for deterministic testing
    let now = Utc::now();

    // Create segments spanning 400 hours
    // Very old boundary at 300 hours ago (will be deleted)
    let old_boundary_id = insert_segment_relative_to(&pool, now, 300, true, dummy_data);
    insert_segment_relative_to(&pool, now, 299, false, dummy_data);
    insert_segment_relative_to(&pool, now, 298, false, dummy_data);

    // Keeper boundary at 175 hours ago (last boundary before cutoff - will be kept)
    let keeper_boundary_id = insert_segment_relative_to(&pool, now, 175, true, dummy_data);
    insert_segment_relative_to(&pool, now, 174, false, dummy_data);
    insert_segment_relative_to(&pool, now, 173, false, dummy_data);

    // Recent boundary at 50 hours ago (within retention period)
    let recent_boundary_id = insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);
    insert_segment_relative_to(&pool, now, 48, false, dummy_data);

    // Total: 9 segments
    assert_eq!(count_segments(&pool), 9);

    // Run cleanup with 168 hour retention (~7 days) using the same reference time
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should have deleted segments before the keeper boundary (300h boundary + 2 segments)
    // Should keep: keeper boundary (175h) + 2 segments + recent boundary (50h) + 2 segments = 6 segments
    assert_eq!(count_segments(&pool), 6);

    // Verify old segments are deleted
    assert!(!segment_exists(&pool, old_boundary_id));

    // Verify keeper boundary and everything after is preserved
    assert!(segment_exists(&pool, keeper_boundary_id));
    assert!(segment_exists(&pool, recent_boundary_id));
}

#[test]
fn test_cleanup_preserves_all_recent_data() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // All segments within retention period (< 168 hours ago)
    insert_segment(&pool, 100, true, dummy_data);
    insert_segment(&pool, 99, false, dummy_data);
    insert_segment(&pool, 50, true, dummy_data);
    insert_segment(&pool, 49, false, dummy_data);
    insert_segment(&pool, 10, true, dummy_data);
    insert_segment(&pool, 9, false, dummy_data);

    assert_eq!(count_segments(&pool), 6);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // All segments should be preserved
    assert_eq!(count_segments(&pool), 6);
}

#[test]
fn test_cleanup_with_no_old_data() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // All segments very recent
    insert_segment(&pool, 5, true, dummy_data);
    insert_segment(&pool, 4, false, dummy_data);
    insert_segment(&pool, 1, false, dummy_data);

    assert_eq!(count_segments(&pool), 3);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // Nothing should be deleted
    assert_eq!(count_segments(&pool), 3);
}

#[test]
fn test_cleanup_with_no_boundaries() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Old segments but NO boundaries
    insert_segment(&pool, 200, false, dummy_data);
    insert_segment(&pool, 199, false, dummy_data);
    insert_segment(&pool, 50, false, dummy_data);
    insert_segment(&pool, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 4);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // Nothing should be deleted (no boundaries to anchor deletion)
    assert_eq!(count_segments(&pool), 4);
}

#[test]
fn test_cleanup_keeps_at_least_one_week_of_data() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Create data spanning multiple weeks
    // Very old data (3 weeks ago) with boundary
    insert_segment(&pool, 24 * 21, true, dummy_data); // 21 days ago
    insert_segment(&pool, 24 * 20, false, dummy_data);

    // Old data (2 weeks ago) with boundary - this should be the keeper
    let keeper_id = insert_segment(&pool, 24 * 14, true, dummy_data); // 14 days ago
    insert_segment(&pool, 24 * 13, false, dummy_data);

    // Within retention (1 week ago) with boundary
    let recent_id = insert_segment(&pool, 24 * 5, true, dummy_data); // 5 days ago
    insert_segment(&pool, 24 * 4, false, dummy_data);

    // Very recent
    insert_segment(&pool, 12, false, dummy_data); // 12 hours ago
    insert_segment(&pool, 6, false, dummy_data); // 6 hours ago

    assert_eq!(count_segments(&pool), 8);

    // Run cleanup with 168 hour (7 day) retention
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // Should delete segments before the 14-day-old boundary
    // Should keep: boundary at 14d + segment at 13d + boundary at 5d + segment at 4d + 2 recent = 6 segments
    assert_eq!(count_segments(&pool), 6);

    // Verify the keeper boundary is preserved
    assert!(segment_exists(&pool, keeper_id));
    assert!(segment_exists(&pool, recent_id));
}

#[test]
fn test_cleanup_with_multiple_old_boundaries() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Use a fixed reference time for deterministic testing
    let now = Utc::now();

    // Multiple boundaries in old data
    let oldest_boundary = insert_segment_relative_to(&pool, now, 300, true, dummy_data);
    insert_segment_relative_to(&pool, now, 299, false, dummy_data);

    let old_boundary_2 = insert_segment_relative_to(&pool, now, 250, true, dummy_data);
    insert_segment_relative_to(&pool, now, 249, false, dummy_data);

    let old_boundary_3 = insert_segment_relative_to(&pool, now, 200, true, dummy_data);
    insert_segment_relative_to(&pool, now, 199, false, dummy_data);

    // Keeper boundary (last one before retention cutoff)
    let keeper_boundary = insert_segment_relative_to(&pool, now, 175, true, dummy_data);
    insert_segment_relative_to(&pool, now, 174, false, dummy_data);

    // Recent data
    insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 10);

    // Run cleanup with 168 hour retention using the same reference time
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should delete all segments before the keeper boundary at 175h
    // Should keep: keeper (175h) + 1 segment + recent boundary + 1 segment = 4 segments
    assert_eq!(count_segments(&pool), 4);

    // Verify old boundaries are deleted
    assert!(!segment_exists(&pool, oldest_boundary));
    assert!(!segment_exists(&pool, old_boundary_2));
    assert!(!segment_exists(&pool, old_boundary_3));

    // Verify keeper boundary is preserved
    assert!(segment_exists(&pool, keeper_boundary));
}

#[test]
fn test_cleanup_boundary_exactly_at_cutoff() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Boundary exactly at the cutoff point
    let boundary_at_cutoff = insert_segment(&pool, 168, true, dummy_data);
    insert_segment(&pool, 167, false, dummy_data);

    // Recent data
    insert_segment(&pool, 50, true, dummy_data);
    insert_segment(&pool, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 4);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // Boundary exactly at cutoff should be preserved as it's not strictly less than cutoff
    // All 4 segments should remain
    assert_eq!(count_segments(&pool), 4);
    assert!(segment_exists(&pool, boundary_at_cutoff));
}

#[test]
fn test_cleanup_empty_database() {
    let (pool, db, _guard) = create_test_database();

    // No segments at all
    assert_eq!(count_segments(&pool), 0);

    // Run cleanup - should not error
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // Still empty
    assert_eq!(count_segments(&pool), 0);
}

#[test]
fn test_cleanup_verifies_sequential_deletion() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Create segments with IDs 1-10
    insert_segment(&pool, 200, true, dummy_data); // id 1
    insert_segment(&pool, 199, false, dummy_data); // id 2
    insert_segment(&pool, 198, false, dummy_data); // id 3
    let keeper_boundary = insert_segment(&pool, 180, true, dummy_data); // id 4
    insert_segment(&pool, 179, false, dummy_data); // id 5
    insert_segment(&pool, 50, true, dummy_data); // id 6
    insert_segment(&pool, 49, false, dummy_data); // id 7

    let (min_before, max_before) = get_segment_range(&pool);
    assert_eq!(min_before, Some(1));
    assert_eq!(max_before, Some(7));

    // Run cleanup
    cleanup_old_sections_with_retention(&db, 168).unwrap();

    // After cleanup, min_id should be the keeper boundary
    let (min_after, max_after) = get_segment_range(&pool);
    assert_eq!(min_after, Some(keeper_boundary));
    assert_eq!(max_after, Some(7));

    // Verify segments 1-3 are deleted, 4-7 remain
    assert!(!segment_exists(&pool, 1));
    assert!(!segment_exists(&pool, 2));
    assert!(!segment_exists(&pool, 3));
    assert!(segment_exists(&pool, 4));
    assert!(segment_exists(&pool, 5));
    assert!(segment_exists(&pool, 6));
    assert!(segment_exists(&pool, 7));
}

#[test]
fn test_cleanup_uses_pending_section_id_as_keeper() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    // Use a fixed reference time for deterministic testing
    let now = Utc::now();

    // Create very old section
    let very_old_section = insert_segment_relative_to(&pool, now, 300, true, dummy_data);
    insert_segment_relative_to(&pool, now, 299, false, dummy_data);

    // Create pending section (old but should be preserved)
    let pending_boundary = insert_segment_relative_to(&pool, now, 250, true, dummy_data);
    let pending_segment_1 = insert_segment_relative_to(&pool, now, 249, false, dummy_data);
    let pending_segment_2 = insert_segment_relative_to(&pool, now, 248, false, dummy_data);

    // Get the section_id for the pending boundary
    let pending_section_id = get_section_id_for_segment(&pool, pending_boundary);

    // Set pending_section_id in metadata
    insert_metadata(&pool, "pending_section_id", &pending_section_id.to_string());

    // Section newer than pending (200h ago is more recent than 250h ago)
    let newer_section = insert_segment_relative_to(&pool, now, 200, true, dummy_data);
    insert_segment_relative_to(&pool, now, 199, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 9);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should keep: keeper (pending 250h) + sections with start_timestamp_ms >= cutoff (50h)
    // Should delete: sections with start_timestamp_ms < cutoff except keeper
    // Deleted: very_old (300h) + 1 segment, newer (200h) + 1 segment = 4 segments
    // Kept: pending (250h keeper) + 2 segments + recent (50h) + 1 segment = 5 segments
    assert_eq!(count_segments(&pool), 5);

    // Verify pending section is preserved (it's the keeper)
    assert!(segment_exists(&pool, pending_boundary));
    assert!(segment_exists(&pool, pending_segment_1));
    assert!(segment_exists(&pool, pending_segment_2));

    // Verify recent section is preserved (>= cutoff)
    // But newer_section at 200h is deleted (< cutoff and not keeper)
    assert!(!segment_exists(&pool, newer_section));

    // Verify very old section is deleted
    assert!(!segment_exists(&pool, very_old_section));
}

#[test]
fn test_cleanup_falls_back_when_no_pending_section_id() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Old section
    let old_boundary = insert_segment_relative_to(&pool, now, 300, true, dummy_data);
    insert_segment_relative_to(&pool, now, 299, false, dummy_data);

    // Keeper section (latest before cutoff)
    let keeper_boundary = insert_segment_relative_to(&pool, now, 175, true, dummy_data);
    insert_segment_relative_to(&pool, now, 174, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 6);

    // NO pending_section_id in metadata - should use fallback logic
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should use fallback: keep keeper boundary (175h) and everything newer
    assert_eq!(count_segments(&pool), 4);

    // Verify keeper is preserved
    assert!(segment_exists(&pool, keeper_boundary));

    // Verify old section is deleted
    assert!(!segment_exists(&pool, old_boundary));
}

#[test]
fn test_cleanup_falls_back_when_pending_section_has_no_segments() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Create sections with segments
    let old_boundary = insert_segment_relative_to(&pool, now, 300, true, dummy_data);
    insert_segment_relative_to(&pool, now, 299, false, dummy_data);

    let keeper_boundary = insert_segment_relative_to(&pool, now, 175, true, dummy_data);
    insert_segment_relative_to(&pool, now, 174, false, dummy_data);

    insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);

    // Create an empty section (section with no segments)
    let empty_section_id = (now.timestamp_millis() + 999999) * 1000;
    insert_section(&pool, empty_section_id, now.timestamp_millis());

    // Set the empty section as pending
    insert_metadata(&pool, "pending_section_id", &empty_section_id.to_string());

    assert_eq!(count_segments(&pool), 6);

    // Cleanup should fall back to keeper logic since pending section has no segments
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should use fallback logic
    assert_eq!(count_segments(&pool), 4);

    assert!(segment_exists(&pool, keeper_boundary));
    assert!(!segment_exists(&pool, old_boundary));
}

#[test]
fn test_cleanup_preserves_pending_section_even_when_very_old() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Very old sections
    insert_segment_relative_to(&pool, now, 500, true, dummy_data);
    insert_segment_relative_to(&pool, now, 499, false, dummy_data);

    insert_segment_relative_to(&pool, now, 400, true, dummy_data);
    insert_segment_relative_to(&pool, now, 399, false, dummy_data);

    // Pending section (very old but should be preserved)
    let pending_boundary = insert_segment_relative_to(&pool, now, 350, true, dummy_data);
    let pending_segment = insert_segment_relative_to(&pool, now, 349, false, dummy_data);

    let pending_section_id = get_section_id_for_segment(&pool, pending_boundary);

    insert_metadata(&pool, "pending_section_id", &pending_section_id.to_string());

    // Section that would normally be the keeper (latest before cutoff)
    insert_segment_relative_to(&pool, now, 175, true, dummy_data);
    insert_segment_relative_to(&pool, now, 174, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 10);

    // Cleanup with 168 hour retention
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should keep: keeper (pending 350h) + sections with start_timestamp_ms >= cutoff (50h)
    // Should delete: all other sections with start_timestamp_ms < cutoff
    // Deleted: 500h, 400h, 175h sections (6 segments)
    // Kept: pending (350h keeper) + 1 segment + recent (50h) + 1 segment = 4 segments
    assert_eq!(count_segments(&pool), 4);

    // Verify pending section is preserved
    assert!(segment_exists(&pool, pending_boundary));
    assert!(segment_exists(&pool, pending_segment));
}

#[test]
fn test_cleanup_with_pending_section_id_and_multiple_sections_before_cutoff() {
    let (pool, db, _guard) = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Multiple old sections
    let very_old = insert_segment_relative_to(&pool, now, 500, true, dummy_data);
    insert_segment_relative_to(&pool, now, 499, false, dummy_data);

    let old_2 = insert_segment_relative_to(&pool, now, 400, true, dummy_data);
    insert_segment_relative_to(&pool, now, 399, false, dummy_data);

    let old_3 = insert_segment_relative_to(&pool, now, 300, true, dummy_data);
    insert_segment_relative_to(&pool, now, 299, false, dummy_data);

    // Pending section (should act as keeper)
    let pending_boundary = insert_segment_relative_to(&pool, now, 200, true, dummy_data);
    insert_segment_relative_to(&pool, now, 199, false, dummy_data);

    let pending_section_id = get_section_id_for_segment(&pool, pending_boundary);

    insert_metadata(&pool, "pending_section_id", &pending_section_id.to_string());

    // Section between pending and cutoff
    insert_segment_relative_to(&pool, now, 175, true, dummy_data);
    insert_segment_relative_to(&pool, now, 174, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&pool, now, 50, true, dummy_data);
    insert_segment_relative_to(&pool, now, 49, false, dummy_data);

    assert_eq!(count_segments(&pool), 12);

    // Cleanup
    cleanup_old_sections_with_params(&db, 168, Some(now)).unwrap();

    // Should keep: keeper (pending 200h) + sections with start_timestamp_ms >= cutoff (50h)
    // Should delete: sections with start_timestamp_ms < cutoff except keeper
    // Deleted: 500h, 400h, 300h, 175h sections (8 segments)
    // Kept: pending (200h keeper) + 1 segment + recent (50h) + 1 segment = 4 segments
    assert_eq!(count_segments(&pool), 4);

    // Verify old sections are deleted
    assert!(!segment_exists(&pool, very_old));
    assert!(!segment_exists(&pool, old_2));
    assert!(!segment_exists(&pool, old_3));

    // Verify pending is preserved (it's the keeper)
    assert!(segment_exists(&pool, pending_boundary));
}
