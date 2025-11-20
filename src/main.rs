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
use config::MultiSessionConfig;
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
        /// URL of remote recording server (e.g., http://remote:3000)
        #[arg(short, long)]
        remote_url: String,

        /// Local base directory for synced databases
        #[arg(short, long)]
        local_dir: PathBuf,

        /// Show names to sync (can specify multiple)
        #[arg(short = 'n', long = "show", num_args = 1..)]
        shows: Vec<String>,

        /// Chunk size for batch fetching (default: 100)
        #[arg(short = 's', long, default_value = "100")]
        chunk_size: u64,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args = Args::parse();

    match args.command {
        Command::Record { config, port } => record_multi_session(config, port),
        Command::Serve { sqlite_file, port } => serve::serve(sqlite_file, port),
        Command::Sync {
            remote_url,
            local_dir,
            shows,
            chunk_size,
        } => sync::sync_shows(remote_url, local_dir, shows, chunk_size),
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

    if multi_config.sessions.is_empty() {
        return Err("No sessions defined in config file".into());
    }

    println!("Starting {} recording session(s)", multi_config.sessions.len());

    // Spawn a thread for each session
    let mut handles = Vec::new();
    for (session_idx, mut session_config) in multi_config.sessions.into_iter().enumerate() {
        // Copy global output_dir to session config
        session_config.output_dir = multi_config.output_dir.clone();

        // Apply port override if provided (increment for each session to avoid conflicts)
        if let Some(base_port) = port_override {
            session_config.api_port = Some(base_port + session_idx as u16);
        }

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

    // Wait for all sessions (they run indefinitely)
    for (session_name, handle) in handles {
        if let Err(e) = handle.join() {
            eprintln!("[{}] Thread panicked: {:?}", session_name, e);
        }
    }

    Ok(())
}
