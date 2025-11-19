use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use crossbeam_channel::{bounded, Receiver, Sender};
use hound::{WavSpec, WavWriter};
use rusqlite::Connection;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc as StdArc;
use uuid::Uuid;
use fdk_aac::enc::{
    AudioObjectType, BitRate as AacBitRate, ChannelMode, Encoder as AacEncoder, EncoderParams,
    Transport,
};
use ogg::writing::PacketWriter;
use opus::{Application, Bitrate as OpusBitrate, Channels, Encoder as OpusEncoder};
use reqwest::blocking::Client;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AudioFormat {
    /// AAC-LC format (16kHz mono, 32kbps)
    Aac,
    /// Opus format (48kHz mono, 16kbps)
    Opus,
    /// WAV format (lossless, preserves original sample rate)
    Wav,
}

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StorageFormat {
    /// Save to individual files
    File,
    /// Save to SQLite database
    Sqlite,
}

/// Configuration file structure
#[derive(Debug, Deserialize)]
struct Config {
    /// URL of the Shoutcast/Icecast stream (required)
    url: String,
    /// Duration in seconds to record (default: 30)
    duration: Option<u64>,
    /// Audio format: aac, opus, or wav (default: opus)
    audio_format: Option<AudioFormat>,
    /// Storage format: file or sqlite (default: sqlite)
    storage_format: Option<StorageFormat>,
    /// Bitrate in kbps (default: 32 for AAC, 16 for Opus)
    bitrate: Option<u32>,
    /// Name prefix for output file (required)
    name: String,
    /// Output directory (default: tmp)
    output_dir: Option<String>,
    /// Split interval in seconds (0 = no splitting)
    split_interval: Option<u64>,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Download Shoutcast stream and save as AAC, Opus, or WAV")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Record audio from a stream
    Record {
        /// Path to config file (TOML format)
        #[arg(short, long)]
        config: PathBuf,

        /// Duration in seconds to record (overrides config file)
        #[arg(short, long)]
        duration: Option<u64>,
    },
    /// Serve audio from SQLite database via HTTP
    Serve {
        /// Path to SQLite database file
        sqlite_file: PathBuf,

        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
}

/// A streaming media source that reads from a channel
struct StreamingSource {
    receiver: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    position: usize,
    total_bytes: Arc<AtomicU64>,
    is_finished: bool,
}

impl StreamingSource {
    fn new(receiver: Receiver<Vec<u8>>, total_bytes: Arc<AtomicU64>) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            position: 0,
            total_bytes,
            is_finished: false,
        }
    }

    fn fill_buffer(&mut self) {
        // Try to receive more data without blocking if buffer is getting low
        while self.position >= self.buffer.len() && !self.is_finished {
            match self.receiver.recv() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        self.is_finished = true;
                        break;
                    }
                    // Reset buffer with new chunk
                    self.buffer = chunk;
                    self.position = 0;
                }
                Err(_) => {
                    // Channel closed
                    self.is_finished = true;
                    break;
                }
            }
        }
    }
}

impl Read for StreamingSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.fill_buffer();

        if self.position >= self.buffer.len() {
            return Ok(0); // EOF
        }

        let available = self.buffer.len() - self.position;
        let to_read = std::cmp::min(available, buf.len());
        buf[..to_read].copy_from_slice(&self.buffer[self.position..self.position + to_read]);
        self.position += to_read;

        Ok(to_read)
    }
}

impl Seek for StreamingSource {
    fn seek(&mut self, _pos: SeekFrom) -> std::io::Result<u64> {
        // Streaming source doesn't support seeking
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Seeking not supported for streaming source",
        ))
    }
}

impl MediaSource for StreamingSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        // Return current known length
        Some(self.total_bytes.load(Ordering::Relaxed))
    }
}

/// Create Opus identification header
fn create_opus_id_header(channels: u8, sample_rate: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(19);
    header.extend_from_slice(b"OpusHead");
    header.push(1); // Version
    header.push(channels); // Channel count
    header.extend_from_slice(&0u16.to_le_bytes()); // Pre-skip
    header.extend_from_slice(&sample_rate.to_le_bytes()); // Input sample rate
    header.extend_from_slice(&0i16.to_le_bytes()); // Output gain
    header.push(0); // Channel mapping family
    header
}

