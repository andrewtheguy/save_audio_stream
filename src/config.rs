use clap::ValueEnum;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigType {
    /// Recording configuration
    Record,
    /// Receiver configuration
    Receiver,
}

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    /// AAC-LC format (16kHz mono, 32kbps)
    ///
    /// ⚠️ EXPERIMENTAL:
    /// - The fdk-aac library binding is not widely used
    /// - May be replaced with FFmpeg-based encoding in the future
    ///
    /// Recommendation: Use Opus for production workloads
    Aac,
    /// Opus format (48kHz mono, 16kbps)
    ///
    /// Recommended for production use - provides best quality at low bitrates
    /// with guaranteed gapless playback support
    Opus,
    /// WAV format (lossless, preserves original sample rate)
    Wav,
}

/// Schedule configuration for recording during specific hours
#[derive(Debug, Clone, Deserialize)]
pub struct Schedule {
    /// Start recording at this time (HH:MM in UTC)
    pub record_start: String,
    /// Stop recording at this time (HH:MM in UTC)
    pub record_end: String,
}

fn default_api_port() -> u16 {
    17000
}

/// Multi-session recording configuration file structure
#[derive(Debug, Deserialize)]
pub struct MultiSessionConfig {
    /// Configuration type (must be "record")
    pub config_type: ConfigType,
    /// Array of recording sessions
    pub sessions: Vec<SessionConfig>,
    /// Global output directory for all sessions (default: tmp)
    pub output_dir: Option<PathBuf>,
    /// Global API server port for all sessions (default: 17000)
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// Export configuration (maps to [export] section in TOML)
    pub export: Option<ExportConfig>,
    /// SFTP configuration (maps to [sftp] section in TOML)
    pub sftp: Option<SftpExportConfig>,
}

/// Export configuration (maps to [export] section in TOML)
#[derive(Debug, Clone, Deserialize)]
pub struct ExportConfig {
    /// Export destination for /api/sync/shows/{name}/sections/{id}/export endpoint:
    /// - true: upload to SFTP server (requires [sftp] config)
    /// - false: save to local file in output_dir
    #[serde(default)]
    pub to_sftp: bool,
    /// Periodic export behavior (requires to_sftp = true):
    /// - true: automatically export new sections every hour
    /// - false: export only when requested via API call
    #[serde(default)]
    pub periodically: bool,
}

/// SFTP export configuration (maps to [sftp] section in TOML)
#[derive(Debug, Clone, Deserialize)]
pub struct SftpExportConfig {
    /// SFTP server hostname or IP address
    pub host: String,
    /// SFTP server port (default: 22)
    pub port: u16,
    /// SFTP username for authentication
    pub username: String,
    /// Credential profile name to look up password from ~/.config/save_audio_stream/credentials
    pub credential_profile: String,
    /// Remote directory path where files will be uploaded (e.g., /uploads/audio)
    pub remote_dir: String,
}

fn default_receiver_port() -> u16 {
    18000
}

fn default_sync_interval() -> u64 {
    60
}

fn default_database_prefix() -> String {
    "show".to_string()
}

/// PostgreSQL database configuration
#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL without password (e.g., postgres://user@host:5432)
    pub url: String,
    /// Credential profile name to look up password from ~/.config/save_audio_stream/credentials.toml
    /// Password is retrieved from [postgres.<credential_profile>] section
    pub credential_profile: String,
    /// Database name prefix between "save_audio_" and the show name (default: "show")
    /// For example, with prefix "show" and show name "am1430", the database name will be "save_audio_show_am1430"
    #[serde(default = "default_database_prefix")]
    pub prefix: String,
}

/// Per-show configuration for receiver mode
#[derive(Debug, Clone, Deserialize)]
pub struct ShowConfig {
    /// Show name to sync
    pub name: String,
    /// Retention period in hours (None = no cleanup, sync all data)
    /// When set, sync skips fetching segments older than retention period
    /// and deletes local sections older than retention after sync
    pub retention_hours: Option<i64>,
}

/// Sync configuration file structure (used by receiver command)
#[derive(Debug, Deserialize, Clone)]
pub struct SyncConfig {
    /// Configuration type (must be "receiver")
    pub config_type: ConfigType,
    /// URL of remote recording server (e.g., http://remote:3000)
    pub remote_url: String,
    /// PostgreSQL database configuration
    pub database: DatabaseConfig,
    /// Show configurations to sync (optional - if not specified, sync all shows from remote)
    /// Each show can have its own retention policy
    pub shows: Option<Vec<ShowConfig>>,
    /// Chunk size for batch fetching (default: 100)
    pub chunk_size: Option<u64>,
    /// Port for the receiver HTTP server (default: 18000)
    #[serde(default = "default_receiver_port")]
    pub port: u16,
    /// Interval in seconds between sync polling (default: 60)
    #[serde(default = "default_sync_interval")]
    pub sync_interval_seconds: u64,
    /// Custom lease name for sync coordination (default: "sync")
    /// Useful for testing to allow parallel execution with unique lease names
    #[serde(default)]
    pub lease_name: Option<String>,
}

/// Single session configuration
#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfig {
    /// URL of the Shoutcast/Icecast stream (required)
    pub url: String,
    /// Schedule for recording during specific hours (required)
    pub schedule: Schedule,
    /// Audio format: aac, opus, or wav (default: opus)
    pub audio_format: Option<AudioFormat>,
    /// Bitrate in kbps (default: 32 for AAC, 16 for Opus)
    pub bitrate: Option<u32>,
    /// Name prefix for output file (required)
    pub name: String,
    /// Split interval in seconds (0 = no splitting)
    pub split_interval: Option<u64>,
    /// Retention period in hours (default: 168 hours = 1 week)
    pub retention_hours: Option<i64>,
    /// Output directory (populated from global config, not in TOML)
    #[serde(skip)]
    pub output_dir: Option<PathBuf>,
}

impl MultiSessionConfig {
    /// Check if SFTP export is enabled
    pub fn export_to_sftp(&self) -> bool {
        self.export.as_ref().map(|e| e.to_sftp).unwrap_or(false)
    }

    /// Check if periodic export is enabled
    pub fn export_periodically(&self) -> bool {
        self.export.as_ref().map(|e| e.periodically).unwrap_or(false)
    }

    /// Validate SFTP configuration
    ///
    /// If `export.to_sftp` is true, ensures that the `sftp` configuration section exists.
    /// If `export.periodically` is true, ensures that `export.to_sftp` is also true.
    pub fn validate_sftp(&self) -> Result<(), String> {
        if self.export_to_sftp() {
            if self.sftp.is_none() {
                return Err(
                    "[export] to_sftp is enabled but [sftp] section is missing in config".to_string(),
                );
            }
        }

        // Validate that periodic export requires SFTP export to be enabled
        if self.export_periodically() && !self.export_to_sftp() {
            return Err(
                "[export] periodically requires to_sftp to be true".to_string(),
            );
        }

        Ok(())
    }
}
