use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Credentials file structure
///
/// Format:
/// ```toml
/// [sftp.profile_name]
/// password = "your_sftp_password_here"
///
/// [postgres.profile_name]
/// password = "your_postgres_password_here"
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Credentials {
    #[serde(default)]
    pub sftp: HashMap<String, CredentialProfile>,
    #[serde(default)]
    pub postgres: HashMap<String, CredentialProfile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CredentialProfile {
    pub password: String,
}

/// Credential type for looking up passwords
#[derive(Debug, Clone, Copy)]
pub enum CredentialType {
    Sftp,
    Postgres,
}

/// Get the default credentials file path: ~/.config/save_audio_stream/credentials.toml
pub fn get_credentials_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home)
        .join(".config")
        .join("save_audio_stream")
        .join("credentials.toml")
}

/// Load credentials from the default location
/// Returns None if the file doesn't exist
pub fn load_credentials() -> Result<Option<Credentials>, Box<dyn std::error::Error + Send + Sync>> {
    let creds_path = get_credentials_path();

    if !creds_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&creds_path)?;
    let credentials: Credentials = toml::from_str(&content)?;

    Ok(Some(credentials))
}

/// Get password for a specific profile and credential type
pub fn get_password(
    credentials: &Option<Credentials>,
    cred_type: CredentialType,
    profile: &str,
) -> Result<String, String> {
    let section_name = match cred_type {
        CredentialType::Sftp => "sftp",
        CredentialType::Postgres => "postgres",
    };

    match credentials {
        Some(creds) => {
            let profiles = match cred_type {
                CredentialType::Sftp => &creds.sftp,
                CredentialType::Postgres => &creds.postgres,
            };
            profiles
                .get(profile)
                .map(|p| p.password.clone())
                .ok_or_else(|| {
                    format!(
                        "Credential profile '[{}.{}]' not found in credentials file",
                        section_name, profile
                    )
                })
        }
        None => Err(format!(
            "Credentials file not found. Expected at: {}",
            get_credentials_path().display()
        )),
    }
}
