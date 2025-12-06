use reqwest::blocking::Client;
use serde::Deserialize;
use sqlx::postgres::PgPool;
use std::collections::HashSet;
use std::sync::mpsc::{self, RecvTimeoutError};

use crate::config::SyncConfig;
use crate::constants::EXPECTED_DB_VERSION;
use crate::db_postgres::{self, SyncDbPg};
use crate::queries::{metadata, segments};
use crate::segment_wire;

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
    bitrate: String,
    sample_rate: String,
    version: String,
    min_id: i64,
    max_id: i64,
}

#[derive(Debug, Deserialize)]
struct SectionData {
    id: i64,
    start_timestamp_ms: i64,
}

/// Response from find_by_timestamp endpoint
#[derive(Debug, Deserialize)]
struct SectionMatch {
    id: i64,
    start_timestamp_ms: i64,
}

#[derive(Debug, Deserialize)]
struct FindSectionResponse {
    after_section: Option<SectionMatch>,
    before_or_equal_section: Option<SectionMatch>,
    source_unique_id: String,
    min_id: i64,
    max_id: i64,
}

#[derive(Debug, Deserialize)]
struct SectionSegmentRange {
    min_id: Option<i64>,
    max_id: Option<i64>,
}

/// Get the PostgreSQL database name for a show
/// Pattern: save_audio_{prefix}_{show_name}
/// Default prefix is "show", resulting in "save_audio_show_{show_name}"
pub fn get_pg_database_name(prefix: &str, show_name: &str) -> String {
    format!("save_audio_{}_{}", prefix, show_name)
}

/// Lease name for global sync coordination
pub const SYNC_LEASE_NAME: &str = "sync";

/// Result of sync_shows indicating whether sync was performed
#[derive(Debug)]
pub enum SyncResult {
    /// Sync completed successfully
    Completed,
    /// Sync was skipped because another instance holds the lease
    Skipped,
}

/// Result of replace_source operation
#[derive(Debug)]
pub enum ReplaceSourceResult {
    /// Successfully replaced source, ready to sync from matched section
    Replaced {
        old_source_id: String,
        new_source_id: String,
        matched_section_id: i64,
        matched_section_timestamp_ms: i64,
        resume_from_segment_id: i64,
    },
    /// Skipped because another operation holds the lease
    Skipped,
    /// No existing data in receiver - will start fresh from new source
    FreshStart { new_source_id: String },
}

/// Get retention hours for a show from config (None if not configured)
fn get_show_retention_hours(config: &SyncConfig, show_name: &str) -> Option<i64> {
    config.shows.as_ref().and_then(|shows| {
        shows
            .iter()
            .find(|s| s.name == show_name)
            .and_then(|s| s.retention_hours)
    })
}

/// Clean up old sections with minimum retention guarantee (keeper section)
///
/// This ensures at least one complete section is preserved at the retention boundary.
/// Returns the number of sections deleted.
fn cleanup_show_retention_pg(
    db: &SyncDbPg,
    show_name: &str,
    retention_hours: i64,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::try_hours(retention_hours).expect("Valid hours");
    let cutoff_ms = cutoff.timestamp_millis();

    // Find keeper section (latest section before cutoff - ensures minimum retention)
    let keeper_section_id =
        crate::db_postgres::get_latest_section_before_cutoff_pg_sync(db, cutoff_ms)?;

    if let Some(keeper_id) = keeper_section_id {
        let deleted = crate::db_postgres::delete_old_sections_pg_sync(db, cutoff_ms, keeper_id)?;
        if deleted > 0 {
            println!(
                "[{}] Cleaned up {} old sections (retention: {}h, keeper: {})",
                show_name, deleted, retention_hours, keeper_id
            );
        }
        Ok(deleted)
    } else {
        Ok(0)
    }
}

