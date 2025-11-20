use clap::ValueEnum;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    /// AAC-LC format (16kHz mono, 32kbps)
    Aac,
    /// Opus format (48kHz mono, 16kbps)
    Opus,
    /// WAV format (lossless, preserves original sample rate)
    Wav,
}

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageFormat {
    /// Save to individual files
    File,
    /// Save to SQLite database
    Sqlite,
}

/// Schedule configuration for recording during specific hours
#[derive(Debug, Clone, Deserialize)]
pub struct Schedule {
    /// Start recording at this time (HH:MM in UTC)
    pub record_start: String,
    /// Stop recording at this time (HH:MM in UTC)
    pub record_end: String,
}

/// Multi-session configuration file structure
#[derive(Debug, Deserialize)]
pub struct MultiSessionConfig {
    /// Array of recording sessions
    pub sessions: Vec<SessionConfig>,
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
    /// Storage format: file or sqlite (default: sqlite)
    pub storage_format: Option<StorageFormat>,
    /// Bitrate in kbps (default: 32 for AAC, 16 for Opus)
    pub bitrate: Option<u32>,
    /// Name prefix for output file (required)
    pub name: String,
    /// Output directory (default: tmp)
    pub output_dir: Option<String>,
    /// Split interval in seconds (0 = no splitting)
    pub split_interval: Option<u64>,
}