/// Create Opus comment header
fn create_opus_comment_header() -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(b"OpusTags");
    let vendor = b"save_audio_stream";
    header.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    header.extend_from_slice(vendor);
    header.extend_from_slice(&0u32.to_le_bytes()); // No user comments
    header
}

/// Resample audio from source sample rate to target rate
fn resample(samples: &[i16], src_rate: u32, target_rate: u32) -> Vec<i16> {
    if src_rate == target_rate {
        return samples.to_vec();
    }

    let ratio = target_rate as f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    let mut resampled = Vec::with_capacity(new_len);

    for i in 0..new_len {
        let src_pos = i as f64 / ratio;
        let src_idx = src_pos as usize;
        let frac = src_pos - src_idx as f64;

        let sample = if src_idx + 1 < samples.len() {
            let s1 = samples[src_idx] as f64;
            let s2 = samples[src_idx + 1] as f64;
            (s1 + frac * (s2 - s1)) as i16
        } else if src_idx < samples.len() {
            samples[src_idx]
        } else {
            0
        };

        resampled.push(sample);
    }

    resampled
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    match args.command {
        Command::Record { config, duration } => record(config, duration),
        Command::Serve { sqlite_file, port } => serve(sqlite_file, port),
    }
}

fn record(config_path: PathBuf, duration_override: Option<u64>) -> Result<(), Box<dyn std::error::Error>> {
    // Load config file (required)
    let config_content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config file '{}': {}", config_path.display(), e))?;
    let config: Config = toml::from_str(&config_content)
        .map_err(|e| format!("Failed to parse config file '{}': {}", config_path.display(), e))?;

    // Extract config values with defaults
    let url = config.url;
    let duration = duration_override.or(config.duration).unwrap_or(30);
    let audio_format = config.audio_format.unwrap_or(AudioFormat::Opus);
    let storage_format = config.storage_format.unwrap_or(StorageFormat::Sqlite);
    let bitrate = config.bitrate;
    let name = config.name;
    let output_dir = config.output_dir.unwrap_or_else(|| "tmp".to_string());
    let split_interval = config.split_interval.unwrap_or(0);

    println!("Connecting to: {}", url);
    println!("Recording duration: {} seconds", duration);

    // Create HTTP client and make request
    let client = Client::builder()
        .timeout(None) // No timeout for streaming
        .build()?;

    let response = client.get(&url).send()?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
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

    // Create the full output directory structure
    std::fs::create_dir_all(&output_path)
        .map_err(|e| format!("Failed to create output directory '{}': {}", output_path, e))?;

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
    println!("Output file: {}", output_filename);
    println!("Audio format: {:?}", audio_format);
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

    println!("Detected codec: {}", codec_hint);

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
        AudioFormat::Wav => (src_sample_rate, 1024usize, 0u32), // WAV uses source rate, frame_size is arbitrary
    };
    let bitrate_kbps = bitrate.unwrap_or(default_bitrate);
    let bitrate = bitrate_kbps as i32 * 1000;

    match audio_format {
        AudioFormat::Wav => println!(
            "Output: {} Hz, mono, lossless WAV",
            output_sample_rate
        ),
        _ => println!(
            "Output: {} Hz, mono, {} kbps {:?}",
            output_sample_rate,
            bitrate_kbps,
            audio_format
        ),
    }

    // Helper to create AAC encoder
    let create_aac_encoder = || -> Result<AacEncoder, Box<dyn std::error::Error>> {
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
    let create_opus_encoder = || -> Result<OpusEncoder, Box<dyn std::error::Error>> {
        let mut encoder = OpusEncoder::new(48000, Channels::Mono, Application::Voip)
            .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
        encoder
            .set_bitrate(OpusBitrate::Bits(bitrate))
            .map_err(|e| format!("Failed to set bitrate: {}", e))?;
        Ok(encoder)
    };

    // Helper to create Ogg writer with Opus headers
    let create_ogg_writer = |filename: &str| -> Result<PacketWriter<File>, Box<dyn std::error::Error>> {
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
    let create_wav_writer = |filename: &str| -> Result<WavWriter<std::io::BufWriter<File>>, Box<dyn std::error::Error>> {
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
    let mut sqlite_conn: Option<Connection> = None;
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
                    audio_data BLOB NOT NULL
                )",
                [],
            )?;

            // Check if database already has metadata and validate it matches config
            let audio_format_str = match audio_format {
                AudioFormat::Aac => "aac",
                AudioFormat::Opus => "opus",
                AudioFormat::Wav => "wav",
            };

            let existing_uuid: Option<String> = conn
                .query_row("SELECT value FROM metadata WHERE key = 'uuid'", [], |row| row.get(0))
                .ok();
            let existing_name: Option<String> = conn
                .query_row("SELECT value FROM metadata WHERE key = 'name'", [], |row| row.get(0))
                .ok();
            let existing_format: Option<String> = conn
                .query_row("SELECT value FROM metadata WHERE key = 'audio_format'", [], |row| row.get(0))
                .ok();
            let existing_interval: Option<String> = conn
                .query_row("SELECT value FROM metadata WHERE key = 'split_interval'", [], |row| row.get(0))
                .ok();

            // Check if this is an existing database
            let is_existing_db = existing_name.is_some() || existing_format.is_some() || existing_interval.is_some();

            if is_existing_db {
                // Existing database must have uuid
                if existing_uuid.is_none() {
                    return Err("Database is missing uuid in metadata".into());
                }

                // Validate metadata matches config
                if let Some(ref db_name) = existing_name {
                    if db_name != &name {
                        return Err(format!(
                            "Config mismatch: database has name '{}' but config specifies '{}'",
                            db_name, name
                        ).into());
                    }
                }
                if let Some(ref db_format) = existing_format {
                    if db_format != audio_format_str {
                        return Err(format!(
                            "Config mismatch: database has audio_format '{}' but config specifies '{}'",
                            db_format, audio_format_str
                        ).into());
                    }
                }
                if let Some(ref db_interval) = existing_interval {
                    let db_interval_val: u64 = db_interval.parse().unwrap_or(0);
                    if db_interval_val != split_interval {
                        return Err(format!(
                            "Config mismatch: database has split_interval '{}' but config specifies '{}'",
                            db_interval_val, split_interval
                        ).into());
                    }
                }

                println!("SQLite database: {}", db_path);
                println!("Session UUID: {}", existing_uuid.unwrap());
            } else {
                // New database - insert metadata with new uuid
                let session_uuid = Uuid::new_v4().to_string();
                conn.execute("INSERT INTO metadata (key, value) VALUES ('uuid', ?1)", [&session_uuid])?;
                conn.execute("INSERT INTO metadata (key, value) VALUES ('name', ?1)", [&name])?;
                conn.execute("INSERT INTO metadata (key, value) VALUES ('audio_format', ?1)", [audio_format_str])?;
                conn.execute("INSERT INTO metadata (key, value) VALUES ('split_interval', ?1)", [&split_interval.to_string()])?;

                println!("SQLite database: {}", db_path);
                println!("Session UUID: {}", session_uuid);
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
        StorageFormat::File => {
            match audio_format {
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
            }
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
    let mut segment_start_samples: u64 = 0; // For SQLite timestamp calculation
    let mut files_written: Vec<String> = vec![output_filename.clone()];

    // Helper to insert segment into SQLite
    let insert_segment = |conn: &Connection, timestamp_ms: i64, data: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        conn.execute(
            "INSERT INTO segments (timestamp_ms, audio_data) VALUES (?1, ?2)",
            rusqlite::params![timestamp_ms, data],
        )?;
        Ok(())
    };

    // Decode and encode in real-time
    println!("Decoding and encoding to {:?}...", audio_format);
    let mut total_input_samples = 0usize;
    let mut packets_decoded = 0usize;
    let mut total_output_samples: u64 = 0;

    // Buffer for collecting mono samples before encoding
    let mut mono_buffer: Vec<i16> = Vec::new();
    let mut encode_output = vec![0u8; 8192]; // Buffer for encoded output

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
                        // Get the audio buffer spec
                        let spec = *decoded.spec();
                        let duration = decoded.capacity() as u64;

                        // Create a sample buffer to hold the decoded samples
                        let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                        sample_buf.copy_interleaved_ref(decoded);

                        let samples = sample_buf.samples();
                        total_input_samples += samples.len();

                        // Convert to mono
                        let mono_samples: Vec<i16> = if src_channels == 1 {
                            samples.to_vec()
                        } else {
                            // Average channels to mono
                            samples
                                .chunks(src_channels as usize)
                                .map(|chunk| {
                                    let sum: i32 = chunk.iter().map(|&s| s as i32).sum();
                                    (sum / chunk.len() as i32) as i16
                                })
                                .collect()
                        };

                        // Resample to output sample rate
                        let resampled = resample(&mono_samples, src_sample_rate, output_sample_rate);
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
                                                            file.write_all(&encode_output[..info.output_size])?;
                                                        }
                                                    }
                                                    StorageFormat::Sqlite => {
                                                        segment_buffer.extend_from_slice(&encode_output[..info.output_size]);
                                                    }
                                                }

                                                // Check if we need to split
                                                if split_interval > 0 && segment_samples >= split_samples {
                                                    match storage_format {
                                                        StorageFormat::File => {
                                                            segment_number += 1;
                                                            segment_samples = 0;
                                                            let new_filename = generate_filename(Some(segment_number));
                                                            println!("\nStarting new segment: {}", new_filename);
                                                            files_written.push(new_filename.clone());
                                                            output_file = Some(File::create(&new_filename)?);
                                                        }
                                                        StorageFormat::Sqlite => {
                                                            if let Some(ref conn) = sqlite_conn {
                                                                let timestamp_ms = base_timestamp_ms + (segment_start_samples as i64 * 1000 / output_sample_rate as i64);
                                                                insert_segment(conn, timestamp_ms, &segment_buffer)?;
                                                                println!("\nInserted segment {} ({} bytes)", segment_number, segment_buffer.len());
                                                            }
                                                            segment_buffer.clear();
                                                            segment_number += 1;
                                                            segment_start_samples = total_output_samples;
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
                                                            // Check if this is the last packet before split
                                                            let is_end_of_segment = split_interval > 0 && segment_samples >= split_samples;
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
                                                        // Store raw Opus packets with length prefix
                                                        segment_buffer.extend_from_slice(&(len as u16).to_le_bytes());
                                                        segment_buffer.extend_from_slice(&encode_output[..len]);
                                                    }
                                                }

                                                // Check if we need to split
                                                if split_interval > 0 && segment_samples >= split_samples {
                                                    match storage_format {
                                                        StorageFormat::File => {
                                                            segment_number += 1;
                                                            segment_samples = 0;
                                                            let new_filename = generate_filename(Some(segment_number));
                                                            println!("\nStarting new segment: {}", new_filename);
                                                            files_written.push(new_filename.clone());
                                                            ogg_writer = Some(create_ogg_writer(&new_filename)?);
                                                        }
                                                        StorageFormat::Sqlite => {
                                                            if let Some(ref conn) = sqlite_conn {
                                                                let timestamp_ms = base_timestamp_ms + (segment_start_samples as i64 * 1000 / output_sample_rate as i64);
                                                                insert_segment(conn, timestamp_ms, &segment_buffer)?;
                                                                println!("\nInserted segment {} ({} bytes)", segment_number, segment_buffer.len());
                                                            }
                                                            segment_buffer.clear();
                                                            segment_number += 1;
                                                            segment_start_samples = total_output_samples;
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
                                            // Store raw PCM samples as bytes
                                            for sample in &frame {
                                                segment_buffer.extend_from_slice(&sample.to_le_bytes());
                                            }
                                        }
                                    }

                                    // Check if we need to split
                                    if split_interval > 0 && segment_samples >= split_samples {
                                        match storage_format {
                                            StorageFormat::File => {
                                                // Finalize current WAV file
                                                if let Some(writer) = wav_writer.take() {
                                                    writer.finalize()?;
                                                }
                                                segment_number += 1;
                                                segment_samples = 0;
                                                let new_filename = generate_filename(Some(segment_number));
                                                println!("\nStarting new segment: {}", new_filename);
                                                files_written.push(new_filename.clone());
                                                wav_writer = Some(create_wav_writer(&new_filename)?);
                                            }
                                            StorageFormat::Sqlite => {
                                                if let Some(ref conn) = sqlite_conn {
                                                    let timestamp_ms = base_timestamp_ms + (segment_start_samples as i64 * 1000 / output_sample_rate as i64);
                                                    insert_segment(conn, timestamp_ms, &segment_buffer)?;
                                                    println!("\nInserted segment {} ({} bytes)", segment_number, segment_buffer.len());
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

                        // Progress update every 100 packets
                        if packets_decoded % 100 == 0 {
                            let duration_secs = total_input_samples as f64
                                / (src_sample_rate as f64 * src_channels as f64);
                            print!("\rDecoded {:.1}s of audio...", duration_secs);
                            std::io::stdout().flush()?;
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
                // End of stream
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

    // Encode any remaining samples (pad with silence if needed for AAC/Opus, write as-is for WAV)
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
                                segment_buffer.extend_from_slice(&encode_output[..info.output_size]);
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
                // WAV doesn't need padding - write remaining samples as-is
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
            // Insert final segment if there's any buffered data
            if !segment_buffer.is_empty() {
                if let Some(ref conn) = sqlite_conn {
                    let timestamp_ms = base_timestamp_ms + (segment_start_samples as i64 * 1000 / output_sample_rate as i64);
                    insert_segment(conn, timestamp_ms, &segment_buffer)?;
                    println!("\nInserted final segment {} ({} bytes)", segment_number, segment_buffer.len());
                }
            }
        }
    }

    println!(); // New line after progress

    // Wait for download thread to finish
    let bytes_downloaded = download_handle.join().expect("Download thread panicked");

    if total_input_samples == 0 {
        return Err("No audio samples decoded".into());
    }

    let duration_secs =
        total_input_samples as f64 / (src_sample_rate as f64 * src_channels as f64);
    println!(
        "Decoded {} samples from {} packets",
        total_input_samples, packets_decoded
    );

    match storage_format {
        StorageFormat::File => {
            if files_written.len() > 1 {
                println!(
                    "Successfully saved {} files ({:.1} seconds of audio, {} bytes downloaded):",
                    files_written.len(), duration_secs, bytes_downloaded
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
            let total_segments = segment_number + 1; // segment_number is 0-indexed
            println!(
                "Successfully saved {} segments to {} ({:.1} seconds of audio, {} bytes downloaded)",
                total_segments, db_path, duration_secs, bytes_downloaded
            );
        }
    }

    Ok(())
}

// Serve subcommand implementation
fn serve(sqlite_file: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // Verify database exists and is Opus format
    if !sqlite_file.exists() {
        return Err(format!("Database file not found: {}", sqlite_file.display()).into());
    }

    let conn = Connection::open(&sqlite_file)?;

    // Check audio format
    let audio_format: String = conn
        .query_row("SELECT value FROM metadata WHERE key = 'audio_format'", [], |row| row.get(0))
        .map_err(|_| "Database missing audio_format metadata")?;

    if audio_format != "opus" {
        return Err(format!("Only Opus format is supported for serving, found: {}", audio_format).into());
    }

    let db_path = sqlite_file.to_string_lossy().to_string();
    println!("Starting server for: {}", db_path);
    println!("Listening on: http://0.0.0.0:{}", port);
    println!("Endpoint: GET /audio?start_id=<N>&end_id=<N>");

    // Create tokio runtime and run server
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app_state = StdArc::new(AppState { db_path });

        let app = Router::new()
            .route("/audio", get(audio_handler))
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await
            .map_err(|e| format!("Failed to bind to port {}: {}", port, e))?;
        axum::serve(listener, app).await
            .map_err(|e| format!("Server error: {}", e))?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

// State for axum handlers
struct AppState {
    db_path: String,
}

// Query parameters for audio endpoint
#[derive(Deserialize)]
struct AudioQuery {
    start_id: i64,
    end_id: i64,
}

// Audio endpoint handler
async fn audio_handler(
    State(state): State<StdArc<AppState>>,
    Query(query): Query<AudioQuery>,
) -> impl IntoResponse {
    let conn = match Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response(),
    };

    // Get max id
    let max_id: i64 = match conn.query_row("SELECT MAX(id) FROM segments", [], |row| row.get(0)) {
        Ok(id) => id,
        Err(_) => return (StatusCode::NOT_FOUND, "No segments in database").into_response(),
    };

    // Validate end_id
    if query.end_id > max_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("end_id {} exceeds max id {}", query.end_id, max_id),
        ).into_response();
    }

    if query.start_id > query.end_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("start_id {} cannot be greater than end_id {}", query.start_id, query.end_id),
        ).into_response();
    }

    // Query segments
    let mut stmt = match conn.prepare("SELECT audio_data FROM segments WHERE id >= ?1 AND id <= ?2 ORDER BY id") {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Query error: {}", e)).into_response(),
    };

    let segments: Vec<Vec<u8>> = match stmt.query_map([query.start_id, query.end_id], |row| row.get(0)) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Fetch error: {}", e)).into_response(),
    };

    if segments.is_empty() {
        return (StatusCode::NOT_FOUND, "No segments found in range").into_response();
    }

    // First pass: count total samples to calculate duration
    let mut total_samples: u64 = 0;
    for segment in &segments {
        let mut offset = 0;
        while offset + 2 <= segment.len() {
            let len = u16::from_le_bytes([segment[offset], segment[offset + 1]]) as usize;
            offset += 2;
            if offset + len > segment.len() {
                break;
            }
            offset += len;
            total_samples += 960; // Each Opus packet is 960 samples at 48kHz
        }
    }
    let duration_secs = total_samples as f64 / 48000.0;

    // Build Ogg container with Opus data
    let mut ogg_data = Vec::new();
    let mut granule_pos: u64 = 0;
    {
        let mut writer = PacketWriter::new(&mut ogg_data);

        // Write OpusHead header (required for valid Ogg/Opus)
        let mut opus_head = Vec::new();
        opus_head.extend_from_slice(b"OpusHead");  // Magic signature
        opus_head.push(1);                          // Version
        opus_head.push(1);                          // Channel count (mono)
        opus_head.extend_from_slice(&0u16.to_le_bytes());  // Pre-skip
        opus_head.extend_from_slice(&48000u32.to_le_bytes()); // Sample rate
        opus_head.extend_from_slice(&0i16.to_le_bytes());  // Output gain
        opus_head.push(0);                          // Channel mapping family

        if let Err(e) = writer.write_packet(
            opus_head,
            1,
            ogg::writing::PacketWriteEndInfo::EndPage,
            0,
        ) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("OpusHead write error: {}", e)).into_response();
        }

        // Write OpusTags header with duration
        let mut opus_tags = Vec::new();
        opus_tags.extend_from_slice(b"OpusTags");  // Magic signature
        let vendor = b"save_audio_stream";
        opus_tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        opus_tags.extend_from_slice(vendor);
        // Add DURATION comment
        let duration_comment = format!("DURATION={:.3}", duration_secs);
        opus_tags.extend_from_slice(&1u32.to_le_bytes());  // 1 comment
        opus_tags.extend_from_slice(&(duration_comment.len() as u32).to_le_bytes());
        opus_tags.extend_from_slice(duration_comment.as_bytes());

        if let Err(e) = writer.write_packet(
            opus_tags,
            1,
            ogg::writing::PacketWriteEndInfo::EndPage,
            0,
        ) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("OpusTags write error: {}", e)).into_response();
        }

        // Write audio data packets
        for (i, segment) in segments.iter().enumerate() {
            // Parse length-prefixed Opus packets from segment
            let mut offset = 0;
            while offset + 2 <= segment.len() {
                let len = u16::from_le_bytes([segment[offset], segment[offset + 1]]) as usize;
                offset += 2;

                if offset + len > segment.len() {
                    break;
                }

                let packet = &segment[offset..offset + len];
                offset += len;

                // Each Opus packet is 960 samples at 48kHz (20ms)
                granule_pos += 960;

                let is_last = i == segments.len() - 1 && offset >= segment.len();
                let end_info = if is_last {
                    ogg::writing::PacketWriteEndInfo::EndStream
                } else {
                    ogg::writing::PacketWriteEndInfo::NormalPacket
                };

                if let Err(e) = writer.write_packet(packet.to_vec(), 1, end_info, granule_pos) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("Ogg write error: {}", e)).into_response();
                }
            }
        }
    }

    (
        StatusCode::OK,
        [
            ("content-type", "audio/ogg"),
            ("x-duration-seconds", &format!("{:.3}", duration_secs)),
        ],
        ogg_data,
    ).into_response()
}
