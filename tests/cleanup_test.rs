use chrono::{DateTime, Utc};
use rusqlite::Connection;

// Import the cleanup functions from the library
use save_audio_stream::record::{
    cleanup_old_sections_with_params, cleanup_old_sections_with_retention,
};

/// Helper function to create a test database with segments
fn create_test_database() -> Connection {
    let conn = save_audio_stream::db::create_test_connection_in_memory();

    // Create tables
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
            section_id INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE
        )",
        [],
    )
    .unwrap();

    // Create indexes
    conn.execute(
        "CREATE INDEX idx_segments_boundary
         ON segments(is_timestamp_from_source, timestamp_ms)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE INDEX idx_segments_section_id
         ON segments(section_id)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE INDEX idx_sections_start_timestamp
         ON sections(start_timestamp_ms)",
        [],
    )
    .unwrap();

    conn
}

/// Helper to insert a segment with explicit timestamp (milliseconds)
fn insert_segment_with_timestamp(
    conn: &Connection,
    timestamp_ms: i64,
    is_boundary: bool,
    data: &[u8],
) -> i64 {
    // Generate section_id based on boundaries:
    // - If is_boundary=true, create a new section_id (using timestamp_ms for uniqueness)
    // - If is_boundary=false, use the most recent section_id from the database
    let section_id = if is_boundary {
        // New section: use timestamp_ms as base (convert to microseconds range)
        let new_section_id = timestamp_ms * 1000;

        // Insert into sections table
        conn.execute(
            "INSERT INTO sections (id, start_timestamp_ms) VALUES (?1, ?2)",
            rusqlite::params![new_section_id, timestamp_ms],
        )
        .unwrap();

        new_section_id
    } else {
        // Continuation: get the most recent section_id
        conn.query_row(
            "SELECT section_id FROM segments ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| {
            // No existing segments - create a default section
            let default_section_id = timestamp_ms * 1000;
            conn.execute(
                "INSERT INTO sections (id, start_timestamp_ms) VALUES (?1, ?2)",
                rusqlite::params![default_section_id, timestamp_ms],
            )
            .unwrap();
            default_section_id
        })
    };

    conn.execute(
        "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data, section_id) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![timestamp_ms, is_boundary as i32, data, section_id],
    )
    .unwrap();

    conn.last_insert_rowid()
}

/// Helper to insert a segment relative to current time
fn insert_segment(conn: &Connection, hours_ago: i64, is_boundary: bool, data: &[u8]) -> i64 {
    let now = Utc::now();
    let timestamp = now - chrono::Duration::try_hours(hours_ago).expect("Valid hours");
    let timestamp_ms = timestamp.timestamp_millis();
    insert_segment_with_timestamp(conn, timestamp_ms, is_boundary, data)
}

/// Helper to insert a segment relative to a fixed reference time
fn insert_segment_relative_to(
    conn: &Connection,
    reference_time: DateTime<Utc>,
    hours_ago: i64,
    is_boundary: bool,
    data: &[u8],
) -> i64 {
    let timestamp = reference_time - chrono::Duration::try_hours(hours_ago).expect("Valid hours");
    let timestamp_ms = timestamp.timestamp_millis();
    insert_segment_with_timestamp(conn, timestamp_ms, is_boundary, data)
}

/// Helper to count segments
fn count_segments(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM segments", [], |row| row.get(0))
        .unwrap()
}

/// Helper to get min and max segment IDs
fn get_segment_range(conn: &Connection) -> (Option<i64>, Option<i64>) {
    let min: Option<i64> = conn
        .query_row("SELECT MIN(id) FROM segments", [], |row| row.get(0))
        .ok();
    let max: Option<i64> = conn
        .query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0))
        .ok();
    (min, max)
}

/// Helper to check if a segment exists
fn segment_exists(conn: &Connection, id: i64) -> bool {
    conn.query_row("SELECT 1 FROM segments WHERE id = ?1", [id], |_| Ok(()))
        .is_ok()
}

/// Helper to dump all segments for debugging
#[allow(dead_code)]
fn dump_segments(conn: &Connection) {
    let mut stmt = conn
        .prepare("SELECT id, timestamp_ms, is_timestamp_from_source FROM segments ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i32>(2)?,
            ))
        })
        .unwrap();

    println!("=== Segments in database ===");
    for row in rows {
        let (id, ts, is_boundary) = row.unwrap();
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
}

