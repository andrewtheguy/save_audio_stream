use crate::audio::resample;
use crate::config::{AudioFormat, SessionConfig};
use crate::constants::EXPECTED_DB_VERSION;
use crate::db;
use crate::schedule::{
    is_in_active_window, parse_time, seconds_until_end, seconds_until_start, time_to_minutes,
};
use crate::streaming::StreamingSource;

// Import ShowLocks and get_show_lock from the crate root
use crate::{ShowLocks, get_show_lock};
use std::path::{Path, PathBuf};
use chrono::{DateTime, Timelike, Utc};
use crossbeam_channel::{bounded, Receiver, Sender};
use fdk_aac::enc::{
    AudioObjectType, BitRate as AacBitRate, ChannelMode, Encoder as AacEncoder, EncoderParams,
    Transport,
};
use fs2::FileExt;
use log::debug;
use opus::{Application, Bitrate as OpusBitrate, Channels, Encoder as OpusEncoder};
use reqwest::blocking::Client;
use sqlx::sqlite::SqlitePool;
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

// Retention period for recorded sections (in hours)
// For testing: set to 1 (1 hour), 24 (1 day), or 168 (1 week)
const RETENTION_HOURS: i64 = 168; // ~1 week

/// Calculate backoff delay based on elapsed failure duration
fn get_backoff_ms(elapsed_secs: u64) -> u64 {
    match elapsed_secs {
        0..=29 => 500,     // 0.5s
        30..=59 => 1000,   // 1s
        60..=119 => 2000,  // 2s
        120..=179 => 5000, // 5s
        _ => 10000,        // 10s
    }
}

/// Test if a stream URL can be successfully decoded
/// This connects to the stream, reads a small amount of data synchronously,
/// and verifies it can be decoded. Runs fully synchronously without spawning threads.
/// Returns Ok if the stream is decodable, Err otherwise
fn test_url_decode(url: &str, name: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("[{}] Testing stream URL: {}", name, url);

    // Create HTTP client with connection timeout
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
        .build()?;

    // Connect to the stream
    let mut response = client
        .get(url)
        .send()
        .map_err(|e| format!("[{}] Failed to connect: {}", name, e))?;

    if !response.status().is_success() {
        return Err(format!("[{}] HTTP error: {}", name, response.status()).into());
    }

    // Extract content type
    let content_type = response
        .headers()
        .get("content-type")
        .ok_or(format!("[{}] Missing Content-Type header", name))?
        .to_str()
        .map_err(|_| format!("[{}] Invalid Content-Type header encoding", name))?
        .to_string();

    // Determine codec from content type
    let codec_hint = match content_type.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/aac" | "audio/aacp" | "audio/x-aac" => "aac",
        _ => {
            return Err(format!(
                "[{}] Unsupported Content-Type: '{}'. Supported types: audio/mpeg, audio/mp3, audio/aac, audio/aacp, audio/x-aac",
                name, content_type
            )
            .into());
        }
    };

    println!("[{}] Content-Type: {} (codec: {})", name, content_type, codec_hint);

    // Read a fixed amount of data for testing (200 KB should be enough for several packets)
    let test_data_size = 200 * 1024; // 200 KB
    let mut buffer = vec![0u8; test_data_size];
    let bytes_read = response
        .read(&mut buffer)
        .map_err(|e| format!("[{}] Failed to read data: {}", name, e))?;

    if bytes_read == 0 {
        return Err(format!("[{}] No data received from stream", name).into());
    }

    buffer.truncate(bytes_read);
    println!("[{}] Downloaded {} bytes for testing", name, bytes_read);

    // Create a cursor from the buffer for synchronous reading
    let cursor = std::io::Cursor::new(buffer);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    // Create a hint to help the format registry guess the format
    let mut hint = Hint::new();
    hint.with_extension(codec_hint);

    // Use the default options for format reader and metadata
    let format_opts = FormatOptions::default();
    let metadata_opts = MetadataOptions::default();

    // Probe the media source
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &format_opts, &metadata_opts)
        .map_err(|e| format!("[{}] Failed to probe audio format: {}", name, e))?;

    let mut format = probed.format;

    // Find the first audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or(format!("[{}] No audio track found", name))?;

    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    // Create a decoder for the track
    let decoder_opts = DecoderOptions::default();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &decoder_opts)
        .map_err(|e| format!("[{}] Failed to create decoder: {}", name, e))?;

    // Get audio parameters
    let src_sample_rate = codec_params
        .sample_rate
        .ok_or(format!("[{}] Unknown sample rate", name))?;
    let src_channels = codec_params
        .channels
        .ok_or(format!("[{}] Unknown channel count", name))?
        .count() as u16;

    println!(
        "[{}] Stream format: {} Hz, {} channels",
        name, src_sample_rate, src_channels
    );

    // Try to decode a few packets to ensure decoding works
    let mut packets_decoded = 0;
    let target_packets = 5; // Decode 5 packets as a test

    for _ in 0..20 {
        // Try up to 20 packets to get 5 successful decodes
        match format.next_packet() {
            Ok(packet) => {
                if packet.track_id() != track_id {
                    continue;
                }

                match decoder.decode(&packet) {
                    Ok(_decoded) => {
                        packets_decoded += 1;
                        if packets_decoded >= target_packets {
                            break;
                        }
                    }
                    Err(symphonia::core::errors::Error::DecodeError(e)) => {
                        eprintln!("[{}] Decode error during test: {}", name, e);
                        continue;
                    }
                    Err(e) => {
                        return Err(format!("[{}] Fatal decode error: {}", name, e).into());
                    }
                }
            }
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                return Err(format!("[{}] Format error during test: {}", name, e).into());
            }
        }
    }

    if packets_decoded == 0 {
        return Err(format!("[{}] Failed to decode any packets", name).into());
    }

    println!(
        "[{}] Successfully decoded {} packets - stream is valid",
        name, packets_decoded
    );

    Ok(())
}

/// Helper to convert query Result to Option, preserving errors other than "no rows"
/// - No row -> Ok(None) (acceptable - key doesn't exist)
/// - Other errors -> Err (corruption, locking, table missing, etc.)
fn query_optional_metadata(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    db::query_metadata_sync(pool, key)
        .map_err(|e| format!("Failed to query metadata key '{}': {}", key, e).into())
}

