use fs2::FileExt;
use reqwest::blocking::Client;
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;
use std::collections::HashSet;
use std::fs::File;
use std::path::PathBuf;
use tokio::runtime::Runtime;

use crate::constants::EXPECTED_DB_VERSION;
use crate::queries::{metadata, segments};

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

#[derive(Debug, Deserialize)]
struct SegmentData {
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    #[serde(with = "serde_bytes")]
    audio_data: Vec<u8>,
    section_id: i64,
    duration_samples: i64,
}

#[derive(Debug, Deserialize)]
struct SectionData {
    id: i64,
    start_timestamp_ms: i64,
}

/// Main entry point for syncing multiple shows
pub fn sync_shows(
    remote_url: String,
    local_dir: PathBuf,
    show_names_filter: Option<Vec<String>>,
    chunk_size: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Create local directory if it doesn't exist
    std::fs::create_dir_all(&local_dir)?;

    // Acquire exclusive lock to prevent multiple sync instances
    let lock_path = local_dir.join(".sync.lock");
    let _lock_file = File::create(&lock_path).map_err(|e| {
        format!(
            "Failed to create lock file '{}': {}",
            lock_path.display(),
            e
        )
    })?;
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

    let available_shows: HashSet<String> =
        shows_list.shows.iter().map(|s| s.name.clone()).collect();

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
        println!(
            "\n[{}/{}] Syncing show: {}",
            idx + 1,
            show_names.len(),
            show_name
        );

        // Sync single show - exit immediately on any error
        sync_single_show(&remote_url, &local_dir, show_name, chunk_size)?;

        println!(
            "[{}/{}] ✓ Show '{}' synced successfully",
            idx + 1,
            show_names.len(),
            show_name
        );
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    // Fetch remote metadata (no retry on network error)
    println!("[{}]   Fetching metadata from remote...", show_name);
    let metadata_url = format!("{}/api/sync/shows/{}/metadata", remote_url, show_name);
    let metadata: ShowMetadata = client
        .get(&metadata_url)
        .send()
        .map_err(|e| format!("Network error fetching metadata: {}", e))?
        .json()
        .map_err(|e| {
            format!(
                "Failed to parse metadata JSON: {}. \
                 This may indicate the remote server is running incompatible old code. \
                 Please upgrade the remote server.",
                e
            )
        })?;

    println!(
        "[{}]   Remote: unique_id={}, min_id={}, max_id={}",
        show_name, metadata.unique_id, metadata.min_id, metadata.max_id
    );

    // Validate remote version BEFORE doing anything else
    // This ensures we never sync from incompatible schema versions
    if metadata.version != EXPECTED_DB_VERSION {
        return Err(format!(
            "Remote database has unsupported schema version '{}' (expected '{}'). \
             Cannot sync from incompatible versions. Please upgrade the remote server.",
            metadata.version, EXPECTED_DB_VERSION
        )
        .into());
    }

    // Open or create local database
    let local_db_path = local_dir.join(format!("{}.sqlite", show_name));
    let pool = crate::db::open_database_connection_sync(&local_db_path)?;
    // Ensure schema exists before querying metadata (idempotent for existing DBs)
    crate::db::init_database_schema_sync(&pool)?;

    // Check if database exists (has metadata)
    let existing_unique_id: Option<String> = crate::db::query_metadata_sync(&pool, "unique_id")?;

    let start_id = if let Some(existing_unique_id) = existing_unique_id {
        // Existing database - validate and resume
        println!("[{}]   Found existing local database", show_name);
        println!("[{}]   Existing target unique_id: {}", show_name, existing_unique_id);

        // Validate local version matches expected version
        let local_version: String = crate::db::query_metadata_sync(&pool, "version")?
            .ok_or_else(|| "Failed to read version from local database: key not found")?;

        if local_version != EXPECTED_DB_VERSION {
            return Err(format!(
                "Local database has unsupported schema version '{}' (expected '{}'). \
                 Cannot sync with incompatible local database. Please delete and re-sync.",
                local_version, EXPECTED_DB_VERSION
            )
            .into());
        }

        // Validate source_unique_id matches remote unique_id
        let source_unique_id: String = crate::db::query_metadata_sync(&pool, "source_unique_id")?
            .ok_or_else(|| "Failed to read source_unique_id from local database: key not found")?;

        if source_unique_id != metadata.unique_id {
            return Err(format!(
                "Source database mismatch: local expects source '{}', but remote source is '{}'. Cannot sync from different source databases.",
                source_unique_id, metadata.unique_id
            )
            .into());
        }

        // Validate metadata matches (audio_format, split_interval, bitrate)
        // Version is already validated above
        validate_metadata(&pool, &metadata)?;

        // Get last synced ID
        let last_synced_id: i64 = crate::db::query_metadata_sync(&pool, "last_synced_id")?
            .map(|v| v.parse().unwrap_or(0))
            .unwrap_or(0);

        // Ensure last_boundary_end_id exists (for older databases that don't have it)
        let has_boundary_end = crate::db::metadata_exists_sync(&pool, "last_boundary_end_id")?;

        if !has_boundary_end {
            crate::db::insert_metadata_sync(&pool, "last_boundary_end_id", "0")?;
        }

        println!("[{}]   Resuming from segment {}", show_name, last_synced_id + 1);
        last_synced_id + 1
    } else {
        // New database - initialize
        // Generate a new unique_id for this target database
        let target_unique_id = crate::constants::generate_db_unique_id();
        println!("[{}]   Creating new local database", show_name);
        println!("[{}]   Initialized with target unique_id: {}", show_name, target_unique_id);

        // Initialize schema using common helper
        crate::db::init_database_schema_sync(&pool)?;

        // Insert metadata from remote
        crate::db::insert_metadata_sync(&pool, "version", &metadata.version)?;
        crate::db::insert_metadata_sync(&pool, "unique_id", &target_unique_id)?;
        crate::db::insert_metadata_sync(&pool, "name", &metadata.name)?;
        crate::db::insert_metadata_sync(&pool, "audio_format", &metadata.audio_format)?;
        crate::db::insert_metadata_sync(&pool, "split_interval", &metadata.split_interval)?;
        crate::db::insert_metadata_sync(&pool, "bitrate", &metadata.bitrate)?;
        crate::db::insert_metadata_sync(&pool, "sample_rate", &metadata.sample_rate)?;
        crate::db::insert_metadata_sync(&pool, "is_recipient", "true")?;
        // Store the source database's unique_id for validation on future syncs
        crate::db::insert_metadata_sync(&pool, "source_unique_id", &metadata.unique_id)?;
        crate::db::insert_metadata_sync(&pool, "last_synced_id", "0")?;
        crate::db::insert_metadata_sync(&pool, "last_boundary_end_id", "0")?;

        metadata.min_id
    };

    // Sync sections table first
    println!("[{}]   Syncing sections metadata...", show_name);
    let sections_url = format!("{}/api/sync/shows/{}/sections", remote_url, show_name);
    let remote_sections: Vec<SectionData> = client
        .get(&sections_url)
        .send()
        .map_err(|e| format!("Network error fetching sections: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse sections JSON: {}", e))?;

    // Insert sections into local database
    // Use INSERT OR IGNORE to avoid replacing existing sections
    // (REPLACE would trigger CASCADE delete of associated segments)
    let sections_count = remote_sections.len();
    for section in remote_sections {
        crate::db::insert_section_or_ignore_sync(&pool, section.id, section.start_timestamp_ms)?;
    }
    println!("[{}]   Synced {} sections", show_name, sections_count);

    // Sync segments in batches
    let target_max_id = metadata.max_id;
    let mut current_id = start_id;

    if current_id > target_max_id {
        println!(
            "[{}]   Already up to date (local_id {} >= remote_max {})",
            show_name, current_id - 1,
            target_max_id
        );
        return Ok(());
    }

    println!(
        "[{}]   Syncing segments {} to {} (chunk_size={})",
        show_name, current_id, target_max_id, chunk_size
    );

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
        let rt = Runtime::new().map_err(|e| format!("Failed to create runtime: {}", e))?;
        let mut last_boundary_end_id: Option<i64> = None;

        // Track section boundaries
        let mut prev_section_id: Option<i64> = None;
        let mut prev_id: Option<i64> = None;
        for segment in &segments {
            if let Some(prev_sec_id) = prev_section_id {
                if segment.section_id != prev_sec_id {
                    if let Some(prev) = prev_id {
                        last_boundary_end_id = Some(prev);
                    }
                }
            }
            prev_section_id = Some(segment.section_id);
            prev_id = Some(segment.id);
        }

        let last_id = segments.last().unwrap().id;
        let segments_ref = &segments;
        let pool_ref = &pool;
        let boundary_end = last_boundary_end_id;

        rt.block_on(async {
            let mut tx = pool_ref.begin().await?;

            for segment in segments_ref {
                let sql = segments::insert_with_id(
                    segment.id,
                    segment.timestamp_ms,
                    segment.is_timestamp_from_source,
                    &segment.audio_data,
                    segment.section_id,
                    segment.duration_samples,
                );
                sqlx::query(&sql).execute(&mut *tx).await?;
            }

            // Update last_synced_id (in same transaction)
            let sql = metadata::update("last_synced_id", &last_id.to_string());
            sqlx::query(&sql).execute(&mut *tx).await?;

            // Update last_boundary_end_id if we found a new boundary in this batch
            if let Some(boundary) = boundary_end {
                let sql = metadata::update("last_boundary_end_id", &boundary.to_string());
                sqlx::query(&sql).execute(&mut *tx).await?;
            }

            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        }).map_err(|e| format!("Database transaction error: {}", e))?;

        println!(
            "[{}]   Synced segments {} to {} ({}/{} segments, {:.1}% complete)",
            show_name, current_id,
            last_id,
            last_id - start_id + 1,
            target_max_id - start_id + 1,
            ((last_id - start_id + 1) as f64 / (target_max_id - start_id + 1) as f64) * 100.0
        );

        current_id = last_id + 1;
    }

    println!(
        "[{}]   ✓ Sync complete: {} segments",
        show_name, target_max_id - start_id + 1
    );
    Ok(())
}

