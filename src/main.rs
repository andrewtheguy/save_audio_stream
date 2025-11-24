use clap::{Parser, Subcommand};
use save_audio_stream::config::{ConfigType, MultiSessionConfig, SyncConfig};
use save_audio_stream::{record, serve};
use std::path::PathBuf;

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
    /// Inspect audio from SQLite database via HTTP
    Inspect {
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
    /// Receive and browse synced shows from remote server
    Receiver {
        /// Path to receiver config file (TOML format, same as sync config)
        #[arg(short, long)]
        config: PathBuf,

        /// Sync once and exit without starting the server
        #[arg(long)]
        sync_only: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args = Args::parse();

    match args.command {
        Command::Record { config, port } => record_multi_session(config, port),
        Command::Inspect { sqlite_file, port, immutable } => serve::inspect_audio(sqlite_file, port, immutable),
        Command::Receiver { config, sync_only } => receiver_from_config(config, sync_only),
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

fn receiver_from_config(config_path: PathBuf, sync_only: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Load receiver/sync config file
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
            "Config file '{}' has config_type = {:?}, but 'receiver' command requires config_type = 'sync'",
            config_path.display(),
            sync_config.config_type
        )
        .into());
    }

    if sync_only {
        // Sync once and exit
        save_audio_stream::sync::sync_shows(
            sync_config.remote_url,
            sync_config.local_dir,
            sync_config.shows,
            sync_config.chunk_size.unwrap_or(100),
        )
    } else {
        // Call receiver function which starts the server and background sync
        serve::receiver_audio(sync_config)
    }
}