#[test]
fn test_cleanup_deletes_old_segments_before_boundary() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Use a fixed reference time for deterministic testing
    let now = Utc::now();

    // Create segments spanning 400 hours
    // Very old boundary at 300 hours ago (will be deleted)
    let old_boundary_id = insert_segment_relative_to(&conn, now, 300, true, dummy_data);
    insert_segment_relative_to(&conn, now, 299, false, dummy_data);
    insert_segment_relative_to(&conn, now, 298, false, dummy_data);

    // Keeper boundary at 175 hours ago (last boundary before cutoff - will be kept)
    let keeper_boundary_id = insert_segment_relative_to(&conn, now, 175, true, dummy_data);
    insert_segment_relative_to(&conn, now, 174, false, dummy_data);
    insert_segment_relative_to(&conn, now, 173, false, dummy_data);

    // Recent boundary at 50 hours ago (within retention period)
    let recent_boundary_id = insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);
    insert_segment_relative_to(&conn, now, 48, false, dummy_data);

    // Total: 9 segments
    assert_eq!(count_segments(&conn), 9);

    // Run cleanup with 168 hour retention (~7 days) using the same reference time
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should have deleted segments before the keeper boundary (300h boundary + 2 segments)
    // Should keep: keeper boundary (175h) + 2 segments + recent boundary (50h) + 2 segments = 6 segments
    assert_eq!(count_segments(&conn), 6);

    // Verify old segments are deleted
    assert!(!segment_exists(&conn, old_boundary_id));

    // Verify keeper boundary and everything after is preserved
    assert!(segment_exists(&conn, keeper_boundary_id));
    assert!(segment_exists(&conn, recent_boundary_id));
}

#[test]
fn test_cleanup_preserves_all_recent_data() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // All segments within retention period (< 168 hours ago)
    insert_segment(&conn, 100, true, dummy_data);
    insert_segment(&conn, 99, false, dummy_data);
    insert_segment(&conn, 50, true, dummy_data);
    insert_segment(&conn, 49, false, dummy_data);
    insert_segment(&conn, 10, true, dummy_data);
    insert_segment(&conn, 9, false, dummy_data);

    assert_eq!(count_segments(&conn), 6);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // All segments should be preserved
    assert_eq!(count_segments(&conn), 6);
}

#[test]
fn test_cleanup_with_no_old_data() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // All segments very recent
    insert_segment(&conn, 5, true, dummy_data);
    insert_segment(&conn, 4, false, dummy_data);
    insert_segment(&conn, 1, false, dummy_data);

    assert_eq!(count_segments(&conn), 3);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // Nothing should be deleted
    assert_eq!(count_segments(&conn), 3);
}

#[test]
fn test_cleanup_with_no_boundaries() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Old segments but NO boundaries
    insert_segment(&conn, 200, false, dummy_data);
    insert_segment(&conn, 199, false, dummy_data);
    insert_segment(&conn, 50, false, dummy_data);
    insert_segment(&conn, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 4);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // Nothing should be deleted (no boundaries to anchor deletion)
    assert_eq!(count_segments(&conn), 4);
}

#[test]
fn test_cleanup_keeps_at_least_one_week_of_data() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Create data spanning multiple weeks
    // Very old data (3 weeks ago) with boundary
    insert_segment(&conn, 24 * 21, true, dummy_data); // 21 days ago
    insert_segment(&conn, 24 * 20, false, dummy_data);

    // Old data (2 weeks ago) with boundary - this should be the keeper
    let keeper_id = insert_segment(&conn, 24 * 14, true, dummy_data); // 14 days ago
    insert_segment(&conn, 24 * 13, false, dummy_data);

    // Within retention (1 week ago) with boundary
    let recent_id = insert_segment(&conn, 24 * 5, true, dummy_data); // 5 days ago
    insert_segment(&conn, 24 * 4, false, dummy_data);

    // Very recent
    insert_segment(&conn, 12, false, dummy_data); // 12 hours ago
    insert_segment(&conn, 6, false, dummy_data); // 6 hours ago

    assert_eq!(count_segments(&conn), 8);

    // Run cleanup with 168 hour (7 day) retention
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // Should delete segments before the 14-day-old boundary
    // Should keep: boundary at 14d + segment at 13d + boundary at 5d + segment at 4d + 2 recent = 6 segments
    assert_eq!(count_segments(&conn), 6);

    // Verify the keeper boundary is preserved
    assert!(segment_exists(&conn, keeper_id));
    assert!(segment_exists(&conn, recent_id));
}