/// Main entry point for syncing multiple shows (PostgreSQL version)
/// Handles lease acquisition, renewal, and release internally.
/// Returns SyncResult::Skipped if another instance is already syncing.
pub fn sync_shows(
    config: &SyncConfig,
    password: &str,
    global_pool: &PgPool,
) -> Result<SyncResult, Box<dyn std::error::Error + Send + Sync>> {
    // Generate unique holder ID for this sync operation
    let holder_id = format!(
        "sync-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    // Use custom lease name from config if provided, otherwise use default
    let lease_name = config
        .lease_name
        .as_deref()
        .unwrap_or(SYNC_LEASE_NAME);

    let lease_duration_ms = db_postgres::DEFAULT_LEASE_DURATION_MS;

    // Try to acquire the lease
    let rt = tokio::runtime::Runtime::new()?;
    let acquired = rt.block_on(async {
        db_postgres::try_acquire_lease_pg(
            global_pool,
            lease_name,
            &holder_id,
            lease_duration_ms,
        )
        .await
    })?;

    if !acquired {
        println!("[Sync] Lease held by another instance, skipping");
        return Ok(SyncResult::Skipped);
    }

    println!("[Sync] Acquired sync lease");

    // Spawn lease renewal thread
    let renewal_pool = global_pool.clone();
    let renewal_holder_id = holder_id.clone();
    let renewal_lease_name = lease_name.to_string();
    let renewal_interval_ms = (lease_duration_ms / 4).clamp(10_000, 30_000) as u64;
    let renewal_interval = std::time::Duration::from_millis(renewal_interval_ms);
    let (stop_tx, stop_rx) = mpsc::channel();

    let renewal_handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create renewal runtime");
        loop {
            match stop_rx.recv_timeout(renewal_interval) {
                Ok(_) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    let renewed = rt.block_on(async {
                        db_postgres::renew_lease_pg(
                            &renewal_pool,
                            &renewal_lease_name,
                            &renewal_holder_id,
                            lease_duration_ms,
                        )
                        .await
                    });
                    match renewed {
                        Ok(true) => println!("[Sync] Lease renewed"),
                        Ok(false) => {
                            eprintln!("[Sync] Warning: Failed to renew lease - lost ownership")
                        }
                        Err(e) => eprintln!("[Sync] Warning: Lease renewal error: {}", e),
                    }
                }
            }
        }
    });

    // Run the actual sync, capturing the result
    let sync_result = sync_shows_internal(config, password);

    // Stop renewal thread
    let _ = stop_tx.send(());
    let _ = renewal_handle.join();

    // Release the lease
    let _ = rt.block_on(async {
        db_postgres::release_lease_pg(global_pool, lease_name, &holder_id).await
    });
    println!("[Sync] Released sync lease");

    // Return the sync result
    sync_result?;
    Ok(SyncResult::Completed)
}