/// Clean up old sections from database, keeping data starting from a natural boundary
///
/// For testing, pass a specific retention_hours value and optionally a fixed reference_time.
pub fn cleanup_old_sections_with_retention(
    pool: &SqlitePool,
    retention_hours: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    cleanup_old_sections_with_params(pool, retention_hours, None)
}

/// Clean up old sections with explicit reference time need for testing
pub fn cleanup_old_sections_with_params(
    pool: &SqlitePool,
    retention_hours: i64,
    reference_time: Option<DateTime<Utc>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Calculate cutoff timestamp (reference_time or current time - retention_hours)
    let now = reference_time.unwrap_or_else(|| Utc::now());
    let cutoff = now - chrono::Duration::try_hours(retention_hours).expect("Valid hours");
    let cutoff_ms = cutoff.timestamp_millis();

    println!(
        "Checking for sections older than {} hours (cutoff: {})",
        retention_hours,
        cutoff.format("%Y-%m-%d %H:%M:%S UTC")
    );

    // Try to use pending_section_id from metadata as keeper
    // This preserves the currently active recording session
    let pending_keeper: Option<i64> = match db::query_metadata_sync(pool, "pending_section_id") {
        Ok(Some(value)) => {
            match value.parse::<i64>() {
                Ok(pending_id) => {
                    // Verify that this section has segments (not empty)
                    match db::segments_exist_for_section_sync(pool, pending_id) {
                        Ok(has_segments) => {
                            if has_segments {
                                Some(pending_id)
                            } else {
                                None
                            }
                        }
                        Err(e) => {
                            return Err(format!("Failed to check if section {} has segments: {}", pending_id, e).into());
                        }
                    }
                }
                Err(_) => None,
            }
        }
        Ok(None) => None, // Expected - pending_section_id doesn't exist
        Err(e) => {
            return Err(format!("Failed to query pending_section_id metadata: {}", e).into());
        }
    };

    // Use pending_section_id if available, otherwise query for fallback
    let keeper_section_id = if pending_keeper.is_some() {
        pending_keeper
    } else {
        // Fallback: Find the section with the latest start_timestamp_ms BEFORE the cutoff
        // This ensures we keep complete sessions and don't break playback continuity
        db::get_latest_section_before_cutoff_sync(pool, cutoff_ms).ok().flatten()
    };

    // If we found a section to keep, delete all older sections
    // Segments will be automatically deleted via ON DELETE CASCADE
    if let Some(keeper_section_id) = keeper_section_id {
        // Delete sections timestamped before cutoff, except the keeper
        // This preserves the keeper section (whether from pending_section_id or fallback)
        // and all sections with start_timestamp_ms >= cutoff
        let deleted_sections = db::delete_old_sections_sync(pool, cutoff_ms, keeper_section_id)?;

        if deleted_sections > 0 {
            println!(
                "Cleaned up {} sections and their associated segments (keeping section_id={} and newer)",
                deleted_sections, keeper_section_id
            );
        } else {
            println!("No old sections to clean up");
        }
    } else {
        println!("No old sections to clean up (no sections found before cutoff)");
    }

    Ok(())
}

