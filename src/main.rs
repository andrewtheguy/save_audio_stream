mod audio;
mod config;
mod constants;
mod credentials;
mod db;
mod fmp4;
mod record;
mod schedule;
mod serve;
mod serve_record;
mod sftp;
mod streaming;
mod sync;
mod webm;

use chrono::Timelike;
use clap::{Parser, Subcommand};
use config::{ConfigType, MultiSessionConfig, SyncConfig};
use dashmap::DashMap;
use reqwest::blocking::Client;
use save_audio_stream::{ShowLocks, get_show_lock};
use schedule::{parse_time, seconds_until_start, time_to_minutes};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Download and convert audio streams to compressed formats"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Record audio from multiple streams
    Record {
        /// Path to multi-session config file (TOML format with [[sessions]] array)
        #[arg(short, long)]
        config: PathBuf,

        /// Override API server port for all sessions (overrides config file setting)
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Serve audio from SQLite database via HTTP
    Serve {
        /// Path to SQLite database file
        sqlite_file: PathBuf,

        /// Port to listen on
        #[arg(short, long, default_value = "3000")]
        port: u16,

        /// Use immutable mode (WORKAROUND for network/read-only filesystems)
        /// WARNING: Only use for databases that cannot be modified. Setting immutable
        /// on a database that can change will cause SQLITE_CORRUPT errors or incorrect
        /// query results. This disables all locking and change detection.
        /// See: https://www.sqlite.org/uri.html#uriimmutable
        #[arg(long, default_value = "false")]
        immutable: bool,
    },
    /// Sync show(s) from remote recording server to local database
    Sync {
        /// Path to sync config file (TOML format)
        #[arg(short, long)]
        config: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args = Args::parse();

    match args.command {
        Command::Record { config, port } => record_multi_session(config, port),
        Command::Serve { sqlite_file, port, immutable } => serve::serve_audio(sqlite_file, port, immutable),
        Command::Sync { config } => sync_from_config(config),
    }
}

fn record_multi_session(
    config_path: PathBuf,
    port_override: Option<u16>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load multi-session config file
    let config_content = std::fs::read_to_string(&config_path).map_err(|e| {
        format!(
            "Failed to read config file '{}': {}",
            config_path.display(),
            e
        )
    })?;
    let multi_config: MultiSessionConfig = toml::from_str(&config_content).map_err(|e| {
        format!(
            "Failed to parse config file '{}': {}",
            config_path.display(),
            e
        )
    })?;

    // Validate config type
    if multi_config.config_type != ConfigType::Record {
        return Err(format!(
            "Config file '{}' has config_type = {:?}, but 'record' command requires config_type = 'record'",
            config_path.display(),
            multi_config.config_type
        )
        .into());
    }

    if multi_config.sessions.is_empty() {
        return Err("No sessions defined in config file".into());
    }

    // Validate SFTP configuration if enabled
    if let Err(e) = multi_config.validate_sftp() {
        return Err(format!("SFTP configuration error: {}", e).into());
    }

    // Load credentials file if SFTP export is enabled
    let credentials = if multi_config.export_to_sftp.unwrap_or(false) {
        println!("Loading credentials from {}...", credentials::get_credentials_path().display());
        match credentials::load_credentials() {
            Ok(creds) => creds,
            Err(e) => {
                return Err(format!("Failed to load credentials: {}", e).into());
            }
        }
    } else {
        None
    };

    // Determine output directory and API port
    let output_dir = multi_config
        .output_dir
        .clone()
        .unwrap_or_else(|| "tmp".to_string());
    let api_port = port_override.unwrap_or(multi_config.api_port);

    // Extract SFTP config for API server
    let sftp_config = if multi_config.export_to_sftp.unwrap_or(false) {
        multi_config.sftp.clone()
    } else {
        None
    };

    // Test SFTP connection if enabled
    if let Some(ref config) = sftp_config {
        use sftp::{SftpClient, SftpConfig};

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
        let options = sftp::UploadOptions::default();

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

    // Extract periodic export flag
    let export_to_remote_periodically = multi_config.export_to_remote_periodically.unwrap_or(false);

    // Extract session names for periodic export
    let session_names: Vec<String> = multi_config.sessions.iter()
        .map(|s| s.name.clone())
        .collect();

    // Create output directory if it doesn't exist
    let output_dir_path = PathBuf::from(output_dir.clone());
    std::fs::create_dir_all(&output_dir_path)?;
    println!("Output directory: {}", output_dir_path.display());

    // Create shared locks for coordinating export and cleanup operations
    let show_locks: ShowLocks = Arc::new(DashMap::new());
    let locks_for_server = show_locks.clone();
    let locks_for_recording = show_locks.clone();

    // Start API server first in a separate thread
    println!("Starting API server on port {}", api_port);

    let api_handle = thread::spawn(move || {
        if let Err(e) = serve_record::serve_for_sync(
            output_dir_path,
            api_port,
            sftp_config,
            export_to_remote_periodically,
            session_names,
            credentials,
            locks_for_server,  // Pass locks to API server
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

    // Now spawn recording session threads (they run in background with supervision)
    let mut recording_handles = Vec::new();
    for (_session_idx, mut session_config) in multi_config.sessions.into_iter().enumerate() {
        // Copy global output_dir to session config
        session_config.output_dir = Some(output_dir.clone());

        let session_name = session_config.name.clone();
        let session_name_for_handle = session_name.clone();
        let locks_for_session = locks_for_recording.clone();

        let handle = thread::spawn(move || {
            // Supervision loop for this session
            loop {
                println!("[{}] Starting recording session", session_name_for_handle);

                match record::record(session_config.clone(), locks_for_session.clone()) {
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
                    parse_time(&session_config.schedule.record_start)
                {
                    let start_mins = time_to_minutes(start_hour, start_min);
                    let now = chrono::Utc::now();
                    let current_mins = time_to_minutes(now.hour(), now.minute());
                    let wait_secs = seconds_until_start(current_mins, start_mins);

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

fn sync_from_config(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // Load sync config file
    let config_content = std::fs::read_to_string(&config_path).map_err(|e| {
        format!(
            "Failed to read config file '{}': {}",
            config_path.display(),
            e
        )
    })?;
    let sync_config: SyncConfig = toml::from_str(&config_content).map_err(|e| {
        format!(
            "Failed to parse config file '{}': {}",
            config_path.display(),
            e
        )
    })?;

    // Validate config type
    if sync_config.config_type != ConfigType::Sync {
        return Err(format!(
            "Config file '{}' has config_type = {:?}, but 'sync' command requires config_type = 'sync'",
            config_path.display(),
            sync_config.config_type
        )
        .into());
    }

    // Call sync function with config values
    let local_dir = PathBuf::from(&sync_config.local_dir);
    let chunk_size = sync_config.chunk_size.unwrap_or(100);

    sync::sync_shows(
        sync_config.remote_url,
        local_dir,
        sync_config.shows,
        chunk_size,
    )
}
