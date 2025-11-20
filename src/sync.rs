use fs2::FileExt;
use reqwest::blocking::Client;
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::File;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ShowInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ShowsList {
    shows: Vec<ShowInfo>,
}

#[derive(Debug, Deserialize)]
struct ShowMetadata {
    uuid: String,
    name: String,
    audio_format: String,
    split_interval: String,
    bitrate: String,
    sample_rate: String,
    version: String,
    min_id: i64,
    max_id: i64,
}

#[derive(Debug, Deserialize)]
struct SegmentData {
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    #[serde(with = "serde_bytes")]
    audio_data: Vec<u8>,
}


/// Main entry point for syncing multiple shows
pub fn sync_shows(
    remote_url: String,
    local_dir: PathBuf,
    show_names_filter: Option<Vec<String>>,
    chunk_size: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create local directory if it doesn't exist
    std::fs::create_dir_all(&local_dir)?;

    // Acquire exclusive lock to prevent multiple sync instances
    let lock_path = local_dir.join(".sync.lock");
    let _lock_file = File::create(&lock_path)
        .map_err(|e| format!("Failed to create lock file '{}': {}", lock_path.display(), e))?;
    _lock_file.try_lock_exclusive().map_err(|_| {
        format!(
            "Another sync is already running. Lock file: {}",
            lock_path.display()
        )
    })?;
    // Lock will be held until _lock_file is dropped (end of function)

    // Fetch available shows from remote server
    println!("Fetching available shows from remote...");
    let client = Client::new();
    let shows_url = format!("{}/api/sync/shows", remote_url);

    let shows_list: ShowsList = client
        .get(&shows_url)
        .send()
        .map_err(|e| format!("Failed to fetch shows list from remote: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse shows list response: {}", e))?;

    let available_shows: HashSet<String> = shows_list
        .shows
        .iter()
        .map(|s| s.name.clone())
        .collect();

    if available_shows.is_empty() {
        println!("No shows available on remote server");
        return Ok(());
    }

    // Determine which shows to sync
    let show_names: Vec<String> = match show_names_filter {
        Some(whitelist) => {
            // Validate that all whitelisted shows exist on remote
            let missing_shows: Vec<String> = whitelist
                .iter()
                .filter(|name| !available_shows.contains(*name))
                .cloned()
                .collect();

            if !missing_shows.is_empty() {
                return Err(format!(
                    "The following show(s) in whitelist were not found on remote: {}",
                    missing_shows.join(", ")
                )
                .into());
            }

            println!("Using whitelist: {} show(s)", whitelist.len());
            whitelist
        }
        None => {
            // Sync all available shows
            let all_shows: Vec<String> = available_shows.into_iter().collect();
            println!("Syncing all available shows: {} show(s)", all_shows.len());
            all_shows
        }
    };

    if show_names.is_empty() {
        println!("No shows to sync");
        return Ok(());
    }

    println!("Starting sync of {} show(s)", show_names.len());

    // Process each show sequentially
    for (idx, show_name) in show_names.iter().enumerate() {
        println!("\n[{}/{}] Syncing show: {}", idx + 1, show_names.len(), show_name);

        // Sync single show - exit immediately on any error
        sync_single_show(&remote_url, &local_dir, show_name, chunk_size)?;

        println!("[{}/{}] ✓ Show '{}' synced successfully", idx + 1, show_names.len(), show_name);
    }

    println!("\n✓ All {} show(s) synced successfully", show_names.len());
    Ok(())
}

