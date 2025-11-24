use clap::ValueEnum;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigType {
    /// Recording configuration
    Record,
    /// Syncing configuration
    Sync,
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
    3000
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
    /// Global API server port for all sessions (default: 3000)
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// Enable SFTP export for section exports (default: false)
    pub export_to_sftp: Option<bool>,
    /// SFTP configuration (maps to [sftp] section in TOML)
    pub sftp: Option<SftpExportConfig>,
    /// Enable periodic export of unexported sections to SFTP every hour (default: false)
    /// Requires export_to_sftp to be true
    pub export_to_remote_periodically: Option<bool>,
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

/// Sync configuration file structure
#[derive(Debug, Deserialize)]
pub struct SyncConfig {
    /// Configuration type (must be "sync")
    pub config_type: ConfigType,
    /// URL of remote recording server (e.g., http://remote:3000)
    pub remote_url: String,
    /// Local base directory for synced databases
    pub local_dir: PathBuf,
    /// Show names to sync (optional - if not specified, sync all shows from remote)
    pub shows: Option<Vec<String>>,
    /// Chunk size for batch fetching (default: 100)
    pub chunk_size: Option<u64>,
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
    /// Validate SFTP configuration
    ///
    /// If `export_to_sftp` is true, ensures that the `sftp` configuration section exists
    /// and contains all required fields.
    /// If `export_to_remote_periodically` is true, ensures that `export_to_sftp` is also true.
    pub fn validate_sftp(&self) -> Result<(), String> {
        if self.export_to_sftp.unwrap_or(false) {
            if self.sftp.is_none() {
                return Err(
                    "export_to_sftp is enabled but [sftp] section is missing in config".to_string(),
                );
            }
        }

        // Validate that periodic export requires SFTP export to be enabled
        if self.export_to_remote_periodically.unwrap_or(false) {
            if !self.export_to_sftp.unwrap_or(false) {
                return Err(
                    "export_to_remote_periodically requires export_to_sftp to be true".to_string(),
                );
            }
        }

        Ok(())
    }
}
