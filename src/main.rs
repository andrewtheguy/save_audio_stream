mod audio;
mod config;
mod record;
mod schedule;
mod serve;
mod streaming;
mod sync;
mod webm;

use chrono::Timelike;
use clap::{Parser, Subcommand};
use config::{ConfigType, MultiSessionConfig, SyncConfig};
use schedule::{parse_time, seconds_until_start, time_to_minutes};
use std::path::PathBuf;
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
        Command::Serve { sqlite_file, port } => serve::serve_audio(sqlite_file, port),
        Command::Sync { config } => sync_from_config(config),
    }
}

fn record_multi_session(config_path: PathBuf, port_override: Option<u16>) -> Result<(), Box<dyn std::error::Error>> {
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

    println!("Starting {} recording session(s)", multi_config.sessions.len());

    // Determine output directory and API port
    let output_dir = multi_config.output_dir.clone().unwrap_or_else(|| "tmp".to_string());
    let api_port = port_override.unwrap_or(multi_config.api_port);

    // Spawn recording session threads first (they run in background)
    let mut handles = Vec::new();
    for (_session_idx, mut session_config) in multi_config.sessions.into_iter().enumerate() {
        // Copy global output_dir to session config
        session_config.output_dir = Some(output_dir.clone());

        let session_name = session_config.name.clone();
        let session_name_for_handle = session_name.clone();

        let handle = thread::spawn(move || {
            // Supervision loop for this session
            loop {
                println!("[{}] Starting recording session", session_name_for_handle);

                match record::record(session_config.clone()) {
                    Ok(_) => {
                        // record() runs indefinitely, should never return Ok
                        eprintln!("[{}] Recording ended unexpectedly", session_name_for_handle);
                    }
                    Err(e) => {
                        eprintln!("[{}] Recording failed: {}", session_name_for_handle, e);
                    }
                }

                // Calculate wait time until next scheduled start
                if let Ok((start_hour, start_min)) = parse_time(&session_config.schedule.record_start) {
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
                    eprintln!("[{}] Invalid schedule time, waiting 60 seconds before retry", session_name_for_handle);
                    thread::sleep(Duration::from_secs(60));
                }
            }
        });

        handles.push((session_name, handle));
    }

    // Run API server in main thread (blocking)
    // This allows future API-based control of the program
    let output_dir_path = PathBuf::from(output_dir);
    println!("Starting API server on port {} (main thread)", api_port);

    // API server runs in main thread - if it fails, program exits
    if let Err(e) = serve::serve_for_sync(output_dir_path, api_port) {
        eprintln!("API server failed: {}", e);
        eprintln!("Aborting program due to API server failure");
        std::process::abort();
    }

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
