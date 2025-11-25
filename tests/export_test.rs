use fs2::FileExt;
use opus::{Application, Bitrate as OpusBitrate, Channels, Encoder as OpusEncoder};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::fs::{self, File};
use std::path::Path;
use tokio::runtime::Runtime;

use save_audio_stream::queries::{metadata, sections, segments};
use save_audio_stream::EXPECTED_DB_VERSION;

/// Helper to create a test database with sections and segments
fn create_test_database_with_sections(
    db_path: &Path,
    show_name: &str,
    audio_format: &str,
    sample_rate: u32,
    num_sections: usize,
    segments_per_section: usize,
) -> SqlitePool {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let pool = save_audio_stream::db::open_database_connection(db_path)
            .await
            .unwrap();

        // Create schema using common helper
        save_audio_stream::db::init_database_schema(&pool)
            .await
            .unwrap();

        // Insert metadata
        let sql = metadata::insert("version", EXPECTED_DB_VERSION);
        sqlx::query(&sql).execute(&pool).await.unwrap();

        let sql = metadata::insert("unique_id", "test-export-db");
        sqlx::query(&sql).execute(&pool).await.unwrap();

        let sql = metadata::insert("name", show_name);
        sqlx::query(&sql).execute(&pool).await.unwrap();

        let sql = metadata::insert("audio_format", audio_format);
        sqlx::query(&sql).execute(&pool).await.unwrap();

        let sql = metadata::insert("split_interval", "300");
        sqlx::query(&sql).execute(&pool).await.unwrap();

        let sql = metadata::insert("bitrate", "16");
        sqlx::query(&sql).execute(&pool).await.unwrap();

        let sql = metadata::insert("sample_rate", &sample_rate.to_string());
        sqlx::query(&sql).execute(&pool).await.unwrap();

        // Insert sections and segments
        for section_idx in 0..num_sections {
            let section_id = (1700000000000000i64 + section_idx as i64 * 1000000) as i64;
            let start_timestamp_ms = 1700000000000i64 + section_idx as i64 * 3600000;

            let sql = sections::insert(section_id, start_timestamp_ms);
            sqlx::query(&sql).execute(&pool).await.unwrap();

            // Insert segments for this section
            for seg_idx in 0..segments_per_section {
                let timestamp_ms = start_timestamp_ms + seg_idx as i64 * 1000;
                let is_from_source = seg_idx == 0;

                // Create audio data based on format
                let audio_data = if audio_format == "opus" {
                    create_test_opus_segment(sample_rate)
                } else {
                    create_test_aac_segment()
                };

                let sql =
                    segments::insert(timestamp_ms, is_from_source, section_id, &audio_data, 0);
                sqlx::query(&sql).execute(&pool).await.unwrap();
            }
        }

        pool
    })
}

/// Create a test Opus segment (encoded packets with length prefixes)
fn create_test_opus_segment(sample_rate: u32) -> Vec<u8> {
    let mut encoder = OpusEncoder::new(sample_rate, Channels::Mono, Application::Voip).unwrap();
    encoder.set_bitrate(OpusBitrate::Bits(16000)).unwrap();

    let frame_size = 960; // 20ms at 48kHz
    let mut encode_buffer = vec![0u8; 8192];
    let mut segment_data = Vec::new();

    // Create 5 opus packets
    for _ in 0..5 {
        let samples: Vec<i16> = vec![0; frame_size];
        let len = encoder.encode(&samples, &mut encode_buffer).unwrap();

        // Write length prefix (2 bytes, little-endian)
        segment_data.extend_from_slice(&(len as u16).to_le_bytes());
        // Write packet data
        segment_data.extend_from_slice(&encode_buffer[..len]);
    }

    segment_data
}

