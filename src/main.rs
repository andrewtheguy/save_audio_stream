mod audio;
mod config;
mod record;
mod schedule;
mod serve;
mod streaming;
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
    },
    /// Serve audio from SQLite database via HTTP
    Serve {
        /// Path to SQLite database file
        sqlite_file: PathBuf,

        /// Port to listen on
        #[arg(short, long, default_value = "3000")]
        port: u16,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args = Args::parse();

    match args.command {
        Command::Record { config } => record_multi_session(config),
        Command::Serve { sqlite_file, port } => serve::serve(sqlite_file, port),
    }
}

fn record_multi_session(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
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
    for session_config in multi_config.sessions {
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