#[test]
fn test_cleanup_with_multiple_old_boundaries() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Use a fixed reference time for deterministic testing
    let now = Utc::now();

    // Multiple boundaries in old data
    let oldest_boundary = insert_segment_relative_to(&conn, now, 300, true, dummy_data);
    insert_segment_relative_to(&conn, now, 299, false, dummy_data);

    let old_boundary_2 = insert_segment_relative_to(&conn, now, 250, true, dummy_data);
    insert_segment_relative_to(&conn, now, 249, false, dummy_data);

    let old_boundary_3 = insert_segment_relative_to(&conn, now, 200, true, dummy_data);
    insert_segment_relative_to(&conn, now, 199, false, dummy_data);

    // Keeper boundary (last one before retention cutoff)
    let keeper_boundary = insert_segment_relative_to(&conn, now, 175, true, dummy_data);
    insert_segment_relative_to(&conn, now, 174, false, dummy_data);

    // Recent data
    insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 10);

    // Run cleanup with 168 hour retention using the same reference time
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should delete all segments before the keeper boundary at 175h
    // Should keep: keeper (175h) + 1 segment + recent boundary + 1 segment = 4 segments
    assert_eq!(count_segments(&conn), 4);

    // Verify old boundaries are deleted
    assert!(!segment_exists(&conn, oldest_boundary));
    assert!(!segment_exists(&conn, old_boundary_2));
    assert!(!segment_exists(&conn, old_boundary_3));

    // Verify keeper boundary is preserved
    assert!(segment_exists(&conn, keeper_boundary));
}

#[test]
fn test_cleanup_boundary_exactly_at_cutoff() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Boundary exactly at the cutoff point
    let boundary_at_cutoff = insert_segment(&conn, 168, true, dummy_data);
    insert_segment(&conn, 167, false, dummy_data);

    // Recent data
    insert_segment(&conn, 50, true, dummy_data);
    insert_segment(&conn, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 4);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // Boundary exactly at cutoff should be preserved as it's not strictly less than cutoff
    // All 4 segments should remain
    assert_eq!(count_segments(&conn), 4);
    assert!(segment_exists(&conn, boundary_at_cutoff));
}

#[test]
fn test_cleanup_empty_database() {
    let conn = create_test_database();

    // No segments at all
    assert_eq!(count_segments(&conn), 0);

    // Run cleanup - should not error
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // Still empty
    assert_eq!(count_segments(&conn), 0);
}

#[test]
fn test_cleanup_verifies_sequential_deletion() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Create segments with IDs 1-10
    insert_segment(&conn, 200, true, dummy_data); // id 1
    insert_segment(&conn, 199, false, dummy_data); // id 2
    insert_segment(&conn, 198, false, dummy_data); // id 3
    let keeper_boundary = insert_segment(&conn, 180, true, dummy_data); // id 4
    insert_segment(&conn, 179, false, dummy_data); // id 5
    insert_segment(&conn, 50, true, dummy_data); // id 6
    insert_segment(&conn, 49, false, dummy_data); // id 7

    let (min_before, max_before) = get_segment_range(&conn);
    assert_eq!(min_before, Some(1));
    assert_eq!(max_before, Some(7));

    // Run cleanup
    cleanup_old_sections_with_retention(&conn, 168).unwrap();

    // After cleanup, min_id should be the keeper boundary
    let (min_after, max_after) = get_segment_range(&conn);
    assert_eq!(min_after, Some(keeper_boundary));
    assert_eq!(max_after, Some(7));

    // Verify segments 1-3 are deleted, 4-7 remain
    assert!(!segment_exists(&conn, 1));
    assert!(!segment_exists(&conn, 2));
    assert!(!segment_exists(&conn, 3));
    assert!(segment_exists(&conn, 4));
    assert!(segment_exists(&conn, 5));
    assert!(segment_exists(&conn, 6));
    assert!(segment_exists(&conn, 7));
}

