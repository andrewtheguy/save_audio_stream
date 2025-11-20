use chrono::{DateTime, Timelike, Utc};
use crossbeam_channel::{bounded, Receiver, Sender};
use fdk_aac::enc::{
    AudioObjectType, BitRate as AacBitRate, ChannelMode, Encoder as AacEncoder, EncoderParams,
    Transport,
};
use hound::{WavSpec, WavWriter};
use ogg::writing::PacketWriter;
use opus::{Application, Bitrate as OpusBitrate, Channels, Encoder as OpusEncoder};
use reqwest::blocking::Client;
use rusqlite::Connection;
use fs2::FileExt;
use log::debug;
use rand::Rng;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use crate::audio::{create_opus_comment_header, create_opus_id_header, resample};
use crate::config::{AudioFormat, SessionConfig, StorageFormat};
use crate::schedule::{
    is_in_active_window, parse_time, seconds_until_end, seconds_until_start, time_to_minutes,
};
use crate::streaming::StreamingSource;

// Retention period for recorded segments (in hours)
// For testing: set to 1 (1 hour), 24 (1 day), or 168 (1 week)
const RETENTION_HOURS: i64 = 168; // ~1 week

/// Calculate backoff delay based on elapsed failure duration
fn get_backoff_ms(elapsed_secs: u64) -> u64 {
    match elapsed_secs {
        0..=29 => 500,     // 0.5s
        30..=59 => 1000,   // 1s
        60..=119 => 2000,  // 2s
        120..=179 => 4000, // 4s
        _ => 5000,         // 5s
    }
}

/// Clean up old segments from database, keeping data starting from a natural boundary
///
/// For testing, pass a specific retention_hours value and optionally a fixed reference_time.
pub fn cleanup_old_segments_with_retention(
    conn: &Connection,
    retention_hours: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    cleanup_old_segments_with_params(conn, retention_hours, None)
}

/// Clean up old segments with explicit reference time (for testing)
pub fn cleanup_old_segments_with_params(
    conn: &Connection,
    retention_hours: i64,
    reference_time: Option<DateTime<Utc>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Calculate cutoff timestamp (reference_time or current time - retention_hours)
    let now = reference_time.unwrap_or_else(|| Utc::now());
    let cutoff = now - chrono::Duration::try_hours(retention_hours).expect("Valid hours");
    let cutoff_ms = cutoff.timestamp_millis();

    println!(
        "Checking for segments older than {} hours (cutoff: {})",
        retention_hours,
        cutoff.format("%Y-%m-%d %H:%M:%S UTC")
    );

    // Find the last boundary segment BEFORE the cutoff
    // This ensures we keep complete sessions and don't break playback continuity
    let last_keeper_boundary: Option<i64> = conn
        .query_row(
            "SELECT MAX(id) FROM segments
             WHERE is_timestamp_from_source = 1
             AND timestamp_ms < ?1",
            [cutoff_ms],
            |row| row.get(0),
        )
        .ok();

    // If we found a boundary to keep, delete everything before it
    if let Some(boundary_id) = last_keeper_boundary {
        let deleted = conn.execute("DELETE FROM segments WHERE id < ?1", [boundary_id])?;

        if deleted > 0 {
            println!(
                "Cleaned up {} old segments (keeping boundary at id={})",
                deleted, boundary_id
            );
        } else {
            println!("No old segments to clean up");
        }
    } else {
        println!("No old segments to clean up (no boundaries found before cutoff)");
    }

    Ok(())
}

/// Clean up old segments using the default RETENTION_HOURS constant
fn cleanup_old_segments(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    cleanup_old_segments_with_retention(conn, RETENTION_HOURS)
}

