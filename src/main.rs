use clap::{Parser, Subcommand};
use save_audio_stream::config::{ConfigType, MultiSessionConfig, SyncConfig};
use save_audio_stream::{record, serve};
use std::path::PathBuf;

fn default_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("save_audio_stream")
}

fn default_record_config() -> PathBuf {
    default_config_dir().join("record.toml")
}

fn default_receiver_config() -> PathBuf {
    default_config_dir().join("receiver.toml")
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
        /// Default: ~/.config/save_audio_stream/record.toml
        #[arg(short, long, default_value_os_t = default_record_config())]
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
        #[arg(short, long, default_value = "16000")]
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
        /// Default: ~/.config/save_audio_stream/receiver.toml
        #[arg(short, long, default_value_os_t = default_receiver_config())]
        config: PathBuf,

        /// Sync once and exit without starting the server
        #[arg(long)]
        sync_only: bool,
    },
    /// Replace the source database for a receiver show
    ///
    /// Use this when the source server has been replaced and the receiver
    /// needs to continue syncing from a new source with a different unique_id.
    /// The operation finds a matching section on the new source based on
    /// timestamp proximity and updates the receiver's tracking metadata.
    ReplaceSource {
        /// Path to receiver config file (TOML format, same as sync config)
        /// Default: ~/.config/save_audio_stream/receiver.toml
        #[arg(short, long, default_value_os_t = default_receiver_config())]
        config: PathBuf,

        /// Name of the show to replace source for
        #[arg(long)]
        show: String,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init();

    let args = Args::parse();

    match args.command {
        Command::Record { config, port } => record_multi_session(config, port),
        Command::Inspect {
            sqlite_file,
            port,
            immutable,
        } => serve::inspect_audio(sqlite_file, port, immutable),
        Command::Receiver { config, sync_only } => receiver_from_config(config, sync_only),
        Command::ReplaceSource { config, show } => replace_source_command(config, show),
    }
}

fn record_multi_session(
    config_path: PathBuf,
    port_override: Option<u16>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

fn receiver_from_config(
    config_path: PathBuf,
    sync_only: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    if sync_config.config_type != ConfigType::Receiver {
        return Err(format!(
            "Config file '{}' has config_type = {:?}, but 'receiver' command requires config_type = 'receiver'",
            config_path.display(),
            sync_config.config_type
        )
        .into());
    }

    // Load credentials and get password for PostgreSQL
    let credentials = save_audio_stream::credentials::load_credentials()?;
    let password = save_audio_stream::credentials::get_password(
        &credentials,
        save_audio_stream::credentials::CredentialType::Postgres,
        &sync_config.database.credential_profile,
    )
    .map_err(|e| {
        format!(
            "Failed to get password for profile '{}': {}",
            sync_config.database.credential_profile, e
        )
    })?;

    if sync_only {
        // Sync once and exit - create global pool for lease management
        let rt = tokio::runtime::Runtime::new()?;
        let global_pool = rt.block_on(async {
            let pool = save_audio_stream::db_postgres::open_postgres_connection_create_if_needed(
                &sync_config.database.url,
                &password,
                save_audio_stream::db_postgres::GLOBAL_DATABASE_NAME,
            )
            .await?;
            save_audio_stream::db_postgres::create_leases_table_pg(&pool).await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(pool)
        })?;

        match save_audio_stream::sync::sync_shows(&sync_config, &password, &global_pool) {
            Ok(save_audio_stream::sync::SyncResult::Completed) => {
                println!("Sync completed successfully");
                Ok(())
            }
            Ok(save_audio_stream::sync::SyncResult::Skipped) => {
                println!("Sync skipped (another instance is syncing)");
                Ok(())
            }
            Err(e) => Err(e),
        }
    } else {
        // Call receiver function which starts the server and background sync
        serve::receiver_audio(sync_config, password)
    }
}

fn replace_source_command(
    config_path: PathBuf,
    show_name: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    if sync_config.config_type != ConfigType::Receiver {
        return Err(format!(
            "Config file '{}' has config_type = {:?}, but 'replace-source' command requires config_type = 'receiver'",
            config_path.display(),
            sync_config.config_type
        )
        .into());
    }

    // Load credentials and get password for PostgreSQL
    let credentials = save_audio_stream::credentials::load_credentials()?;
    let password = save_audio_stream::credentials::get_password(
        &credentials,
        save_audio_stream::credentials::CredentialType::Postgres,
        &sync_config.database.credential_profile,
    )
    .map_err(|e| {
        format!(
            "Failed to get password for profile '{}': {}",
            sync_config.database.credential_profile, e
        )
    })?;

    // Create global pool for lease management
    let rt = tokio::runtime::Runtime::new()?;
    let global_pool = rt.block_on(async {
        let pool = save_audio_stream::db_postgres::open_postgres_connection_create_if_needed(
            &sync_config.database.url,
            &password,
            save_audio_stream::db_postgres::GLOBAL_DATABASE_NAME,
        )
        .await?;
        save_audio_stream::db_postgres::create_leases_table_pg(&pool).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(pool)
    })?;

    // Run replace source operation
    match save_audio_stream::sync::replace_source(&sync_config, &password, &global_pool, &show_name)
    {
        Ok(save_audio_stream::sync::ReplaceSourceResult::Replaced {
            old_source_id,
            new_source_id,
            matched_section_id,
            matched_section_timestamp_ms,
            resume_from_segment_id,
        }) => {
            println!("Source replaced successfully!");
            println!("  Old source: {}", old_source_id);
            println!("  New source: {}", new_source_id);
            println!(
                "  Matched section: {} (timestamp: {})",
                matched_section_id, matched_section_timestamp_ms
            );
            println!("  Will resume from segment: {}", resume_from_segment_id);
            println!("\nRun 'receiver --sync-only' or 'receiver' to continue syncing from new source.");
            Ok(())
        }
        Ok(save_audio_stream::sync::ReplaceSourceResult::Skipped) => {
            println!("Replace source skipped (another sync/replace operation is in progress)");
            Ok(())
        }
        Ok(save_audio_stream::sync::ReplaceSourceResult::FreshStart { new_source_id }) => {
            println!("Receiver has no existing data.");
            println!("  New source: {}", new_source_id);
            println!("\nRun 'receiver --sync-only' or 'receiver' to start syncing from new source.");
            Ok(())
        }
        Err(e) => Err(e),
    }
}