#[test]
fn test_cleanup_uses_pending_section_id_as_keeper() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    // Use a fixed reference time for deterministic testing
    let now = Utc::now();

    // Create very old section
    let very_old_section = insert_segment_relative_to(&conn, now, 300, true, dummy_data);
    insert_segment_relative_to(&conn, now, 299, false, dummy_data);

    // Create pending section (old but should be preserved)
    let pending_boundary = insert_segment_relative_to(&conn, now, 250, true, dummy_data);
    let pending_segment_1 = insert_segment_relative_to(&conn, now, 249, false, dummy_data);
    let pending_segment_2 = insert_segment_relative_to(&conn, now, 248, false, dummy_data);

    // Get the section_id for the pending boundary
    let pending_section_id: i64 = conn
        .query_row(
            "SELECT section_id FROM segments WHERE id = ?1",
            [pending_boundary],
            |row| row.get(0),
        )
        .unwrap();

    // Set pending_section_id in metadata
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('pending_section_id', ?1)",
        [pending_section_id.to_string()],
    )
    .unwrap();

    // Section newer than pending (200h ago is more recent than 250h ago)
    let newer_section = insert_segment_relative_to(&conn, now, 200, true, dummy_data);
    insert_segment_relative_to(&conn, now, 199, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 9);

    // Run cleanup with 168 hour retention
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should keep: keeper (pending 250h) + sections with start_timestamp_ms >= cutoff (50h)
    // Should delete: sections with start_timestamp_ms < cutoff except keeper
    // Deleted: very_old (300h) + 1 segment, newer (200h) + 1 segment = 4 segments
    // Kept: pending (250h keeper) + 2 segments + recent (50h) + 1 segment = 5 segments
    assert_eq!(count_segments(&conn), 5);

    // Verify pending section is preserved (it's the keeper)
    assert!(segment_exists(&conn, pending_boundary));
    assert!(segment_exists(&conn, pending_segment_1));
    assert!(segment_exists(&conn, pending_segment_2));

    // Verify recent section is preserved (>= cutoff)
    // But newer_section at 200h is deleted (< cutoff and not keeper)
    assert!(!segment_exists(&conn, newer_section));

    // Verify very old section is deleted
    assert!(!segment_exists(&conn, very_old_section));
}

#[test]
fn test_cleanup_falls_back_when_no_pending_section_id() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Old section
    let old_boundary = insert_segment_relative_to(&conn, now, 300, true, dummy_data);
    insert_segment_relative_to(&conn, now, 299, false, dummy_data);

    // Keeper section (latest before cutoff)
    let keeper_boundary = insert_segment_relative_to(&conn, now, 175, true, dummy_data);
    insert_segment_relative_to(&conn, now, 174, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 6);

    // NO pending_section_id in metadata - should use fallback logic
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should use fallback: keep keeper boundary (175h) and everything newer
    assert_eq!(count_segments(&conn), 4);

    // Verify keeper is preserved
    assert!(segment_exists(&conn, keeper_boundary));

    // Verify old section is deleted
    assert!(!segment_exists(&conn, old_boundary));
}

#[test]
fn test_cleanup_falls_back_when_pending_section_has_no_segments() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Create sections with segments
    let old_boundary = insert_segment_relative_to(&conn, now, 300, true, dummy_data);
    insert_segment_relative_to(&conn, now, 299, false, dummy_data);

    let keeper_boundary = insert_segment_relative_to(&conn, now, 175, true, dummy_data);
    insert_segment_relative_to(&conn, now, 174, false, dummy_data);

    insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);

    // Create an empty section (section with no segments)
    let empty_section_id = (now.timestamp_millis() + 999999) * 1000;
    conn.execute(
        "INSERT INTO sections (id, start_timestamp_ms) VALUES (?1, ?2)",
        rusqlite::params![empty_section_id, now.timestamp_millis()],
    )
    .unwrap();

    // Set the empty section as pending
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('pending_section_id', ?1)",
        [empty_section_id.to_string()],
    )
    .unwrap();

    assert_eq!(count_segments(&conn), 6);

    // Cleanup should fall back to keeper logic since pending section has no segments
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should use fallback logic
    assert_eq!(count_segments(&conn), 4);

    assert!(segment_exists(&conn, keeper_boundary));
    assert!(!segment_exists(&conn, old_boundary));
}