/// Sync a single show from remote to local
fn sync_single_show(
    remote_url: &str,
    local_dir: &PathBuf,
    show_name: &str,
    chunk_size: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();

    // Fetch remote metadata (no retry on network error)
    println!("  Fetching metadata from remote...");
    let metadata_url = format!("{}/api/sync/shows/{}/metadata", remote_url, show_name);
    let metadata: ShowMetadata = client
        .get(&metadata_url)
        .send()
        .map_err(|e| format!("Network error fetching metadata: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse metadata JSON: {}", e))?;

    println!("  Remote: uuid={}, min_id={}, max_id={}", metadata.uuid, metadata.min_id, metadata.max_id);

    // Open or create local database
    let local_db_path = local_dir.join(format!("{}.sqlite", show_name));
    let mut conn = Connection::open(&local_db_path)?;

    // Check if database exists (has metadata)
    let existing_uuid: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'uuid'",
            [],
            |row| row.get(0),
        )
        .ok();

    let start_id = if let Some(_existing_uuid) = existing_uuid {
        // Existing database - validate and resume
        println!("  Found existing local database");

        // Validate source_session_id matches remote uuid
        let source_session_id: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'source_session_id'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| "Local database missing source_session_id")?;

        if source_session_id != metadata.uuid {
            return Err(format!(
                "Source mismatch: local expects '{}', remote is '{}'",
                source_session_id, metadata.uuid
            )
            .into());
        }

        // Validate metadata matches
        validate_metadata(&conn, &metadata)?;

        // Get last synced ID
        let last_synced_id: i64 = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'last_synced_id'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val.parse().unwrap_or(0))
                },
            )
            .unwrap_or(0);

        // Ensure last_boundary_end_id exists (for older databases that don't have it)
        let has_boundary_end: bool = conn
            .query_row(
                "SELECT 1 FROM metadata WHERE key = 'last_boundary_end_id'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if !has_boundary_end {
            conn.execute(
                "INSERT INTO metadata (key, value) VALUES ('last_boundary_end_id', '0')",
                [],
            )?;
        }

        println!("  Resuming from segment {}", last_synced_id + 1);
        last_synced_id + 1
    } else {
        // New database - initialize
        println!("  Creating new local database");

        // Create tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS segments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms INTEGER NOT NULL,
                is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
                audio_data BLOB NOT NULL
            )",
            [],
        )?;

        // Enable WAL mode
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        // Insert metadata from remote
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('version', ?1)",
            [&metadata.version],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('uuid', ?1)",
            [&existing_uuid.unwrap_or_else(|| format!("local_{}", uuid::Uuid::new_v4()))],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('name', ?1)",
            [&metadata.name],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('audio_format', ?1)",
            [&metadata.audio_format],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('split_interval', ?1)",
            [&metadata.split_interval],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('bitrate', ?1)",
            [&metadata.bitrate],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('sample_rate', ?1)",
            [&metadata.sample_rate],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('is_recipient', 'true')",
            [],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('source_session_id', ?1)",
            [&metadata.uuid],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('last_synced_id', '0')",
            [],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES ('last_boundary_end_id', '0')",
            [],
        )?;

        println!("  Initialized with source_session_id={}", metadata.uuid);
        metadata.min_id
    };

    // Sync segments in chunks
    let target_max_id = metadata.max_id;
    let mut current_id = start_id;

    if current_id > target_max_id {
        println!("  Already up to date (local_id {} >= remote_max {})", current_id - 1, target_max_id);
        return Ok(());
    }

    println!("  Syncing segments {} to {} (chunk_size={})", current_id, target_max_id, chunk_size);

    while current_id <= target_max_id {
        let end_id = std::cmp::min(current_id + chunk_size as i64 - 1, target_max_id);

        // Fetch segments (no retry on network error)
        let segments_url = format!(
            "{}/api/sync/shows/{}/segments?start_id={}&end_id={}&limit={}",
            remote_url, show_name, current_id, end_id, chunk_size
        );

        let segments: Vec<SegmentData> = client
            .get(&segments_url)
            .send()
            .map_err(|e| format!("Network error fetching segments: {}", e))?
            .json()
            .map_err(|e| format!("Failed to parse segments JSON: {}", e))?;

        if segments.is_empty() {
            return Err(format!("No segments returned for range {}-{}", current_id, end_id).into());
        }

        // Insert segments into local database (all operations in one transaction)
        let tx = conn.transaction()?;
        let mut last_boundary_end_id: Option<i64> = None;
        {
            let mut prev_id: Option<i64> = None;
            for segment in &segments {
                // Check if current segment is a boundary (new session start)
                // If so, previous segment is the end of a complete session
                if segment.is_timestamp_from_source == 1 {
                    if let Some(prev) = prev_id {
                        last_boundary_end_id = Some(prev);
                    }
                }

                tx.execute(
                    "INSERT INTO segments (id, timestamp_ms, is_timestamp_from_source, audio_data) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![segment.id, segment.timestamp_ms, segment.is_timestamp_from_source, &segment.audio_data],
                )?;

                prev_id = Some(segment.id);
            }

            // Update last_synced_id (in same transaction)
            let last_id = segments.last().unwrap().id;
            tx.execute(
                "UPDATE metadata SET value = ?1 WHERE key = 'last_synced_id'",
                [last_id.to_string()],
            )?;

            // Update last_boundary_end_id if we found a new boundary in this batch (in same transaction)
            if let Some(boundary_end) = last_boundary_end_id {
                tx.execute(
                    "UPDATE metadata SET value = ?1 WHERE key = 'last_boundary_end_id'",
                    [boundary_end.to_string()],
                )?;
            }
        }
        tx.commit()?;

        let last_id = segments.last().unwrap().id;

        println!(
            "  Synced segments {} to {} ({}/{} segments, {:.1}% complete)",
            current_id,
            last_id,
            last_id - start_id + 1,
            target_max_id - start_id + 1,
            ((last_id - start_id + 1) as f64 / (target_max_id - start_id + 1) as f64) * 100.0
        );

        current_id = last_id + 1;
    }

    println!("  ✓ Sync complete: {} segments", target_max_id - start_id + 1);
    Ok(())
}

/// Validate that local metadata matches remote metadata
fn validate_metadata(
    conn: &Connection,
    remote: &ShowMetadata,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate audio_format
    let local_format: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'audio_format'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "Local database missing audio_format")?;
    if local_format != remote.audio_format {
        return Err(format!(
            "Audio format mismatch: local='{}', remote='{}'",
            local_format, remote.audio_format
        )
        .into());
    }

    // Validate split_interval
    let local_interval: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'split_interval'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "Local database missing split_interval")?;
    if local_interval != remote.split_interval {
        return Err(format!(
            "Split interval mismatch: local='{}', remote='{}'",
            local_interval, remote.split_interval
        )
        .into());
    }

    // Validate bitrate
    let local_bitrate: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'bitrate'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "Local database missing bitrate")?;
    if local_bitrate != remote.bitrate {
        return Err(format!(
            "Bitrate mismatch: local='{}', remote='{}'",
            local_bitrate, remote.bitrate
        )
        .into());
    }

    Ok(())
}
