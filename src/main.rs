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

use clap::{Parser, Subcommand};
use config::{ConfigType, MultiSessionConfig, SyncConfig};
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// Define ShowLocks and get_show_lock for binary compilation
// (same as in lib.rs, needed because modules are compiled as part of binary)
pub type ShowLocks = Arc<DashMap<String, Arc<Mutex<()>>>>;

pub fn get_show_lock(locks: &ShowLocks, show_name: &str) -> Arc<Mutex<()>> {
    locks
        .entry(show_name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

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

    // Call the main logic function in record module
    record::run_multi_session(multi_config, port_override)
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