#[test]
fn test_cleanup_preserves_pending_section_even_when_very_old() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Very old sections
    insert_segment_relative_to(&conn, now, 500, true, dummy_data);
    insert_segment_relative_to(&conn, now, 499, false, dummy_data);

    insert_segment_relative_to(&conn, now, 400, true, dummy_data);
    insert_segment_relative_to(&conn, now, 399, false, dummy_data);

    // Pending section (very old but should be preserved)
    let pending_boundary = insert_segment_relative_to(&conn, now, 350, true, dummy_data);
    let pending_segment = insert_segment_relative_to(&conn, now, 349, false, dummy_data);

    let pending_section_id: i64 = conn
        .query_row(
            "SELECT section_id FROM segments WHERE id = ?1",
            [pending_boundary],
            |row| row.get(0),
        )
        .unwrap();

    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('pending_section_id', ?1)",
        [pending_section_id.to_string()],
    )
    .unwrap();

    // Section that would normally be the keeper (latest before cutoff)
    insert_segment_relative_to(&conn, now, 175, true, dummy_data);
    insert_segment_relative_to(&conn, now, 174, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 10);

    // Cleanup with 168 hour retention
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should keep: keeper (pending 350h) + sections with start_timestamp_ms >= cutoff (50h)
    // Should delete: all other sections with start_timestamp_ms < cutoff
    // Deleted: 500h, 400h, 175h sections (6 segments)
    // Kept: pending (350h keeper) + 1 segment + recent (50h) + 1 segment = 4 segments
    assert_eq!(count_segments(&conn), 4);

    // Verify pending section is preserved
    assert!(segment_exists(&conn, pending_boundary));
    assert!(segment_exists(&conn, pending_segment));
}

#[test]
fn test_cleanup_with_pending_section_id_and_multiple_sections_before_cutoff() {
    let conn = create_test_database();
    let dummy_data = b"audio_data";

    let now = Utc::now();

    // Multiple old sections
    let very_old = insert_segment_relative_to(&conn, now, 500, true, dummy_data);
    insert_segment_relative_to(&conn, now, 499, false, dummy_data);

    let old_2 = insert_segment_relative_to(&conn, now, 400, true, dummy_data);
    insert_segment_relative_to(&conn, now, 399, false, dummy_data);

    let old_3 = insert_segment_relative_to(&conn, now, 300, true, dummy_data);
    insert_segment_relative_to(&conn, now, 299, false, dummy_data);

    // Pending section (should act as keeper)
    let pending_boundary = insert_segment_relative_to(&conn, now, 200, true, dummy_data);
    insert_segment_relative_to(&conn, now, 199, false, dummy_data);

    let pending_section_id: i64 = conn
        .query_row(
            "SELECT section_id FROM segments WHERE id = ?1",
            [pending_boundary],
            |row| row.get(0),
        )
        .unwrap();

    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('pending_section_id', ?1)",
        [pending_section_id.to_string()],
    )
    .unwrap();

    // Section between pending and cutoff
    insert_segment_relative_to(&conn, now, 175, true, dummy_data);
    insert_segment_relative_to(&conn, now, 174, false, dummy_data);

    // Recent section
    insert_segment_relative_to(&conn, now, 50, true, dummy_data);
    insert_segment_relative_to(&conn, now, 49, false, dummy_data);

    assert_eq!(count_segments(&conn), 12);

    // Cleanup
    cleanup_old_sections_with_params(&conn, 168, Some(now)).unwrap();

    // Should keep: keeper (pending 200h) + sections with start_timestamp_ms >= cutoff (50h)
    // Should delete: sections with start_timestamp_ms < cutoff except keeper
    // Deleted: 500h, 400h, 300h, 175h sections (8 segments)
    // Kept: pending (200h keeper) + 1 segment + recent (50h) + 1 segment = 4 segments
    assert_eq!(count_segments(&conn), 4);

    // Verify old sections are deleted
    assert!(!segment_exists(&conn, very_old));
    assert!(!segment_exists(&conn, old_2));
    assert!(!segment_exists(&conn, old_3));

    // Verify pending is preserved (it's the keeper)
    assert!(segment_exists(&conn, pending_boundary));
}
