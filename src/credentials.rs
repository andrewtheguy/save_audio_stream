use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Credentials file structure
///
/// Format:
/// ```toml
/// [profile_name]
/// password = "your_password_here"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct Credentials {
    #[serde(flatten)]
    pub profiles: HashMap<String, CredentialProfile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CredentialProfile {
    pub password: String,
}

/// Get the default credentials file path: ~/.config/save_audio_stream/credentials
pub fn get_credentials_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home)
        .join(".config")
        .join("save_audio_stream")
        .join("credentials")
}

/// Load credentials from the default location
/// Returns None if the file doesn't exist
pub fn load_credentials() -> Result<Option<Credentials>, Box<dyn std::error::Error>> {
    let creds_path = get_credentials_path();

    if !creds_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&creds_path)?;
    let credentials: Credentials = toml::from_str(&content)?;

    Ok(Some(credentials))
}

/// Get password for a specific profile
pub fn get_password(
    credentials: &Option<Credentials>,
    profile: &str,
) -> Result<String, String> {
    match credentials {
        Some(creds) => {
            creds.profiles
                .get(profile)
                .map(|p| p.password.clone())
                .ok_or_else(|| format!("Credential profile '{}' not found in credentials file", profile))
        }
        None => Err(format!(
            "Credentials file not found. Expected at: {}",
            get_credentials_path().display()
        )),
    }
}