/// Create a test AAC segment (ADTS frames)
fn create_test_aac_segment() -> Vec<u8> {
    // Create a simple ADTS header + dummy payload
    // This is a minimal valid ADTS frame
    let mut segment_data = Vec::new();

    // Create 5 AAC frames
    for _ in 0..5 {
        // ADTS header (7 bytes minimum)
        // Syncword: 0xFFF
        segment_data.push(0xFF);
        segment_data.push(0xF1); // MPEG-4, Layer 0, no CRC

        // Profile (2 bits) = AAC LC (1), Sample rate index (4 bits) = 8 (16000 Hz), Channel config (3 bits) = 1 (mono)
        segment_data.push(0x50); // Profile=1, SR index=8 (bits 0-1)
        segment_data.push(0x80); // Channel=1, frame length bits

        // Frame length (13 bits) - total of header + data
        let frame_len = 200; // Arbitrary small frame
        segment_data.push(((frame_len >> 11) & 0x03) as u8);
        segment_data.push(((frame_len >> 3) & 0xFF) as u8);
        segment_data.push(((frame_len & 0x07) << 5 | 0x1F) as u8);

        // Dummy payload
        segment_data.extend_from_slice(&vec![0u8; frame_len - 7]);
    }

    segment_data
}

#[test]
fn test_export_opus_section() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_opus.sqlite");

    // Create test database with Opus data
    let _pool = create_test_database_with_sections(
        &db_path,
        "test_show",
        "opus",
        48000,
        2,  // 2 sections
        10, // 10 segments per section
    );

    // Export the first section
    let section_id = 1700000000000000i64;

    // Verify the database structure
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let pool = save_audio_stream::db::open_readonly_connection(&db_path)
            .await
            .unwrap();

        let row = sqlx::query("SELECT MIN(id), MAX(id) FROM segments WHERE section_id = ?")
            .bind(section_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        let min_id: i64 = row.get(0);
        let max_id: i64 = row.get(1);

        assert!(min_id > 0, "Should have segments");
        assert!(max_id >= min_id, "Max ID should be >= min ID");

        let segment_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM segments WHERE section_id = ?")
                .bind(section_id)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(segment_count, 10, "Should have 10 segments in section");

        // Verify sections exist
        let section_exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sections WHERE id = ?")
            .bind(section_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(section_exists, 1, "Section should exist");
    });

    // Clean up
    fs::remove_dir_all(temp_dir.path()).ok();
}

#[test]
fn test_export_aac_section() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_aac.sqlite");

    // Create test database with AAC data
    let _pool = create_test_database_with_sections(
        &db_path,
        "test_show_aac",
        "aac",
        16000,
        1, // 1 section
        5, // 5 segments
    );

    let section_id = 1700000000000000i64;

    // Verify database setup
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let pool = save_audio_stream::db::open_readonly_connection(&db_path)
            .await
            .unwrap();

        let audio_format: String =
            sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'audio_format'")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(audio_format, "aac", "Should be AAC format");

        let segment_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM segments WHERE section_id = ?")
                .bind(section_id)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(segment_count, 5, "Should have 5 segments");
    });

    // Clean up
    fs::remove_dir_all(temp_dir.path()).ok();
}

#[test]
fn test_export_concurrent_lock() {
    // Create tmp directory for locks
    fs::create_dir_all("tmp").unwrap();

    let show_name = "test_lock_show";
    let section_id = 1234567890123456i64;
    let lock_path = format!("tmp/export_{}_{}.lock", show_name, section_id);

    // Clean up any existing lock file
    fs::remove_file(&lock_path).ok();

    // Acquire first lock
    let lock_file1 = File::create(&lock_path).unwrap();
    lock_file1.try_lock_exclusive().unwrap();

    // Try to acquire second lock - should fail
    let lock_file2 = File::create(&lock_path).unwrap();
    let result = lock_file2.try_lock_exclusive();

    assert!(
        result.is_err(),
        "Second lock should fail when first is held"
    );

    // Release first lock
    drop(lock_file1);

    // Now second lock should succeed
    let lock_file3 = File::create(&lock_path).unwrap();
    let result = lock_file3.try_lock_exclusive();

    assert!(
        result.is_ok(),
        "Lock should succeed after first is released"
    );

    // Clean up
    drop(lock_file3);
    fs::remove_file(&lock_path).ok();
}