/// Run the connection loop and handle recording with retries
fn run_connection_loop(
    url: &str,
    audio_format: AudioFormat,
    bitrate_kbps: u32,
    name: &str,
    db_path: &Path,
    split_interval: u64,
    duration: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize database once before the connection loop with WAL mode enabled
    let pool = db::open_database_connection_sync(db_path)?;

    // Initialize schema using common helper
    db::init_database_schema_sync(&pool)?;

    // Check if database already has metadata and validate it matches config
    let audio_format_str = match audio_format {
        AudioFormat::Aac => "aac",
        AudioFormat::Opus => "opus",
        AudioFormat::Wav => "wav",
    };

    let existing_unique_id: Option<String> = query_optional_metadata(&pool, "unique_id")?;
    let existing_name: Option<String> = query_optional_metadata(&pool, "name")?;
    let existing_format: Option<String> = query_optional_metadata(&pool, "audio_format")?;
    let existing_interval: Option<String> = query_optional_metadata(&pool, "split_interval")?;
    let existing_bitrate: Option<String> = query_optional_metadata(&pool, "bitrate")?;
    let existing_version: Option<String> = query_optional_metadata(&pool, "version")?;
    let existing_is_recipient: Option<String> = query_optional_metadata(&pool, "is_recipient")?;

    // Check if this is an existing database
    let is_existing_db =
        existing_name.is_some() || existing_format.is_some() || existing_interval.is_some();

    if is_existing_db {
        // Validate version first
        let db_version = existing_version.ok_or("Database is missing version in metadata")?;
        if db_version != EXPECTED_DB_VERSION {
            return Err(format!(
                "Unsupported database version: '{}'. This application only supports version '{}'",
                db_version, EXPECTED_DB_VERSION
            )
            .into());
        }

        // Check if this is a recipient database (sync target)
        if let Some(is_recipient) = existing_is_recipient {
            if is_recipient == "true" {
                return Err("Cannot record to a recipient database. This database is configured for syncing only.".into());
            }
        }

        // Existing database must have all required metadata
        let db_unique_id = existing_unique_id.ok_or("Database is missing unique_id in metadata")?;
        let db_name = existing_name.ok_or("Database is missing name in metadata")?;
        let db_format = existing_format.ok_or("Database is missing audio_format in metadata")?;
        let db_interval =
            existing_interval.ok_or("Database is missing split_interval in metadata")?;
        let db_bitrate = existing_bitrate.ok_or("Database is missing bitrate in metadata")?;

        // Validate metadata matches config
        if db_name != name {
            return Err(format!(
                "Config mismatch: database has name '{}' but config specifies '{}'",
                db_name, name
            )
            .into());
        }
        if db_format != audio_format_str {
            return Err(format!(
                "Config mismatch: database has audio_format '{}' but config specifies '{}'",
                db_format, audio_format_str
            )
            .into());
        }
        let db_interval_val: u64 = db_interval.parse().unwrap_or(0);
        if db_interval_val != split_interval {
            return Err(format!(
                "Config mismatch: database has split_interval '{}' but config specifies '{}'",
                db_interval_val, split_interval
            )
            .into());
        }

        // Determine expected bitrate for validation
        let (_, _, default_bitrate) = match audio_format {
            AudioFormat::Aac => (16000u32, 1024usize, 32u32),
            AudioFormat::Opus => (48000u32, 960usize, 16u32),
            AudioFormat::Wav => (0u32, 1024usize, 0u32),
        };
        let expected_bitrate = if bitrate_kbps == 0 {
            default_bitrate
        } else {
            bitrate_kbps
        };

        let db_bitrate_val: u32 = db_bitrate.parse().unwrap_or(0);
        if db_bitrate_val != expected_bitrate {
            return Err(format!(
                "Config mismatch: database has bitrate '{}' kbps but config specifies '{}' kbps",
                db_bitrate_val, expected_bitrate
            )
            .into());
        }

        // Validate AAC gapless metadata matches encoder
        if matches!(audio_format, AudioFormat::Aac) {
            let db_encoder_delay: Option<String> = db::query_metadata_sync(&pool, "aac_encoder_delay").ok().flatten();
            let db_frame_size: Option<String> = db::query_metadata_sync(&pool, "aac_frame_size").ok().flatten();

            let aac_bitrate = if db_bitrate_val == 0 {
                32000
            } else {
                db_bitrate_val * 1000
            };
            let params = fdk_aac::enc::EncoderParams {
                bit_rate: fdk_aac::enc::BitRate::Cbr(aac_bitrate),
                sample_rate: 16000,
                channels: fdk_aac::enc::ChannelMode::Mono,
                transport: fdk_aac::enc::Transport::Adts,
                audio_object_type: fdk_aac::enc::AudioObjectType::Mpeg4LowComplexity,
            };
            if let Ok(encoder) = fdk_aac::enc::Encoder::new(params) {
                if let Ok(info) = encoder.info() {
                    if let Some(db_delay) = db_encoder_delay {
                        let db_delay_val: u32 = db_delay.parse().unwrap_or(0);
                        if db_delay_val != info.nDelay {
                            return Err(format!(
                                "AAC encoder mismatch: database has encoder_delay '{}' but encoder reports '{}'",
                                db_delay_val, info.nDelay
                            ).into());
                        }
                    }
                    if let Some(db_frame) = db_frame_size {
                        let db_frame_val: u32 = db_frame.parse().unwrap_or(0);
                        if db_frame_val != info.frameLength {
                            return Err(format!(
                                "AAC encoder mismatch: database has frame_size '{}' but encoder reports '{}'",
                                db_frame_val, info.frameLength
                            ).into());
                        }
                    }
                }
            }
        }

        println!("[{}] Session ID existing db: {}", name, db_unique_id);
    } else {
        // Determine bitrate and sample rate for new database
        let (output_sample_rate, _, default_bitrate) = match audio_format {
            AudioFormat::Aac => (16000u32, 1024usize, 32u32),
            AudioFormat::Opus => (48000u32, 960usize, 16u32),
            AudioFormat::Wav => (48000u32, 1024usize, 0u32), // Will be updated with actual source rate
        };
        let bitrate_to_store = if bitrate_kbps == 0 {
            default_bitrate
        } else {
            bitrate_kbps
        };

        // New database - insert metadata with new unique_id
        let session_unique_id: String = crate::constants::generate_db_unique_id();
        db::insert_metadata_sync(&pool, "version", EXPECTED_DB_VERSION)?;
        db::insert_metadata_sync(&pool, "unique_id", &session_unique_id)?;
        db::insert_metadata_sync(&pool, "name", name)?;
        db::insert_metadata_sync(&pool, "audio_format", audio_format_str)?;
        db::insert_metadata_sync(&pool, "split_interval", &split_interval.to_string())?;
        db::insert_metadata_sync(&pool, "bitrate", &bitrate_to_store.to_string())?;
        db::insert_metadata_sync(&pool, "sample_rate", &output_sample_rate.to_string())?;
        db::insert_metadata_sync(&pool, "is_recipient", "false")?;

        // Add AAC gapless metadata from encoder info
        if matches!(audio_format, AudioFormat::Aac) {
            let aac_bitrate = if bitrate_to_store == 0 {
                32000
            } else {
                bitrate_to_store * 1000
            };
            let params = fdk_aac::enc::EncoderParams {
                bit_rate: fdk_aac::enc::BitRate::Cbr(aac_bitrate),
                sample_rate: 16000,
                channels: fdk_aac::enc::ChannelMode::Mono,
                transport: fdk_aac::enc::Transport::Adts,
                audio_object_type: fdk_aac::enc::AudioObjectType::Mpeg4LowComplexity,
            };
            if let Ok(encoder) = fdk_aac::enc::Encoder::new(params) {
                if let Ok(info) = encoder.info() {
                    db::insert_metadata_sync(&pool, "aac_encoder_delay", &info.nDelay.to_string())?;
                    db::insert_metadata_sync(&pool, "aac_frame_size", &info.frameLength.to_string())?;
                }
            }
        }

        println!("[{}] Session ID new db: {}", name, session_unique_id);
    }

    // Calculate absolute end time based on schedule
    let recording_start_time = Instant::now();
    let recording_end_time = recording_start_time + Duration::from_secs(duration);
    let mut retry_start: Option<Instant> = None;

    // Create HTTP client with connection timeout
    let client = Client::builder()
        .timeout(None) // No overall timeout for streaming
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
        .build()?;

    // Main connection retry loop - each connection is a fresh recording
    'connection: loop {
        // Check if we've reached the schedule end time
        if Instant::now() >= recording_end_time {
            println!("[{}] Recording schedule end time reached", name);
            break 'connection;
        }

        let response = match client.get(url).send() {
            Ok(resp) => {
                retry_start = None; // Reset on success
                resp
            }
            Err(e) => {
                eprintln!("[{}] Connection error: {}", name, e);

                // Check if we've reached schedule end
                if Instant::now() >= recording_end_time {
                    println!("[{}] Recording schedule end time reached during retry", name);
                    break 'connection;
                }

                if retry_start.is_none() {
                    retry_start = Some(Instant::now());
                }

                let backoff_ms = get_backoff_ms(retry_start.unwrap().elapsed().as_secs());
                println!("[{}] Retrying in {}ms...", name, backoff_ms);
                thread::sleep(Duration::from_millis(backoff_ms));
                continue 'connection;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("[{}] HTTP error: {}", name, status);

            // Check if we've reached schedule end
            if Instant::now() >= recording_end_time {
                println!("[{}] Recording schedule end time reached during retry", name);
                break 'connection;
            }

            if retry_start.is_none() {
                retry_start = Some(Instant::now());
            }

            let backoff_ms = get_backoff_ms(retry_start.unwrap().elapsed().as_secs());
            println!("[{}] Retrying in {}ms...", name, backoff_ms);
            thread::sleep(Duration::from_millis(backoff_ms));
            continue 'connection;
        }

        // Extract headers
        let content_type = response
            .headers()
            .get("content-type")
            .ok_or("Missing Content-Type header")?
            .to_str()
            .map_err(|_| "Invalid Content-Type header encoding")?
            .to_string();

        let date_header = response
            .headers()
            .get("date")
            .ok_or("Missing Date header")?
            .to_str()
            .map_err(|_| "Invalid Date header encoding")?;

        // Parse date for filename (HTTP Date header is always GMT/UTC per RFC 7231)
        let timestamp: DateTime<Utc> = {
            let system_time = httpdate::parse_http_date(date_header)
                .map_err(|_| format!("Failed to parse Date header: {}", date_header))?;
            // Convert to DateTime<Utc> to ensure UTC timezone
            system_time.into()
        };

        // Determine codec from content type
        let codec_hint = match content_type.as_str() {
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/aac" | "audio/aacp" | "audio/x-aac" => "aac",
            _ => {
                return Err(format!(
                "Unsupported Content-Type: '{}'. Supported types: audio/mpeg, audio/mp3, audio/aac, audio/aacp, audio/x-aac",
                content_type
            ).into());
            }
        };

        println!(
            "[{}] Source codec: {} (Content-Type: {})",
            name, codec_hint, content_type
        );
        println!("[{}] Target format: {:?}", name, audio_format);
        println!("[{}] Storage: SQLite", name);
        if split_interval > 0 {
            println!("[{}] Split interval: {} seconds", name, split_interval);
        }

        // Create channel for streaming data
        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(100);
        let total_bytes = Arc::new(AtomicU64::new(0));
        let total_bytes_clone = Arc::clone(&total_bytes);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        // Clone name for the download thread
        let name_clone = name.to_string();

        // Spawn download thread
        let download_handle = thread::spawn(move || {
            let start_time = Instant::now();
            let mut reader = response;
            let mut chunk = [0u8; 8192];
            let mut bytes_downloaded = 0u64;

            println!("[{}] Downloading audio data...", name_clone);

            while !stop_flag_clone.load(Ordering::Relaxed) {
                match reader.read(&mut chunk) {
                    Ok(0) => {
                        println!("[{}] Stream ended", name_clone);
                        break;
                    }
                    Ok(n) => {
                        bytes_downloaded += n as u64;
                        total_bytes_clone.store(bytes_downloaded, Ordering::Relaxed);

                        // Send chunk through channel
                        if tx.send(chunk[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("[{}] Read error: {}", name_clone, e);
                        break;
                    }
                }
            }

            println!(
                "[{}] Download complete: {} bytes in {:.1} seconds",
                name_clone, bytes_downloaded,
                start_time.elapsed().as_secs_f64()
            );

            // Signal end of stream
            let _ = tx.send(Vec::new());
            bytes_downloaded
        });

        // Create streaming source for decoder
        let streaming_source = StreamingSource::new(rx, total_bytes);
        let mss = MediaSourceStream::new(Box::new(streaming_source), Default::default());

        // Create a hint to help the format registry guess the format
        let mut hint = Hint::new();
        hint.with_extension(codec_hint);

        // Use the default options for format reader and metadata
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();

        // Probe the media source
        println!("[{}] Probing audio format...", name);
        let probed =
            symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

        let mut format = probed.format;

        // Find the first audio track
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or("No audio track found")?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        // Create a decoder for the track
        let decoder_opts = DecoderOptions::default();
        let mut decoder = symphonia::default::get_codecs().make(&codec_params, &decoder_opts)?;

        // Get audio parameters
        let src_sample_rate = codec_params.sample_rate.ok_or("Unknown sample rate")?;
        let src_channels = codec_params
            .channels
            .ok_or("Unknown channel count")?
            .count() as u16;

        println!("[{}] Source: {} Hz, {} channels", name, src_sample_rate, src_channels);

        // Format-specific setup
        let (output_sample_rate, frame_size, default_bitrate) = match audio_format {
            AudioFormat::Aac => (16000u32, 1024usize, 32u32),
            AudioFormat::Opus => (48000u32, 960usize, 16u32),
            AudioFormat::Wav => (src_sample_rate, 1024usize, 0u32),
        };
        let bitrate_kbps_resolved = if bitrate_kbps == 0 {
            default_bitrate
        } else {
            bitrate_kbps
        };
        let bitrate = bitrate_kbps_resolved as i32 * 1000;

        match audio_format {
            AudioFormat::Wav => println!("[{}] Target: {} Hz, mono, lossless WAV", name, output_sample_rate),
            _ => println!(
                "[{}] Target: {} Hz, mono, {} kbps {:?}",
                name, output_sample_rate, bitrate_kbps_resolved, audio_format
            ),
        }

        // Helper to create AAC encoder
        // opus is recommended instead of aac for voip use cases
        let create_aac_encoder = || -> Result<AacEncoder, Box<dyn std::error::Error + Send + Sync>> {
            let params = EncoderParams {
                bit_rate: AacBitRate::Cbr(bitrate as u32),
                sample_rate: 16000,
                channels: ChannelMode::Mono,
                transport: Transport::Adts,
                audio_object_type: AudioObjectType::Mpeg4LowComplexity,
            };
            AacEncoder::new(params)
                .map_err(|e| format!("Failed to create AAC encoder: {:?}", e).into())
        };

        // Helper to create Opus encoder
        let create_opus_encoder = || -> Result<OpusEncoder, Box<dyn std::error::Error + Send + Sync>> {
            let mut encoder = OpusEncoder::new(48000, Channels::Mono, Application::Voip)
                .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
            encoder
                .set_bitrate(OpusBitrate::Bits(bitrate))
                .map_err(|e| format!("Failed to set bitrate: {}", e))?;
            Ok(encoder)
        };

        // Create encoders (only for SQLite storage)
        let mut aac_encoder = None;
        let mut opus_encoder = None;

        // SQLite storage setup
        let mut segment_buffer: Vec<u8> = Vec::new();
        let base_timestamp_ms = timestamp.timestamp_millis();

        // Create encoders for SQLite storage
        match audio_format {
            AudioFormat::Aac => {
                aac_encoder = Some(create_aac_encoder()?);
            }
            AudioFormat::Opus => {
                opus_encoder = Some(create_opus_encoder()?);
            }
            AudioFormat::Wav => {
                // WAV will be stored as raw PCM in the blob
            }
        }

        // Splitting state
        let split_samples = if split_interval > 0 {
            split_interval * output_sample_rate as u64
        } else {
            u64::MAX // No splitting
        };
        let mut segment_number: u32 = 0;
        let mut segment_samples: u64 = 0;
        let mut segment_start_samples: u64 = 0;

        // Create new section_id for this connection (session boundary)
        let connection_section_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        // Insert new section and update pending_section_id metadata
        db::insert_section_sync(&pool, connection_section_id, base_timestamp_ms)?;
        db::upsert_metadata_sync(
            &pool,
            "pending_section_id",
            &connection_section_id.to_string(),
        )?;

        // Helper to insert segment into SQLite
        let insert_segment = |pool: &SqlitePool,
                              timestamp_ms: i64,
                              is_from_source: bool,
                              section_id: i64,
                              data: &[u8],
                              duration_samples: u64|
         -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            db::insert_segment_sync(pool, timestamp_ms, is_from_source, section_id, data, duration_samples as i64)?;
            Ok(())
        };

        // Decode and encode in real-time
        println!("[{}] Reencoding to {:?}...", name, audio_format);
        let mut total_input_samples = 0usize;
        let mut packets_decoded = 0usize;
        let mut total_output_samples: u64 = 0;

        // Buffer for collecting mono samples before encoding
        let mut mono_buffer: Vec<i16> = Vec::new();
        let mut encode_output = vec![0u8; 8192];

        // Target duration in samples at output rate
        let target_samples = duration * output_sample_rate as u64;

        loop {
            // Check if we've reached target duration
            if total_output_samples >= target_samples {
                stop_flag.store(true, Ordering::Relaxed);
                break;
            }

            match format.next_packet() {
                Ok(packet) => {
                    if packet.track_id() != track_id {
                        continue;
                    }

                    match decoder.decode(&packet) {
                        Ok(decoded) => {
                            let spec = *decoded.spec();
                            let duration = decoded.capacity() as u64;

                            let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                            sample_buf.copy_interleaved_ref(decoded);

                            let samples = sample_buf.samples();
                            total_input_samples += samples.len();

                            // Convert to mono
                            let mono_samples: Vec<i16> = if src_channels == 1 {
                                samples.to_vec()
                            } else {
                                samples
                                    .chunks(src_channels as usize)
                                    .map(|chunk| {
                                        let sum: i32 = chunk.iter().map(|&s| s as i32).sum();
                                        (sum / chunk.len() as i32) as i16
                                    })
                                    .collect()
                            };

                            // Resample to output sample rate
                            let resampled =
                                resample(&mono_samples, src_sample_rate, output_sample_rate);
                            mono_buffer.extend_from_slice(&resampled);

                            // Encode complete frames
                            while mono_buffer.len() >= frame_size {
                                let frame: Vec<i16> = mono_buffer.drain(..frame_size).collect();

                                match audio_format {
                                    AudioFormat::Aac => {
                                        if let Some(ref encoder) = aac_encoder {
                                            match encoder.encode(&frame, &mut encode_output) {
                                                Ok(info) => {
                                                    total_output_samples += frame_size as u64;
                                                    segment_samples += frame_size as u64;

                                                    segment_buffer.extend_from_slice(
                                                        &encode_output[..info.output_size],
                                                    );

                                                    if split_interval > 0
                                                        && segment_samples >= split_samples
                                                    {
                                                        let timestamp_ms = base_timestamp_ms
                                                            + (segment_start_samples as i64 * 1000
                                                                / output_sample_rate as i64);
                                                        insert_segment(
                                                            &pool,
                                                            timestamp_ms,
                                                            segment_number == 0,
                                                            connection_section_id,
                                                            &segment_buffer,
                                                            segment_samples,
                                                        )?;
                                                        debug!(
                                                            "Inserted segment {} ({} bytes)",
                                                            segment_number,
                                                            segment_buffer.len()
                                                        );
                                                        segment_buffer.clear();
                                                        segment_number += 1;
                                                        segment_start_samples =
                                                            total_output_samples;
                                                        segment_samples = 0;
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!("[{}] AAC encode error: {:?}", name, e);
                                                }
                                            }
                                        }
                                    }
                                    AudioFormat::Opus => {
                                        if let Some(ref mut encoder) = opus_encoder {
                                            match encoder.encode(&frame, &mut encode_output) {
                                                Ok(len) => {
                                                    total_output_samples += frame_size as u64;
                                                    segment_samples += frame_size as u64;

                                                    segment_buffer.extend_from_slice(
                                                        &(len as u16).to_le_bytes(),
                                                    );
                                                    segment_buffer
                                                        .extend_from_slice(&encode_output[..len]);

                                                    if split_interval > 0
                                                        && segment_samples >= split_samples
                                                    {
                                                        let timestamp_ms = base_timestamp_ms
                                                            + (segment_start_samples as i64 * 1000
                                                                / output_sample_rate as i64);
                                                        insert_segment(
                                                            &pool,
                                                            timestamp_ms,
                                                            segment_number == 0,
                                                            connection_section_id,
                                                            &segment_buffer,
                                                            segment_samples,
                                                        )?;
                                                        debug!(
                                                            "Inserted segment {} ({} bytes)",
                                                            segment_number,
                                                            segment_buffer.len()
                                                        );
                                                        segment_buffer.clear();
                                                        segment_number += 1;
                                                        segment_start_samples =
                                                            total_output_samples;
                                                        segment_samples = 0;
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!("[{}] Opus encode error: {:?}", name, e);
                                                }
                                            }
                                        }
                                    }
                                    AudioFormat::Wav => {
                                        total_output_samples += frame_size as u64;
                                        segment_samples += frame_size as u64;

                                        for sample in &frame {
                                            segment_buffer.extend_from_slice(&sample.to_le_bytes());
                                        }

                                        if split_interval > 0 && segment_samples >= split_samples {
                                            let timestamp_ms = base_timestamp_ms
                                                + (segment_start_samples as i64 * 1000
                                                    / output_sample_rate as i64);
                                            insert_segment(
                                                &pool,
                                                timestamp_ms,
                                                segment_number == 0,
                                                connection_section_id,
                                                &segment_buffer,
                                                segment_samples,
                                            )?;
                                            debug!(
                                                "Inserted segment {} ({} bytes)",
                                                segment_number,
                                                segment_buffer.len()
                                            );
                                            segment_buffer.clear();
                                            segment_number += 1;
                                            segment_start_samples = total_output_samples;
                                            segment_samples = 0;
                                        }
                                    }
                                }
                            }

                            packets_decoded += 1;

                            if packets_decoded % 100 == 0 {
                                let duration_secs = total_input_samples as f64
                                    / (src_sample_rate as f64 * src_channels as f64);
                                debug!("Decoded {:.1}s of audio...", duration_secs);
                            }
                        }
                        Err(symphonia::core::errors::Error::DecodeError(e)) => {
                            eprintln!("[{}] Decode error: {}", name, e);
                            continue;
                        }
                        Err(e) => {
                            eprintln!("[{}] Fatal decode error: {}", name, e);
                            break;
                        }
                    }
                }
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    stop_flag.store(true, Ordering::Relaxed);
                    break;
                }
                Err(e) => {
                    eprintln!("[{}] Format error: {}", name, e);
                    stop_flag.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        // Encode any remaining samples
        if !mono_buffer.is_empty() {
            match audio_format {
                AudioFormat::Aac => {
                    mono_buffer.resize(frame_size, 0);
                    if let Some(ref encoder) = aac_encoder {
                        if let Ok(info) = encoder.encode(&mono_buffer, &mut encode_output) {
                            total_output_samples += frame_size as u64;
                            segment_buffer.extend_from_slice(&encode_output[..info.output_size]);
                        }
                    }
                }
                AudioFormat::Opus => {
                    mono_buffer.resize(frame_size, 0);
                    if let Some(ref mut encoder) = opus_encoder {
                        if let Ok(len) = encoder.encode(&mono_buffer, &mut encode_output) {
                            total_output_samples += frame_size as u64;
                            segment_buffer.extend_from_slice(&(len as u16).to_le_bytes());
                            segment_buffer.extend_from_slice(&encode_output[..len]);
                        }
                    }
                }
                AudioFormat::Wav => {
                    for sample in &mono_buffer {
                        segment_buffer.extend_from_slice(&sample.to_le_bytes());
                    }
                    total_output_samples += mono_buffer.len() as u64;
                }
            }
        }

        // Finalize storage
        if !segment_buffer.is_empty() {
            let timestamp_ms = base_timestamp_ms
                + (segment_start_samples as i64 * 1000 / output_sample_rate as i64);
            insert_segment(
                &pool,
                timestamp_ms,
                segment_number == 0,
                connection_section_id,
                &segment_buffer,
                segment_samples,
            )?;
            println!(
                "[{}] Inserted final segment {} ({} bytes)",
                name, segment_number,
                segment_buffer.len()
            );
        }

        println!();

        let bytes_downloaded = download_handle.join().expect("Download thread panicked");

        if total_input_samples == 0 {
            return Err("No audio samples decoded".into());
        }

        let duration_secs =
            total_input_samples as f64 / (src_sample_rate as f64 * src_channels as f64);
        println!(
            "[{}] Decoded {} samples from {} packets",
            name, total_input_samples, packets_decoded
        );

        let total_segments = segment_number + 1;
        println!(
            "[{}] Successfully saved {} segments ({:.1} seconds of audio, {} bytes downloaded)",
            name, total_segments, duration_secs, bytes_downloaded
        );

        // Check if target was reached
        if total_output_samples >= target_samples {
            // Target reached, recording complete
            break;
        }

        // Stream ended early - check if we should retry
        eprintln!(
            "[{}] Stream ended before target duration reached ({} / {} samples)",
            name, total_output_samples, target_samples
        );

        // Check if we've reached schedule end
        if Instant::now() >= recording_end_time {
            println!("[{}] Recording schedule end time reached", name);
            break 'connection;
        }

        if retry_start.is_none() {
            retry_start = Some(Instant::now());
        }

        let backoff_ms = get_backoff_ms(retry_start.unwrap().elapsed().as_secs());
        println!("[{}] Retrying connection in {}ms...", name, backoff_ms);
        thread::sleep(Duration::from_millis(backoff_ms));
        // Continue to next connection attempt
    } // End of 'connection loop

    Ok(())
}

pub fn record(
    config: SessionConfig,
    show_locks: ShowLocks,
    db_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Extract config values with defaults
    let url = config.url.clone();
    let audio_format = config.audio_format.unwrap_or(AudioFormat::Opus);
    let bitrate = config.bitrate;
    let name = config.name.clone();
    let output_dir = config
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("tmp"));
    let split_interval = config.split_interval.unwrap_or(0);
    let retention_hours = config.retention_hours.unwrap_or(RETENTION_HOURS);

    // Acquire exclusive lock to prevent multiple instances
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory '{}': {}", output_dir.display(), e))?;
    let lock_path = output_dir.join(format!("{}.lock", name));
    let _lock_file = File::create(&lock_path)
        .map_err(|e| format!("Failed to create lock file '{}': {}", lock_path.display(), e))?;
    _lock_file.try_lock_exclusive().map_err(|_| {
        format!(
            "Another instance is already recording '{}'. Lock file: {}",
            name, lock_path.display()
        )
    })?;
    // Lock will be held until lock_file is dropped (end of function)

    // Daily loop for scheduled recording - runs indefinitely
    loop {
        // Parse schedule times
        let (start_hour, start_min) = parse_time(&config.schedule.record_start)?;
        let (end_hour, end_min) = parse_time(&config.schedule.record_end)?;
        let start_mins = time_to_minutes(start_hour, start_min);
        let end_mins = time_to_minutes(end_hour, end_min);

        // Get current UTC time
        let now = chrono::Utc::now();
        let current_hour = now.hour();
        let current_min = now.minute();
        let current_mins = time_to_minutes(current_hour, current_min);

        // Check if we're in the active window
        let duration = if !is_in_active_window(current_mins, start_mins, end_mins) {
            // Wait until start time
            println!(
                "[{}] Current time is outside recording window ({} to {} UTC)",
                config.name, config.schedule.record_start, config.schedule.record_end
            );
            println!(
                "[{}] Waiting until {} UTC...",
                config.name, config.schedule.record_start
            );

            // Loop and sleep 1 second at a time for more accurate timing
            loop {
                let now = chrono::Utc::now();
                let current_mins = time_to_minutes(now.hour(), now.minute());

                // Check if we've entered the active window
                if is_in_active_window(current_mins, start_mins, end_mins) {
                    break;
                }

                std::thread::sleep(std::time::Duration::from_secs(1));
            }

            // Recalculate current time after waiting
            let now = chrono::Utc::now();
            let current_hour = now.hour();
            let current_min = now.minute();
            let current_mins = time_to_minutes(current_hour, current_min);
            seconds_until_end(current_mins, end_mins)
        } else {
            seconds_until_end(current_mins, end_mins)
        };

        println!("[{}] Connecting to: {}", name, url);
        println!(
            "[{}] Recording until {} UTC ({} seconds)",
            name, config.schedule.record_end, duration
        );

        // Run the connection loop and record audio
        run_connection_loop(
            &url,
            audio_format,
            bitrate.unwrap_or(0),
            &name,
            &db_path,
            split_interval,
            duration,
        )?;

        // Loop for next day's recording window
        let (start_hour, start_min) = parse_time(&config.schedule.record_start)?;
        let start_mins = time_to_minutes(start_hour, start_min);

        println!(
            "[{}] Recording window complete. Next window starts at {} UTC.",
            name, config.schedule.record_start
        );

        // Acquire lock before cleanup to prevent concurrent export
        let show_lock = get_show_lock(&show_locks, &name);
        println!("[{}] Acquiring cleanup lock...", name);
        let _cleanup_guard = show_lock.lock().unwrap();  // BLOCKS if export is running
        println!("[{}] Cleanup lock acquired", name);

        // Run cleanup of old sections - recreate connection for cleanup
        if let Ok(cleanup_pool) =
            crate::db::open_database_connection_sync(&db_path)
        {
            if let Err(e) = cleanup_old_sections_with_retention(&cleanup_pool, retention_hours) {
                eprintln!("[{}] Warning: Failed to clean up old sections: {}", name, e);
            }
        }

        // Lock automatically released when _cleanup_guard drops
        drop(_cleanup_guard);
        println!("[{}] Cleanup lock released", name);

        // Loop and sleep 1 second at a time for more accurate timing
        loop {
            let now = chrono::Utc::now();
            let current_mins = time_to_minutes(now.hour(), now.minute());
            let wait_secs = seconds_until_start(current_mins, start_mins);

            if wait_secs <= 0 {
                // We've reached the start time
                break;
            }

            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        // Continue to next day's recording
    } // End of daily loop - runs indefinitely
}

/// Run multi-session recording with API server and supervision
pub fn run_multi_session(
    multi_config: crate::config::MultiSessionConfig,
    port_override: Option<u16>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use dashmap::DashMap;
    use reqwest::blocking::Client;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    // Validate SFTP configuration if enabled
    if let Err(e) = multi_config.validate_sftp() {
        return Err(format!("SFTP configuration error: {}", e).into());
    }

    // Load credentials file if SFTP export is enabled
    let credentials = if multi_config.export_to_sftp.unwrap_or(false) {
        println!("Loading credentials from {}...", crate::credentials::get_credentials_path().display());
        match crate::credentials::load_credentials() {
            Ok(creds) => creds,
            Err(e) => {
                return Err(format!("Failed to load credentials: {}", e).into());
            }
        }
    } else {
        None
    };

    // Determine output directory and API port
    let output_dir_path = multi_config
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("tmp"));
    let api_port = port_override.unwrap_or(multi_config.api_port);

    // Extract SFTP config for API server
    let sftp_config = if multi_config.export_to_sftp.unwrap_or(false) {
        multi_config.sftp.clone()
    } else {
        None
    };

    // begin testing, don't start until all checks pass

    // Test SFTP connection if enabled
    if let Some(ref config) = sftp_config {
        use crate::sftp::{SftpClient, SftpConfig};

        println!("Testing SFTP connection to {}:{}...", config.host, config.port);

        let sftp_test_config = SftpConfig::from_export_config(config, &credentials)
            .map_err(|e| format!("Failed to create SFTP config: {}", e))?;

        let client = SftpClient::connect(&sftp_test_config)
            .map_err(|e| format!("Failed to connect to SFTP server: {}", e))?;

        // Try to write a test file
        let test_filename = format!("test_connection_{}.txt", chrono::Utc::now().timestamp());
        let test_path = std::path::Path::new(&config.remote_dir).join(&test_filename);
        let test_data = b"SFTP connection test";

        println!("Writing test file to {}...", test_path.display());
        let mut cursor = std::io::Cursor::new(test_data);
        let options = crate::sftp::UploadOptions::default();

        client.upload_stream(
            &mut cursor,
            &test_path,
            test_data.len() as u64,
            &options,
        ).map_err(|e| format!("Failed to write test file to SFTP: {}", e))?;

        println!("Successfully wrote test file, cleaning up...");

        // Clean up test file
        if let Err(e) = client.remove_file(&test_path) {
            println!("Warning: Failed to remove test file {}: {}", test_path.display(), e);
        }

        let _ = client.disconnect();
        println!("SFTP connection test: PASSED");
    }

    // Test all stream URLs for decode capability
    println!("Testing stream URLs for decode capability...");
    for session_config in &multi_config.sessions {
        println!("Testing session '{}' URL...", session_config.name);
        test_url_decode(&session_config.url, &session_config.name)
            .map_err(|e| format!("URL decode test failed for session '{}': {}", session_config.name, e))?;
    }
    println!("All stream URLs tested successfully");

    // Extract periodic export flag
    let export_to_remote_periodically = multi_config.export_to_remote_periodically.unwrap_or(false);

    // Extract session names for periodic export
    let session_names: Vec<String> = multi_config.sessions.iter()
        .map(|s| s.name.clone())
        .collect();

    // Create output directory if it doesn't exist
    std::fs::create_dir_all(&output_dir_path)?;
    println!("Output directory: {}", output_dir_path.display());

    // Create shared locks for coordinating export and cleanup operations
    let show_locks: ShowLocks = Arc::new(DashMap::new());
    let locks_for_server = show_locks.clone();
    let locks_for_recording = show_locks.clone();

    // Initialize databases for all sessions BEFORE starting any services
    println!("Initializing databases for {} session(s)...", multi_config.sessions.len());
    let mut db_paths = std::collections::HashMap::new();

    for session_config in &multi_config.sessions {
        let db_path = crate::db::get_db_path(&output_dir_path, &session_config.name);
        println!("Initializing database for session '{}' at {}", session_config.name, db_path.display());

        let pool = crate::db::open_database_connection_sync(&db_path)
            .map_err(|e| format!("Failed to open database for session '{}': {}", session_config.name, e))?;

        crate::db::init_database_schema_sync(&pool)
            .map_err(|e| format!("Failed to initialize schema for session '{}': {}", session_config.name, e))?;

        db_paths.insert(session_config.name.clone(), db_path.clone());
        println!("Database initialized successfully for session '{}'", session_config.name);
    }
    println!("All databases initialized successfully");

    // Clone db_paths for the API server thread
    let db_paths_for_server = db_paths.clone();

    // Start API server first in a separate thread
    println!("Starting API server on port {}", api_port);

    let output_dir_path_for_server = output_dir_path.clone();
    let api_handle = thread::spawn(move || {
        if let Err(e) = crate::serve_record::serve_for_sync(
            output_dir_path_for_server,
            api_port,
            sftp_config,
            export_to_remote_periodically,
            session_names,
            credentials,
            locks_for_server,  // Pass locks to API server
            db_paths_for_server,  // Pass pre-initialized db paths
        ) {
            eprintln!("API server failed: {}", e);
            std::process::exit(1);
        }
    });

    // Give the API server a moment to start up
    println!("Waiting for API server to start...");
    thread::sleep(Duration::from_secs(2));

    // Check if API server thread is still running (didn't panic/exit immediately)
    if api_handle.is_finished() {
        return Err("API server failed to start".into());
    }

    // Perform healthcheck to verify API server is responding
    println!("Performing API server healthcheck...");
    let healthcheck_url = format!("http://localhost:{}/health", api_port);
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let mut healthcheck_passed = false;
    for attempt in 1..=5 {
        match client.get(&healthcheck_url).send() {
            Ok(response) if response.status().is_success() => {
                println!("API server healthcheck passed (attempt {})", attempt);
                healthcheck_passed = true;
                break;
            }
            Ok(response) => {
                eprintln!(
                    "API server healthcheck failed with status {} (attempt {})",
                    response.status(),
                    attempt
                );
            }
            Err(e) => {
                eprintln!("API server healthcheck failed: {} (attempt {})", e, attempt);
            }
        }

        // Check if server thread crashed
        if api_handle.is_finished() {
            return Err("API server thread terminated during healthcheck".into());
        }

        if attempt < 5 {
            thread::sleep(Duration::from_secs(1));
        }
    }

    if !healthcheck_passed {
        return Err("API server healthcheck failed after 5 attempts".into());
    }

    println!("API server is healthy and ready");
    println!(
        "Starting {} recording session(s)",
        multi_config.sessions.len()
    );

    // end testing

    // Now spawn recording session threads (they run in background with supervision)
    let mut recording_handles = Vec::new();
    for (_session_idx, mut session_config) in multi_config.sessions.into_iter().enumerate() {
        // Copy global output_dir to session config
        session_config.output_dir = Some(output_dir_path.clone());

        let session_name = session_config.name.clone();
        let session_name_for_handle = session_name.clone();
        let locks_for_session = locks_for_recording.clone();

        // Get pre-initialized db path for this session
        let db_path = db_paths.get(&session_name)
            .ok_or_else(|| format!("Database path not found for session '{}'", session_name))?
            .clone();

        let handle = thread::spawn(move || {
            // Supervision loop for this session
            loop {
                println!("[{}] Starting recording session", session_name_for_handle);

                match record(session_config.clone(), locks_for_session.clone(), db_path.clone()) {
                    Ok(_) => {
                        // record() runs indefinitely, should never return Ok
                        eprintln!("[{}] Recording ended unexpectedly", session_name_for_handle);
                    }
                    Err(e) => {
                        eprintln!("[{}] Recording failed: {}", session_name_for_handle, e);
                    }
                }

                // Calculate wait time until next scheduled start
                if let Ok((start_hour, start_min)) =
                    crate::schedule::parse_time(&session_config.schedule.record_start)
                {
                    let start_mins = crate::schedule::time_to_minutes(start_hour, start_min);
                    let now = chrono::Utc::now();
                    let current_mins = crate::schedule::time_to_minutes(now.hour(), now.minute());
                    let wait_secs = crate::schedule::seconds_until_start(current_mins, start_mins);

                    println!(
                        "[{}] Restarting at next scheduled time ({} UTC) in {} seconds ({:.1} hours)",
                        session_name_for_handle,
                        session_config.schedule.record_start,
                        wait_secs,
                        wait_secs as f64 / 3600.0
                    );
                    thread::sleep(Duration::from_secs(wait_secs));
                } else {
                    eprintln!(
                        "[{}] Invalid schedule time, waiting 60 seconds before retry",
                        session_name_for_handle
                    );
                    thread::sleep(Duration::from_secs(60));
                }
            }
        });

        recording_handles.push((session_name, handle));
    }

    // Wait for API server thread (blocking) - if it fails, return error
    api_handle
        .join()
        .map_err(|e| format!("API server thread panicked: {:?}", e))?;

    Ok(())
}