/// Validate that local metadata matches remote metadata
fn validate_metadata(
    pool: &SqlitePool,
    remote: &ShowMetadata,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Validate version
    let local_version: String = crate::db::query_metadata_sync(pool, "version")?
        .ok_or_else(|| "Failed to read version from local database: key not found")?;
    if local_version != remote.version {
        return Err(format!(
            "Version mismatch: local='{}', remote='{}'. Cannot sync between different schema versions.",
            local_version, remote.version
        )
        .into());
    }

    // Validate audio_format
    let local_format: String = crate::db::query_metadata_sync(pool, "audio_format")?
        .ok_or_else(|| "Failed to read audio_format from local database: key not found")?;
    if local_format != remote.audio_format {
        return Err(format!(
            "Audio format mismatch: local='{}', remote='{}'",
            local_format, remote.audio_format
        )
        .into());
    }

    // Validate split_interval
    let local_interval: String = crate::db::query_metadata_sync(pool, "split_interval")?
        .ok_or_else(|| "Failed to read split_interval from local database: key not found")?;
    if local_interval != remote.split_interval {
        return Err(format!(
            "Split interval mismatch: local='{}', remote='{}'",
            local_interval, remote.split_interval
        )
        .into());
    }

    // Validate bitrate
    let local_bitrate: String = crate::db::query_metadata_sync(pool, "bitrate")?
        .ok_or_else(|| "Failed to read bitrate from local database: key not found")?;
    if local_bitrate != remote.bitrate {
        return Err(format!(
            "Bitrate mismatch: local='{}', remote='{}'",
            local_bitrate, remote.bitrate
        )
        .into());
    }

    Ok(())
}