pub fn record(config: SessionConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Setup per-session file logging
    let output_dir = config.output_dir.clone().unwrap_or_else(|| "tmp".to_string());
    let log_path = format!("{}/{}.log", output_dir, config.name);

    // Create output directory for log file
    std::fs::create_dir_all(&output_dir).ok();

    // Setup file logger for this session
    let _log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to open log file '{}': {}", log_path, e))?;

    // Note: Separate log files are created per session
    // TODO: Implement proper file-based logging redirection for this thread
    println!("Session '{}' logging to: {}", config.name, log_path);

    // Extract config values with defaults
    let url = config.url.clone();
    let audio_format = config.audio_format.unwrap_or(AudioFormat::Opus);
    let storage_format = config.storage_format.unwrap_or(StorageFormat::Sqlite);
    let bitrate = config.bitrate;
    let name = config.name.clone();
    let output_dir = config.output_dir.clone().unwrap_or_else(|| "tmp".to_string());
    let split_interval = config.split_interval.unwrap_or(0);

    // Acquire exclusive lock to prevent multiple instances
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory '{}': {}", output_dir, e))?;
    let lock_path = format!("{}/{}.lock", output_dir, name);
    let _lock_file = File::create(&lock_path)
        .map_err(|e| format!("Failed to create lock file '{}': {}", lock_path, e))?;
    _lock_file.try_lock_exclusive().map_err(|_| {
        format!(
            "Another instance is already recording '{}'. Lock file: {}",
            name, lock_path
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
            let wait_secs = seconds_until_start(current_mins, start_mins);
            println!(
                "Current time is outside recording window ({} to {} UTC)",
                config.schedule.record_start, config.schedule.record_end
            );
            println!(
                "Waiting {} seconds ({:.1} hours) until {} UTC...",
                wait_secs,
                wait_secs as f64 / 3600.0,
                config.schedule.record_start
            );
            std::thread::sleep(std::time::Duration::from_secs(wait_secs));

            // Recalculate current time after waiting
            let now = chrono::Utc::now();
            let current_hour = now.hour();
            let current_min = now.minute();
            let current_mins = time_to_minutes(current_hour, current_min);
            seconds_until_end(current_mins, end_mins)
        } else {
            seconds_until_end(current_mins, end_mins)
        };

    println!("Connecting to: {}", url);
    println!(
        "Recording until {} UTC ({} seconds)",
        config.schedule.record_end, duration
    );

    // Retry configuration
    const MAX_RETRY_DURATION: Duration = Duration::from_secs(5 * 60); // 5 minutes
    let mut retry_start: Option<Instant> = None;

    // SQLite connection - persists across retries within the same recording day
    let mut sqlite_conn: Option<Connection> = None;

    // Create HTTP client with connection timeout
    let client = Client::builder()
        .timeout(None) // No overall timeout for streaming
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
        .build()?;

    // Main connection retry loop - each connection is a fresh recording
    'connection: loop {
        let response = match client.get(&url).send() {
            Ok(resp) => {
                retry_start = None; // Reset on success
                resp
            }
            Err(e) => {
                eprintln!("Connection error: {}", e);
                if let Some(start) = retry_start {
                    if start.elapsed() > MAX_RETRY_DURATION {
                        return Err(format!("Max retry duration exceeded. Last error: {}", e).into());
                    }
                } else {
                    retry_start = Some(Instant::now());
                }
                let backoff_ms = get_backoff_ms(retry_start.unwrap().elapsed().as_secs());
                println!("Retrying in {}ms...", backoff_ms);
                thread::sleep(Duration::from_millis(backoff_ms));
                continue 'connection;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("HTTP error: {}", status);
            if let Some(start) = retry_start {
                if start.elapsed() > MAX_RETRY_DURATION {
                    return Err(format!("Max retry duration exceeded. HTTP error: {}", status).into());
                }
            } else {
                retry_start = Some(Instant::now());
            }
            let backoff_ms = get_backoff_ms(retry_start.unwrap().elapsed().as_secs());
            println!("Retrying in {}ms...", backoff_ms);
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

    // Generate output directory path: output_dir/name/yyyy/mm/dd
    let output_path = format!(
        "{}/{}/{}/{}/{}",
        output_dir,
        name,
        timestamp.format("%Y"),
        timestamp.format("%m"),
        timestamp.format("%d")
    );

    // Create the full output directory structure (only for file storage)
    if storage_format == StorageFormat::File {
        std::fs::create_dir_all(&output_path)
            .map_err(|e| format!("Failed to create output directory '{}': {}", output_path, e))?;
    }

    // Generate output filename (with optional segment number for splitting)
    let generate_filename = |segment: Option<u32>| -> String {
        let ext = match audio_format {
            AudioFormat::Aac => "aac",
            AudioFormat::Opus => "opus",
            AudioFormat::Wav => "wav",
        };
        let ts = timestamp.format("%Y%m%d_%H%M%S");
        match segment {
            Some(n) => format!("{}/{}_{}_{:03}.{}", output_path, name, ts, n, ext),
            None => format!("{}/{}_{}.{}", output_path, name, ts, ext),
        }
    };

    let output_filename = if split_interval > 0 {
        generate_filename(Some(0))
    } else {
        generate_filename(None)
    };

    println!("Content-Type: {}", content_type);
    println!("Storage format: {:?}", storage_format);
    if storage_format == StorageFormat::File {
        println!("Output file: {}", output_filename);
    }
    println!("Output audio format: {:?}", audio_format);
    if split_interval > 0 {
        println!("Split interval: {} seconds", split_interval);
    }

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

    println!("Detected source codec: {}", codec_hint);

    // Create channel for streaming data
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(100);
    let total_bytes = Arc::new(AtomicU64::new(0));
    let total_bytes_clone = Arc::clone(&total_bytes);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = Arc::clone(&stop_flag);

    // Spawn download thread
    let download_handle = thread::spawn(move || {
        let start_time = Instant::now();
        let mut reader = response;
        let mut chunk = [0u8; 8192];
        let mut bytes_downloaded = 0u64;

        println!("Downloading audio data...");

        while !stop_flag_clone.load(Ordering::Relaxed) {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    println!("Stream ended");
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
                    eprintln!("Read error: {}", e);
                    break;
                }
            }
        }

        println!(
            "Download complete: {} bytes in {:.1} seconds",
            bytes_downloaded,
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
    println!("Probing audio format...");
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

    println!(
        "Source audio format: {} Hz, {} channels",
        src_sample_rate, src_channels
    );

    // Format-specific setup
    let (output_sample_rate, frame_size, default_bitrate) = match audio_format {
        AudioFormat::Aac => (16000u32, 1024usize, 32u32),
        AudioFormat::Opus => (48000u32, 960usize, 16u32),
        AudioFormat::Wav => (src_sample_rate, 1024usize, 0u32),
    };
    let bitrate_kbps = bitrate.unwrap_or(default_bitrate);
    let bitrate = bitrate_kbps as i32 * 1000;

    match audio_format {
        AudioFormat::Wav => println!("Output: {} Hz, mono, lossless WAV", output_sample_rate),
        _ => println!(
            "Output: {} Hz, mono, {} kbps {:?}",
            output_sample_rate, bitrate_kbps, audio_format
        ),
    }

    // Helper to create AAC encoder
    // ⚠️ EXPERIMENTAL: AAC encoding has known limitations:
    // - The fdk-aac library binding may have stability issues
    // - AAC has inherent encoder priming delay that affects gapless playback
    // - May be replaced with FFmpeg-based encoding in the future for better stability
    // - Recommendation: Use Opus for production workloads
    let create_aac_encoder = || -> Result<AacEncoder, Box<dyn std::error::Error>> {
        let params = EncoderParams {
            bit_rate: AacBitRate::Cbr(bitrate as u32),
            sample_rate: 16000,
            channels: ChannelMode::Mono,
            transport: Transport::Adts,
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        };
        AacEncoder::new(params).map_err(|e| format!("Failed to create AAC encoder: {:?}", e).into())
    };

    // Helper to create Opus encoder
    let create_opus_encoder = || -> Result<OpusEncoder, Box<dyn std::error::Error>> {
        let mut encoder = OpusEncoder::new(48000, Channels::Mono, Application::Voip)
            .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
        encoder
            .set_bitrate(OpusBitrate::Bits(bitrate))
            .map_err(|e| format!("Failed to set bitrate: {}", e))?;
        Ok(encoder)
    };

    // Helper to create Ogg writer with Opus headers
    let create_ogg_writer =
        |filename: &str| -> Result<PacketWriter<File>, Box<dyn std::error::Error>> {
            let file = File::create(filename)?;
            let mut writer = PacketWriter::new(file);

            // Write Opus headers
            let serial = 1;
            let id_header = create_opus_id_header(1, src_sample_rate);
            writer.write_packet(
                id_header,
                serial,
                ogg::writing::PacketWriteEndInfo::EndPage,
                0,
            )?;

            let comment_header = create_opus_comment_header();
            writer.write_packet(
                comment_header,
                serial,
                ogg::writing::PacketWriteEndInfo::EndPage,
                0,
            )?;

            Ok(writer)
        };

    // Helper to create WAV writer
    let create_wav_writer = |filename: &str| -> Result<
        WavWriter<std::io::BufWriter<File>>,
        Box<dyn std::error::Error>,
    > {
        let spec = WavSpec {
            channels: 1,
            sample_rate: output_sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = WavWriter::create(filename, spec)?;
        Ok(writer)
    };

    // Create encoders based on format
    let mut aac_encoder = None;
    let mut opus_encoder = None;
    let mut ogg_writer = None;
    let mut output_file = None;
    let mut wav_writer: Option<WavWriter<std::io::BufWriter<File>>> = None;

    // SQLite storage setup
    let mut segment_buffer: Vec<u8> = Vec::new();
    let base_timestamp_ms = timestamp.timestamp_millis();

    match storage_format {
        StorageFormat::Sqlite => {
            // Create database file
            let db_path = format!("{}/{}.sqlite", output_dir, name);
            let conn = Connection::open(&db_path)?;

            // Enable WAL mode for better concurrent access
            conn.execute_batch("PRAGMA journal_mode=WAL;")?;

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

            // Create indexes for efficient cleanup queries
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_segments_boundary
                 ON segments(is_timestamp_from_source, timestamp_ms)",
                [],
            )?;

            // Check if database already has metadata and validate it matches config
            let audio_format_str = match audio_format {
                AudioFormat::Aac => "aac",
                AudioFormat::Opus => "opus",
                AudioFormat::Wav => "wav",
            };

            let existing_uuid: Option<String> = conn
                .query_row("SELECT value FROM metadata WHERE key = 'uuid'", [], |row| {
                    row.get(0)
                })
                .ok();
            let existing_name: Option<String> = conn
                .query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| {
                    row.get(0)
                })
                .ok();
            let existing_format: Option<String> = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = 'audio_format'",
                    [],
                    |row| row.get(0),
                )
                .ok();
            let existing_interval: Option<String> = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = 'split_interval'",
                    [],
                    |row| row.get(0),
                )
                .ok();
            let existing_bitrate: Option<String> = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = 'bitrate'",
                    [],
                    |row| row.get(0),
                )
                .ok();
            let existing_version: Option<String> = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = 'version'",
                    [],
                    |row| row.get(0),
                )
                .ok();
            let existing_is_recipient: Option<String> = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = 'is_recipient'",
                    [],
                    |row| row.get(0),
                )
                .ok();

            // Check if this is an existing database
            let is_existing_db =
                existing_name.is_some() || existing_format.is_some() || existing_interval.is_some();

            if is_existing_db {
                // Validate version first
                let db_version = existing_version.ok_or("Database is missing version in metadata")?;
                if db_version != "1" {
                    return Err(format!(
                        "Unsupported database version: '{}'. This application only supports version '1'",
                        db_version
                    ).into());
                }

                // Check if this is a recipient database (sync target)
                if let Some(is_recipient) = existing_is_recipient {
                    if is_recipient == "true" {
                        return Err("Cannot record to a recipient database. This database is configured for syncing only.".into());
                    }
                }

                // Existing database must have all required metadata
                let db_uuid = existing_uuid.ok_or("Database is missing uuid in metadata")?;
                let db_name = existing_name.ok_or("Database is missing name in metadata")?;
                let db_format =
                    existing_format.ok_or("Database is missing audio_format in metadata")?;
                let db_interval =
                    existing_interval.ok_or("Database is missing split_interval in metadata")?;
                let db_bitrate =
                    existing_bitrate.ok_or("Database is missing bitrate in metadata")?;

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
                    ).into());
                }
                let db_bitrate_val: u32 = db_bitrate.parse().unwrap_or(0);
                if db_bitrate_val != bitrate_kbps {
                    return Err(format!(
                        "Config mismatch: database has bitrate '{}' kbps but config specifies '{}' kbps",
                        db_bitrate_val, bitrate_kbps
                    ).into());
                }

                println!("SQLite database: {}", db_path);
                println!("Session ID: {}", db_uuid);
            } else {
                // New database - insert metadata with new uuid
                let session_uuid: String = format!("db_{}", rand::thread_rng()
                    .sample_iter(&rand::distributions::Alphanumeric)
                    .take(12)
                    .map(char::from)
                    .collect::<String>());
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('version', '1')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('uuid', ?1)",
                    [&session_uuid],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('name', ?1)",
                    [&name],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('audio_format', ?1)",
                    [audio_format_str],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('split_interval', ?1)",
                    [&split_interval.to_string()],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('bitrate', ?1)",
                    [&bitrate_kbps.to_string()],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('sample_rate', ?1)",
                    [&output_sample_rate.to_string()],
                )?;
                conn.execute(
                    "INSERT INTO metadata (key, value) VALUES ('is_recipient', 'false')",
                    [],
                )?;

                println!("SQLite database: {}", db_path);
                println!("Session ID: {}", session_uuid);
            }

            sqlite_conn = Some(conn);

            // Still need encoders for SQLite storage
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
        }
        StorageFormat::File => match audio_format {
            AudioFormat::Aac => {
                aac_encoder = Some(create_aac_encoder()?);
                output_file = Some(File::create(&output_filename)?);
            }
            AudioFormat::Opus => {
                opus_encoder = Some(create_opus_encoder()?);
                ogg_writer = Some(create_ogg_writer(&output_filename)?);
            }
            AudioFormat::Wav => {
                wav_writer = Some(create_wav_writer(&output_filename)?);
            }
        },
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
    let mut files_written: Vec<String> = vec![output_filename.clone()];

    // Helper to insert segment into SQLite
    let insert_segment = |conn: &Connection,
                          timestamp_ms: i64,
                          is_from_source: bool,
                          data: &[u8]|
     -> Result<(), Box<dyn std::error::Error>> {
        conn.execute(
            "INSERT INTO segments (timestamp_ms, is_timestamp_from_source, audio_data) VALUES (?1, ?2, ?3)",
            rusqlite::params![timestamp_ms, is_from_source as i32, data],
        )?;
        Ok(())
    };

    // Decode and encode in real-time
    println!("Reencoding to {:?}...", audio_format);
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

                                                match storage_format {
                                                    StorageFormat::File => {
                                                        if let Some(ref mut file) = output_file {
                                                            file.write_all(
                                                                &encode_output[..info.output_size],
                                                            )?;
                                                        }
                                                    }
                                                    StorageFormat::Sqlite => {
                                                        segment_buffer.extend_from_slice(
                                                            &encode_output[..info.output_size],
                                                        );
                                                    }
                                                }

                                                if split_interval > 0
                                                    && segment_samples >= split_samples
                                                {
                                                    match storage_format {
                                                        StorageFormat::File => {
                                                            segment_number += 1;
                                                            segment_samples = 0;
                                                            let new_filename = generate_filename(
                                                                Some(segment_number),
                                                            );
                                                            println!(
                                                                "\nStarting new segment: {}",
                                                                new_filename
                                                            );
                                                            files_written
                                                                .push(new_filename.clone());
                                                            output_file =
                                                                Some(File::create(&new_filename)?);
                                                        }
                                                        StorageFormat::Sqlite => {
                                                            if let Some(ref conn) = sqlite_conn {
                                                                let timestamp_ms = base_timestamp_ms
                                                                    + (segment_start_samples
                                                                        as i64
                                                                        * 1000
                                                                        / output_sample_rate
                                                                            as i64);
                                                                insert_segment(
                                                                    conn,
                                                                    timestamp_ms,
                                                                    segment_number == 0,
                                                                    &segment_buffer,
                                                                )?;
                                                                debug!("Inserted segment {} ({} bytes)", segment_number, segment_buffer.len());
                                                            }
                                                            segment_buffer.clear();
                                                            segment_number += 1;
                                                            segment_start_samples =
                                                                total_output_samples;
                                                            segment_samples = 0;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("AAC encode error: {:?}", e);
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

                                                match storage_format {
                                                    StorageFormat::File => {
                                                        if let Some(ref mut writer) = ogg_writer {
                                                            let is_end_of_segment = split_interval
                                                                > 0
                                                                && segment_samples >= split_samples;
                                                            let end_info = if is_end_of_segment {
                                                                ogg::writing::PacketWriteEndInfo::EndStream
                                                            } else {
                                                                ogg::writing::PacketWriteEndInfo::NormalPacket
                                                            };
                                                            writer.write_packet(
                                                                encode_output[..len].to_vec(),
                                                                1,
                                                                end_info,
                                                                total_output_samples,
                                                            )?;
                                                        }
                                                    }
                                                    StorageFormat::Sqlite => {
                                                        segment_buffer.extend_from_slice(
                                                            &(len as u16).to_le_bytes(),
                                                        );
                                                        segment_buffer.extend_from_slice(
                                                            &encode_output[..len],
                                                        );
                                                    }
                                                }

                                                if split_interval > 0
                                                    && segment_samples >= split_samples
                                                {
                                                    match storage_format {
                                                        StorageFormat::File => {
                                                            segment_number += 1;
                                                            segment_samples = 0;
                                                            let new_filename = generate_filename(
                                                                Some(segment_number),
                                                            );
                                                            println!(
                                                                "\nStarting new segment: {}",
                                                                new_filename
                                                            );
                                                            files_written
                                                                .push(new_filename.clone());
                                                            ogg_writer = Some(create_ogg_writer(
                                                                &new_filename,
                                                            )?);
                                                        }
                                                        StorageFormat::Sqlite => {
                                                            if let Some(ref conn) = sqlite_conn {
                                                                let timestamp_ms = base_timestamp_ms
                                                                    + (segment_start_samples
                                                                        as i64
                                                                        * 1000
                                                                        / output_sample_rate
                                                                            as i64);
                                                                insert_segment(
                                                                    conn,
                                                                    timestamp_ms,
                                                                    segment_number == 0,
                                                                    &segment_buffer,
                                                                )?;
                                                                debug!("Inserted segment {} ({} bytes)", segment_number, segment_buffer.len());
                                                            }
                                                            segment_buffer.clear();
                                                            segment_number += 1;
                                                            segment_start_samples =
                                                                total_output_samples;
                                                            segment_samples = 0;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("Opus encode error: {:?}", e);
                                            }
                                        }
                                    }
                                }
                                AudioFormat::Wav => {
                                    total_output_samples += frame_size as u64;
                                    segment_samples += frame_size as u64;

                                    match storage_format {
                                        StorageFormat::File => {
                                            if let Some(ref mut writer) = wav_writer {
                                                for sample in &frame {
                                                    writer.write_sample(*sample)?;
                                                }
                                            }
                                        }
                                        StorageFormat::Sqlite => {
                                            for sample in &frame {
                                                segment_buffer
                                                    .extend_from_slice(&sample.to_le_bytes());
                                            }
                                        }
                                    }

                                    if split_interval > 0 && segment_samples >= split_samples {
                                        match storage_format {
                                            StorageFormat::File => {
                                                if let Some(writer) = wav_writer.take() {
                                                    writer.finalize()?;
                                                }
                                                segment_number += 1;
                                                segment_samples = 0;
                                                let new_filename =
                                                    generate_filename(Some(segment_number));
                                                println!(
                                                    "\nStarting new segment: {}",
                                                    new_filename
                                                );
                                                files_written.push(new_filename.clone());
                                                wav_writer =
                                                    Some(create_wav_writer(&new_filename)?);
                                            }
                                            StorageFormat::Sqlite => {
                                                if let Some(ref conn) = sqlite_conn {
                                                    let timestamp_ms = base_timestamp_ms
                                                        + (segment_start_samples as i64 * 1000
                                                            / output_sample_rate as i64);
                                                    insert_segment(
                                                        conn,
                                                        timestamp_ms,
                                                        segment_number == 0,
                                                        &segment_buffer,
                                                    )?;
                                                    debug!(
                                                        "Inserted segment {} ({} bytes)",
                                                        segment_number,
                                                        segment_buffer.len()
                                                    );
                                                }
                                                segment_buffer.clear();
                                                segment_number += 1;
                                                segment_start_samples = total_output_samples;
                                                segment_samples = 0;
                                            }
                                        }
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
                        eprintln!("\nDecode error: {}", e);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("\nFatal decode error: {}", e);
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
                eprintln!("\nFormat error: {}", e);
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
                        match storage_format {
                            StorageFormat::File => {
                                if let Some(ref mut file) = output_file {
                                    file.write_all(&encode_output[..info.output_size])?;
                                }
                            }
                            StorageFormat::Sqlite => {
                                segment_buffer
                                    .extend_from_slice(&encode_output[..info.output_size]);
                            }
                        }
                    }
                }
            }
            AudioFormat::Opus => {
                mono_buffer.resize(frame_size, 0);
                if let Some(ref mut encoder) = opus_encoder {
                    if let Ok(len) = encoder.encode(&mono_buffer, &mut encode_output) {
                        total_output_samples += frame_size as u64;
                        match storage_format {
                            StorageFormat::File => {
                                if let Some(ref mut writer) = ogg_writer {
                                    writer.write_packet(
                                        encode_output[..len].to_vec(),
                                        1,
                                        ogg::writing::PacketWriteEndInfo::EndStream,
                                        total_output_samples,
                                    )?;
                                }
                            }
                            StorageFormat::Sqlite => {
                                segment_buffer.extend_from_slice(&(len as u16).to_le_bytes());
                                segment_buffer.extend_from_slice(&encode_output[..len]);
                            }
                        }
                    }
                }
            }
            AudioFormat::Wav => {
                match storage_format {
                    StorageFormat::File => {
                        if let Some(ref mut writer) = wav_writer {
                            for sample in &mono_buffer {
                                writer.write_sample(*sample)?;
                            }
                            total_output_samples += mono_buffer.len() as u64;
                        }
                    }
                    StorageFormat::Sqlite => {
                        for sample in &mono_buffer {
                            segment_buffer.extend_from_slice(&sample.to_le_bytes());
                        }
                        total_output_samples += mono_buffer.len() as u64;
                    }
                }
            }
        }
    }

    // Finalize storage
    match storage_format {
        StorageFormat::File => {
            if let Some(writer) = wav_writer.take() {
                writer.finalize()?;
            }
        }
        StorageFormat::Sqlite => {
            if !segment_buffer.is_empty() {
                if let Some(ref conn) = sqlite_conn {
                    let timestamp_ms = base_timestamp_ms
                        + (segment_start_samples as i64 * 1000 / output_sample_rate as i64);
                    insert_segment(conn, timestamp_ms, segment_number == 0, &segment_buffer)?;
                    println!(
                        "\nInserted final segment {} ({} bytes)",
                        segment_number,
                        segment_buffer.len()
                    );
                }
            }
        }
    }

    println!();

    let bytes_downloaded = download_handle.join().expect("Download thread panicked");

    if total_input_samples == 0 {
        return Err("No audio samples decoded".into());
    }

    let duration_secs = total_input_samples as f64 / (src_sample_rate as f64 * src_channels as f64);
    println!(
        "Decoded {} samples from {} packets",
        total_input_samples, packets_decoded
    );

    match storage_format {
        StorageFormat::File => {
            if files_written.len() > 1 {
                println!(
                    "Successfully saved {} files ({:.1} seconds of audio, {} bytes downloaded):",
                    files_written.len(),
                    duration_secs,
                    bytes_downloaded
                );
                for file in &files_written {
                    println!("  - {}", file);
                }
            } else {
                println!(
                    "Successfully saved {} ({:.1} seconds of audio, {} bytes downloaded)",
                    output_filename, duration_secs, bytes_downloaded
                );
            }
        }
        StorageFormat::Sqlite => {
            let db_path = format!("{}/{}.sqlite", output_dir, name);
            let total_segments = segment_number + 1;
            println!(
                "Successfully saved {} segments to {} ({:.1} seconds of audio, {} bytes downloaded)",
                total_segments, db_path, duration_secs, bytes_downloaded
            );
        }
    }

        // Check if target was reached
        if total_output_samples >= target_samples {
            // Target reached, recording complete
            break;
        }

        // Stream ended early - check if we should retry
        eprintln!("\nStream ended before target duration reached ({} / {} samples)",
                  total_output_samples, target_samples);

        if let Some(start) = retry_start {
            if start.elapsed() > MAX_RETRY_DURATION {
                return Err(format!("Max retry duration exceeded. Only recorded {} of {} samples",
                                   total_output_samples, target_samples).into());
            }
        } else {
            retry_start = Some(Instant::now());
        }

        let backoff_ms = get_backoff_ms(retry_start.unwrap().elapsed().as_secs());
        println!("Retrying connection in {}ms...", backoff_ms);
        thread::sleep(Duration::from_millis(backoff_ms));
        // Continue to next connection attempt
    } // End of 'connection loop

        // Loop for next day's recording window
        let (start_hour, start_min) = parse_time(&config.schedule.record_start)?;
        let start_mins = time_to_minutes(start_hour, start_min);

        let now = chrono::Utc::now();
        let current_mins = time_to_minutes(now.hour(), now.minute());
        let wait_secs = seconds_until_start(current_mins, start_mins);

        println!("\nRecording window complete. Next window starts at {} UTC.", config.schedule.record_start);

        // Run cleanup of old segments for SQLite storage
        if storage_format == StorageFormat::Sqlite {
            if let Some(ref conn) = sqlite_conn {
                if let Err(e) = cleanup_old_segments(conn) {
                    eprintln!("Warning: Failed to clean up old segments: {}", e);
                }
            }
        }

        println!("Waiting {} seconds ({:.1} hours)...", wait_secs, wait_secs as f64 / 3600.0);

        std::thread::sleep(std::time::Duration::from_secs(wait_secs));
        // Continue to next day's recording
    } // End of daily loop - runs indefinitely
}