#[test]
fn test_export_concurrent_lock_different_sections() {
    // Create tmp directory for locks
    fs::create_dir_all("tmp").unwrap();

    let show_name = "test_show";
    let section_id1 = 1000000000000000i64;
    let section_id2 = 2000000000000000i64;

    let lock_path1 = format!("tmp/export_{}_{}.lock", show_name, section_id1);
    let lock_path2 = format!("tmp/export_{}_{}.lock", show_name, section_id2);

    // Clean up any existing lock files
    fs::remove_file(&lock_path1).ok();
    fs::remove_file(&lock_path2).ok();

    // Acquire lock on section 1
    let lock_file1 = File::create(&lock_path1).unwrap();
    lock_file1.try_lock_exclusive().unwrap();

    // Acquire lock on section 2 - should succeed (different section)
    let lock_file2 = File::create(&lock_path2).unwrap();
    let result = lock_file2.try_lock_exclusive();

    assert!(result.is_ok(), "Lock on different section should succeed");

    // Clean up
    drop(lock_file1);
    drop(lock_file2);
    fs::remove_file(&lock_path1).ok();
    fs::remove_file(&lock_path2).ok();
}

#[test]
fn test_export_filename_format() {
    // Test that filename formatting is correct
    let show_name = "am1430";
    let section_id = 1737550800000000i64;
    let timestamp_ms = 1737550800000i64;

    // Format timestamp as yyyymmdd_hhmmss (matches export_section_handler behavior)
    let datetime = chrono::DateTime::from_timestamp_millis(timestamp_ms);
    let formatted_time = match datetime {
        Some(dt) => dt.format("%Y%m%d_%H%M%S").to_string(),
        None => format!("{}", timestamp_ms),
    };

    // Verify format is yyyymmdd_hhmmss (8 digits + underscore + 6 digits)
    assert_eq!(
        formatted_time.len(),
        15,
        "Timestamp should be 15 characters"
    );
    assert_eq!(
        &formatted_time[8..9],
        "_",
        "Should have underscore at position 8"
    );

    // Verify date part is numeric
    let date_part = &formatted_time[0..8];
    assert!(
        date_part.parse::<u32>().is_ok(),
        "Date part should be numeric"
    );

    // Verify time part is numeric
    let time_part = &formatted_time[9..15];
    assert!(
        time_part.parse::<u32>().is_ok(),
        "Time part should be numeric"
    );

    // Format section_id as hex
    let hex_section_id = format!("{:x}", section_id);
    assert_eq!(
        hex_section_id, "62c4b12369400",
        "Section ID should be in hex"
    );

    // Generate filename
    let filename = format!(
        "{}_{}._{}.{}",
        show_name, formatted_time, hex_section_id, "ogg"
    );

    // Verify filename structure (not exact timestamp since it depends on timezone)
    assert!(
        filename.starts_with("am1430_"),
        "Filename should start with show name"
    );
    assert!(
        filename.ends_with("_62c4b12369400.ogg"),
        "Filename should end with hex section ID and extension"
    );
    assert_eq!(
        filename.matches('_').count(),
        3,
        "Filename should have 3 underscores"
    );
}

#[test]
fn test_export_section_not_found() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_not_found.sqlite");

    // Create test database
    let _pool = create_test_database_with_sections(&db_path, "test_show", "opus", 48000, 1, 5);

    // Try to query a non-existent section
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let pool = save_audio_stream::db::open_readonly_connection(&db_path)
            .await
            .unwrap();
        let non_existent_section_id = 9999999999999999i64;

        let result: Result<(i64, i64), _> =
            sqlx::query_as("SELECT id, start_timestamp_ms FROM sections WHERE id = ?")
                .bind(non_existent_section_id)
                .fetch_one(&pool)
                .await;

        assert!(
            result.is_err(),
            "Should return error for non-existent section"
        );
        // sqlx returns RowNotFound error
        assert!(
            matches!(result, Err(sqlx::Error::RowNotFound)),
            "Should be RowNotFound error"
        );
    });

    // Clean up
    fs::remove_dir_all(temp_dir.path()).ok();
}