/// Replace the source database for an existing receiver
///
/// This operation:
/// 1. Acquires the sync lease (prevents concurrent sync/replace)
/// 2. Gets the receiver's current latest section timestamp
/// 3. Queries the new source for a matching section
/// 4. Updates source_unique_id and last_synced_id atomically
pub fn replace_source(
    config: &SyncConfig,
    password: &str,
    global_pool: &PgPool,
    show_name: &str,
) -> Result<ReplaceSourceResult, Box<dyn std::error::Error + Send + Sync>> {
    let remote_url = &config.remote_url;

    // Generate unique holder ID for this operation
    let holder_id = format!(
        "replace-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    // Use custom lease name from config if provided, otherwise use default
    let lease_name = config.lease_name.as_deref().unwrap_or(SYNC_LEASE_NAME);
    let lease_duration_ms = db_postgres::DEFAULT_LEASE_DURATION_MS;

    // Try to acquire the lease
    let rt = tokio::runtime::Runtime::new()?;
    let acquired = rt.block_on(async {
        db_postgres::try_acquire_lease_pg(global_pool, lease_name, &holder_id, lease_duration_ms)
            .await
    })?;

    if !acquired {
        println!("[ReplaceSource] Lease held by another instance, skipping");
        return Ok(ReplaceSourceResult::Skipped);
    }

    println!("[ReplaceSource] Acquired sync lease");

    // Run the replace operation
    let result = replace_source_internal(config, password, show_name, remote_url);

    // Release the lease
    let _ = rt.block_on(async {
        db_postgres::release_lease_pg(global_pool, lease_name, &holder_id).await
    });
    println!("[ReplaceSource] Released sync lease");

    result
}

/// Internal implementation of replace_source (without lease handling)
fn replace_source_internal(
    config: &SyncConfig,
    password: &str,
    show_name: &str,
    remote_url: &str,
) -> Result<ReplaceSourceResult, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    // Connect to receiver's PostgreSQL database
    let database_name = get_pg_database_name(&config.database.prefix, show_name);
    println!(
        "[ReplaceSource] Connecting to PostgreSQL database '{}'...",
        database_name
    );

    let db = SyncDbPg::connect(&config.database.url, password, &database_name).map_err(|e| {
        format!(
            "Failed to connect to PostgreSQL database '{}': {}",
            database_name, e
        )
    })?;

    // Check if receiver has any sections
    let receiver_max_timestamp = db_postgres::get_max_section_timestamp_pg_sync(&db)?;

    // Get current source_unique_id (if any)
    let old_source_id: Option<String> =
        db_postgres::query_metadata_pg_sync(&db, "source_unique_id")?;

    // If receiver has no sections, it's a fresh start - just validate and update source_unique_id
    let receiver_max_ts = match receiver_max_timestamp {
        Some(ts) => ts,
        None => {
            println!("[ReplaceSource] Receiver has no sections, fetching new source metadata...");

            // Fetch new source metadata to get unique_id
            let metadata_url = format!("{}/api/sync/shows/{}/metadata", remote_url, show_name);
            let metadata: ShowMetadata = client
                .get(&metadata_url)
                .send()
                .map_err(|e| format!("Network error fetching metadata: {}", e))?
                .json()
                .map_err(|e| format!("Failed to parse metadata JSON: {}", e))?;

            // Validate version
            if metadata.version != EXPECTED_DB_VERSION {
                return Err(format!(
                    "Remote database has unsupported schema version '{}' (expected '{}')",
                    metadata.version, EXPECTED_DB_VERSION
                )
                .into());
            }

            // Update source_unique_id
            if old_source_id.is_some() {
                db_postgres::upsert_metadata_pg_sync(&db, "source_unique_id", &metadata.unique_id)?;
            }

            println!(
                "[ReplaceSource] Receiver has no data, will start fresh from source '{}'",
                metadata.unique_id
            );
            return Ok(ReplaceSourceResult::FreshStart {
                new_source_id: metadata.unique_id,
            });
        }
    };

    println!(
        "[ReplaceSource] Receiver latest section timestamp: {}",
        receiver_max_ts
    );

    // Query new source for section matching
    let find_url = format!(
        "{}/api/sync/shows/{}/sections/find_by_timestamp?timestamp_ms={}",
        remote_url, show_name, receiver_max_ts
    );
    println!("[ReplaceSource] Querying new source for matching section...");

    let find_response: FindSectionResponse = client
        .get(&find_url)
        .send()
        .map_err(|e| format!("Network error querying source: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse find_by_timestamp response: {}", e))?;

    let new_source_id = find_response.source_unique_id.clone();
    println!("[ReplaceSource] New source unique_id: {}", new_source_id);

    // Determine matched section
    let matched_section = if let Some(after) = &find_response.after_section {
        println!(
            "[ReplaceSource] Found section after timestamp: id={}, ts={}",
            after.id, after.start_timestamp_ms
        );
        after
    } else if let Some(before) = &find_response.before_or_equal_section {
        println!(
            "[ReplaceSource] Found section before/equal to timestamp: id={}, ts={}",
            before.id, before.start_timestamp_ms
        );
        before
    } else {
        // Check if source has any data at all
        if find_response.min_id > find_response.max_id {
            // Source is empty
            println!("[ReplaceSource] New source has no data, will start fresh");

            // Update metadata
            db.block_on(async {
                let sql = metadata::update_pg("source_unique_id", &new_source_id);
                sqlx::query(&sql).execute(db.pool()).await?;

                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            })?;

            return Ok(ReplaceSourceResult::FreshStart {
                new_source_id,
            });
        }

        return Err(format!(
            "No matching section found on new source. Receiver timestamp: {}, \
             Source has sections but none match. This may indicate the new source \
             is too far behind in time.",
            receiver_max_ts
        )
        .into());
    };

    // Get min segment ID for the matched section
    let segment_range_url = format!(
        "{}/api/sync/shows/{}/sections/{}/segment_range",
        remote_url, show_name, matched_section.id
    );

    let segment_range: SectionSegmentRange = client
        .get(&segment_range_url)
        .send()
        .map_err(|e| format!("Network error querying segment range: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse segment_range response: {}", e))?;

    let resume_from_segment_id = match segment_range.min_id {
        Some(min_id) => min_id,
        None => {
            return Err(format!(
                "Matched section {} has no segments on new source",
                matched_section.id
            )
            .into());
        }
    };

    println!(
        "[ReplaceSource] Will resume from segment {} (section {} starts at ts={})",
        resume_from_segment_id, matched_section.id, matched_section.start_timestamp_ms
    );

    // Get old source_id for return value
    let old_source_id_for_return = old_source_id.unwrap_or_else(|| "none".to_string());

    // Update metadata atomically
    db.block_on(async {
        let mut tx = db.pool().begin().await?;

        // Update source_unique_id
        let sql = metadata::update_pg("source_unique_id", &new_source_id);
        sqlx::query(&sql).execute(&mut *tx).await?;

        // Set last_synced_id to resume_from_segment_id - 1
        // This way the next sync will start from resume_from_segment_id
        let last_synced = (resume_from_segment_id - 1).to_string();
        let sql = metadata::update_pg("last_synced_id", &last_synced);
        sqlx::query(&sql).execute(&mut *tx).await?;

        tx.commit().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;

    println!(
        "[ReplaceSource] Successfully replaced source: {} -> {}",
        old_source_id_for_return, new_source_id
    );
    println!("[ReplaceSource] Run 'sync' command to continue syncing from new source");

    Ok(ReplaceSourceResult::Replaced {
        old_source_id: old_source_id_for_return,
        new_source_id,
        matched_section_id: matched_section.id,
        matched_section_timestamp_ms: matched_section.start_timestamp_ms,
        resume_from_segment_id,
    })
}

/// Internal sync implementation (without lease handling)
fn sync_shows_internal(
    config: &SyncConfig,
    password: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let remote_url = &config.remote_url;
    let chunk_size = config.chunk_size.unwrap_or(100);

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
    let show_names: Vec<String> = match &config.shows {
        Some(show_configs) => {
            // Extract names from ShowConfig
            let names: Vec<String> = show_configs.iter().map(|c| c.name.clone()).collect();

            // Validate that all whitelisted shows exist on remote
            let missing_shows: Vec<String> = names
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

            println!("Using whitelist: {} show(s)", names.len());
            names
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
        // Get retention hours for this show (None if not configured)
        let retention_hours = get_show_retention_hours(config, show_name);

        println!(
            "\n[{}/{}] Syncing show: {}{}",
            idx + 1,
            show_names.len(),
            show_name,
            retention_hours
                .map(|h| format!(" (retention: {}h)", h))
                .unwrap_or_default()
        );

        // Sync single show - exit immediately on any error
        sync_single_show(
            remote_url,
            &config.database.url,
            password,
            show_name,
            chunk_size,
            &config.database.prefix,
            retention_hours,
        )?;

        // Run retention cleanup if configured
        if let Some(hours) = retention_hours {
            let database_name = get_pg_database_name(&config.database.prefix, show_name);
            match SyncDbPg::connect(&config.database.url, password, &database_name) {
                Ok(db) => {
                    if let Err(e) = cleanup_show_retention_pg(&db, show_name, hours) {
                        eprintln!("[{}] Warning: Retention cleanup failed: {}", show_name, e);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[{}] Warning: Could not connect to database for cleanup: {}",
                        show_name, e
                    );
                }
            }
        }

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

/// Sync a single show from remote to local PostgreSQL database
fn sync_single_show(
    remote_url: &str,
    postgres_url: &str,
    password: &str,
    show_name: &str,
    chunk_size: u64,
    database_prefix: &str,
    retention_hours: Option<i64>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    // Calculate cutoff timestamp if retention is configured
    // This is used to skip fetching old data that will be deleted anyway
    let cutoff_ts = retention_hours.map(|hours| {
        let cutoff = chrono::Utc::now() - chrono::Duration::try_hours(hours).expect("Valid hours");
        cutoff.timestamp_millis()
    });

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
    if metadata.version != EXPECTED_DB_VERSION {
        return Err(format!(
            "Remote database has unsupported schema version '{}' (expected '{}'). \
             Cannot sync from incompatible versions. Please upgrade the remote server.",
            metadata.version, EXPECTED_DB_VERSION
        )
        .into());
    }

    // Connect to PostgreSQL database
    let database_name = get_pg_database_name(database_prefix, show_name);
    println!(
        "[{}]   Connecting to PostgreSQL database '{}'...",
        show_name, database_name
    );

    let db = SyncDbPg::connect(postgres_url, password, &database_name).map_err(|e| {
        format!(
            "Failed to connect to PostgreSQL database '{}': {}",
            database_name, e
        )
    })?;

    // Initialize schema (idempotent)
    crate::db_postgres::init_database_schema_pg_sync(&db)?;

    // Check if database exists (has metadata)
    let existing_unique_id: Option<String> =
        crate::db_postgres::query_metadata_pg_sync(&db, "unique_id")?;

    let start_id = if let Some(existing_unique_id) = existing_unique_id {
        // Existing database - validate and resume
        println!("[{}]   Found existing local database", show_name);
        println!(
            "[{}]   Existing target unique_id: {}",
            show_name, existing_unique_id
        );

        // Validate local version matches expected version
        let local_version: String = crate::db_postgres::query_metadata_pg_sync(&db, "version")?
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
        let source_unique_id: String =
            crate::db_postgres::query_metadata_pg_sync(&db, "source_unique_id")?.ok_or_else(
                || "Failed to read source_unique_id from local database: key not found",
            )?;

        if source_unique_id != metadata.unique_id {
            return Err(format!(
                "Source database mismatch: local expects source '{}', but remote source is '{}'. Cannot sync from different source databases.",
                source_unique_id, metadata.unique_id
            )
            .into());
        }

        // Validate metadata matches
        validate_metadata(&db, &metadata)?;

        // Get last synced ID
        let last_synced_id: i64 =
            crate::db_postgres::query_metadata_pg_sync(&db, "last_synced_id")?
                .map(|v| v.parse().unwrap_or(0))
                .unwrap_or(0);

        println!(
            "[{}]   Resuming from segment {}",
            show_name,
            last_synced_id + 1
        );
        last_synced_id + 1
    } else {
        // New database - initialize
        let target_unique_id = crate::constants::generate_db_unique_id();
        println!("[{}]   Creating new local database", show_name);
        println!(
            "[{}]   Initialized with target unique_id: {}",
            show_name, target_unique_id
        );

        // Insert metadata from remote
        crate::db_postgres::insert_metadata_pg_sync(&db, "version", &metadata.version)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "unique_id", &target_unique_id)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "name", &metadata.name)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "audio_format", &metadata.audio_format)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "bitrate", &metadata.bitrate)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "sample_rate", &metadata.sample_rate)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "is_recipient", "true")?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "source_unique_id", &metadata.unique_id)?;
        crate::db_postgres::insert_metadata_pg_sync(&db, "last_synced_id", "0")?;

        metadata.min_id
    };

    // Sync sections table first
    println!("[{}]   Syncing sections metadata...", show_name);
    let sections_url = match cutoff_ts {
        Some(ts) => format!(
            "{}/api/sync/shows/{}/sections?cutoff_ts={}",
            remote_url, show_name, ts
        ),
        None => format!("{}/api/sync/shows/{}/sections", remote_url, show_name),
    };
    let remote_sections: Vec<SectionData> = client
        .get(&sections_url)
        .send()
        .map_err(|e| format!("Network error fetching sections: {}", e))?
        .json()
        .map_err(|e| format!("Failed to parse sections JSON: {}", e))?;

    // Insert sections into local database
    let sections_count = remote_sections.len();
    for section in remote_sections {
        crate::db_postgres::insert_section_or_ignore_pg_sync(
            &db,
            section.id,
            section.start_timestamp_ms,
        )?;
    }
    println!("[{}]   Synced {} sections", show_name, sections_count);

    // Sync segments in batches
    let target_max_id = metadata.max_id;
    let mut current_id = start_id;

    if current_id > target_max_id {
        println!(
            "[{}]   Already up to date (local_id {} >= remote_max {})",
            show_name,
            current_id - 1,
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

        // Fetch segments as binary (no retry on network error)
        let segments_url = match cutoff_ts {
            Some(ts) => format!(
                "{}/api/sync/shows/{}/segments?start_id={}&end_id={}&limit={}&cutoff_ts={}",
                remote_url, show_name, current_id, end_id, chunk_size, ts
            ),
            None => format!(
                "{}/api/sync/shows/{}/segments?start_id={}&end_id={}&limit={}",
                remote_url, show_name, current_id, end_id, chunk_size
            ),
        };

        let response = client
            .get(&segments_url)
            .send()
            .map_err(|e| format!("Network error fetching segments: {}", e))?;

        // Verify content type
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if content_type != segment_wire::CONTENT_TYPE {
            return Err(format!(
                "Unexpected content type: '{}'. Server may be running old version (expected '{}')",
                content_type,
                segment_wire::CONTENT_TYPE
            )
            .into());
        }

        let body = response
            .bytes()
            .map_err(|e| format!("Failed to read response body: {}", e))?;

        let segments = segment_wire::decode_segments(&body)
            .map_err(|e| format!("Failed to decode segments: {}", e))?;

        if segments.is_empty() {
            if cutoff_ts.is_some() {
                // With cutoff filtering, empty segments is valid (all filtered out)
                // Just advance to next chunk
                current_id = end_id + 1;
                continue;
            }
            return Err(format!("No segments returned for range {}-{}", current_id, end_id).into());
        }

        // Insert segments into local database (all operations in one transaction)
        let last_id = segments.last().unwrap().id;
        let segments_ref = &segments;

        db.block_on(async {
            let mut tx = db
                .pool()
                .begin()
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            for segment in segments_ref {
                let sql = segments::insert_with_id_pg(
                    segment.id,
                    segment.timestamp_ms,
                    segment.is_timestamp_from_source,
                    &segment.audio_data,
                    segment.section_id,
                    segment.duration_samples,
                );
                sqlx::query(&sql)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            }

            // Update last_synced_id (in same transaction)
            let sql = metadata::update_pg("last_synced_id", &last_id.to_string());
            sqlx::query(&sql)
                .execute(&mut *tx)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            tx.commit()
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(())
        })
        .map_err(|e| format!("Database transaction error: {}", e))?;

        println!(
            "[{}]   Synced segments {} to {} ({}/{} segments, {:.1}% complete)",
            show_name,
            current_id,
            last_id,
            last_id - start_id + 1,
            target_max_id - start_id + 1,
            ((last_id - start_id + 1) as f64 / (target_max_id - start_id + 1) as f64) * 100.0
        );

        current_id = last_id + 1;
    }

    println!(
        "[{}]   ✓ Sync complete: {} segments",
        show_name,
        target_max_id - start_id + 1
    );
    Ok(())
}

/// Validate that local metadata matches remote metadata
fn validate_metadata(
    db: &SyncDbPg,
    remote: &ShowMetadata,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Validate version
    let local_version: String = crate::db_postgres::query_metadata_pg_sync(db, "version")?
        .ok_or_else(|| "Failed to read version from local database: key not found")?;
    if local_version != remote.version {
        return Err(format!(
            "Version mismatch: local='{}', remote='{}'. Cannot sync between different schema versions.",
            local_version, remote.version
        )
        .into());
    }

    // Validate audio_format
    let local_format: String = crate::db_postgres::query_metadata_pg_sync(db, "audio_format")?
        .ok_or_else(|| "Failed to read audio_format from local database: key not found")?;
    if local_format != remote.audio_format {
        return Err(format!(
            "Audio format mismatch: local='{}', remote='{}'",
            local_format, remote.audio_format
        )
        .into());
    }

    // Validate bitrate
    let local_bitrate: String = crate::db_postgres::query_metadata_pg_sync(db, "bitrate")?
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
