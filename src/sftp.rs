use ssh2::{Session, Sftp};
use std::error::Error as StdError;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

/// SFTP-specific errors
#[derive(Debug)]
pub enum SftpError {
    /// Failed to establish TCP connection
    ConnectionFailed(String),
    /// SSH authentication failed
    AuthenticationFailed(String),
    /// Local file not found or unreadable
    LocalFileError(PathBuf, std::io::Error),
    /// Remote file operation failed
    RemoteFileError(PathBuf, String),
    /// Directory creation failed
    DirectoryError(PathBuf, String),
    /// File size mismatch after upload
    SizeMismatch { expected: u64, actual: u64 },
    /// General I/O error
    IoError(std::io::Error),
    /// SSH2 library error
    Ssh2Error(ssh2::Error),
}

impl fmt::Display for SftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SftpError::ConnectionFailed(msg) => write!(f, "Connection failed: {}", msg),
            SftpError::AuthenticationFailed(msg) => write!(f, "Authentication failed: {}", msg),
            SftpError::LocalFileError(path, err) => {
                write!(f, "Local file error '{}': {}", path.display(), err)
            }
            SftpError::RemoteFileError(path, msg) => {
                write!(f, "Remote file error '{}': {}", path.display(), msg)
            }
            SftpError::DirectoryError(path, msg) => {
                write!(f, "Directory error '{}': {}", path.display(), msg)
            }
            SftpError::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "Size mismatch: expected {} bytes, got {} bytes",
                    expected, actual
                )
            }
            SftpError::IoError(err) => write!(f, "I/O error: {}", err),
            SftpError::Ssh2Error(err) => write!(f, "SSH2 error: {}", err),
        }
    }
}

impl StdError for SftpError {}

impl From<std::io::Error> for SftpError {
    fn from(err: std::io::Error) -> Self {
        SftpError::IoError(err)
    }
}

impl From<ssh2::Error> for SftpError {
    fn from(err: ssh2::Error) -> Self {
        SftpError::Ssh2Error(err)
    }
}

pub type Result<T> = std::result::Result<T, SftpError>;

/// Authentication method for SFTP
#[derive(Debug, Clone)]
pub enum SftpAuth {
    /// Password-based authentication
    Password(String),
    /// Public key authentication with private key file
    KeyFile(PathBuf, Option<String>), // (key_path, optional passphrase)
}

/// Configuration for SFTP connection
#[derive(Debug, Clone)]
pub struct SftpConfig {
    /// Hostname or IP address
    pub host: String,
    /// Port number (default: 22)
    pub port: u16,
    /// Username for authentication
    pub username: String,
    /// Authentication method
    pub auth: SftpAuth,
}

impl SftpConfig {
    /// Create a new SFTP configuration with password auth
    pub fn with_password(host: String, port: u16, username: String, password: String) -> Self {
        Self {
            host,
            port,
            username,
            auth: SftpAuth::Password(password),
        }
    }

    /// Create a new SFTP configuration with key-based auth
    pub fn with_key_file(
        host: String,
        port: u16,
        username: String,
        key_path: PathBuf,
        passphrase: Option<String>,
    ) -> Self {
        Self {
            host,
            port,
            username,
            auth: SftpAuth::KeyFile(key_path, passphrase),
        }
    }

    /// Create an SFTP configuration from the config file structure
    /// Resolves the password from the credentials file using the credential_profile
    pub fn from_export_config(
        config: &crate::config::SftpExportConfig,
        credentials: &Option<crate::credentials::Credentials>,
    ) -> std::result::Result<Self, String> {
        let password = crate::credentials::get_password(
            credentials,
            crate::credentials::CredentialType::Sftp,
            &config.credential_profile,
        )?;

        Ok(Self::with_password(
            config.host.clone(),
            config.port,
            config.username.clone(),
            password,
        ))
    }
}

/// Options for file upload
#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// Buffer size for reading/writing (default: 64KB)
    pub buffer_size: usize,
    /// Use atomic upload (temp file + rename)
    pub atomic: bool,
    /// Verify file size after upload
    pub verify_size: bool,
    /// File permissions in octal (e.g., 0o644)
    pub permissions: Option<i32>,
    /// Progress callback: (bytes_uploaded, total_bytes)
    pub progress_callback: Option<fn(u64, u64)>,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            buffer_size: 64 * 1024, // 64KB
            atomic: true,
            verify_size: true,
            permissions: Some(0o644),
            progress_callback: None,
        }
    }
}

/// SFTP client for file operations
pub struct SftpClient {
    session: Session,
    sftp: Sftp,
}

impl SftpClient {
    /// Connect to SFTP server with the given configuration
    pub fn connect(config: &SftpConfig) -> Result<Self> {
        let addr = format!("{}:{}", config.host, config.port);

        // Establish TCP connection
        let tcp = TcpStream::connect(&addr).map_err(|e| {
            SftpError::ConnectionFailed(format!("Failed to connect to {}: {}", addr, e))
        })?;

        // Create SSH session
        let mut session = Session::new()?;
        session.set_tcp_stream(tcp);
        session.handshake()?;

        // Authenticate
        match &config.auth {
            SftpAuth::Password(password) => {
                session
                    .userauth_password(&config.username, password)
                    .map_err(|e| {
                        SftpError::AuthenticationFailed(format!(
                            "Password authentication failed for user '{}': {}",
                            config.username, e
                        ))
                    })?;
            }
            SftpAuth::KeyFile(key_path, passphrase) => {
                session
                    .userauth_pubkey_file(&config.username, None, key_path, passphrase.as_deref())
                    .map_err(|e| {
                        SftpError::AuthenticationFailed(format!(
                            "Key-based authentication failed for user '{}' with key '{}': {}",
                            config.username,
                            key_path.display(),
                            e
                        ))
                    })?;
            }
        }

        // Verify authentication succeeded
        if !session.authenticated() {
            return Err(SftpError::AuthenticationFailed(
                "Authentication failed (session not authenticated)".to_string(),
            ));
        }

        // Create SFTP channel
        let sftp = session.sftp()?;

        Ok(Self { session, sftp })
    }

    /// Create a directory recursively, similar to `mkdir -p`
    pub fn mkdir_p(&self, path: &Path, permissions: i32) -> Result<()> {
        let mut current = PathBuf::new();

        for component in path.components() {
            current.push(component);

            // Try to create directory
            match self.sftp.mkdir(&current, permissions) {
                Ok(_) => {
                    // Successfully created
                }
                Err(_) => {
                    // Check if directory already exists
                    match self.sftp.stat(&current) {
                        Ok(stat) => {
                            if !stat.is_dir() {
                                return Err(SftpError::DirectoryError(
                                    current.clone(),
                                    "Path exists but is not a directory".to_string(),
                                ));
                            }
                            // Directory exists, continue
                        }
                        Err(e) => {
                            // Directory doesn't exist and we couldn't create it
                            return Err(SftpError::DirectoryError(
                                current.clone(),
                                format!("Failed to create directory: {}", e),
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Upload data from a reader (e.g., in-memory buffer, file, etc.) to remote path
    ///
    /// This method efficiently uploads data using buffered I/O.
    /// If `atomic` is enabled, the data is first uploaded to a temporary location
    /// and then atomically renamed to the target path.
    ///
    /// # Arguments
    /// * `reader` - Any type implementing Read (e.g., &[u8], File, Cursor, etc.)
    /// * `remote_path` - Target path on the SFTP server
    /// * `size` - Size of the data in bytes (required for verification)
    /// * `options` - Upload options (atomic mode, buffer size, etc.)
    pub fn upload_stream<R: Read>(
        &self,
        reader: &mut R,
        remote_path: &Path,
        size: u64,
        options: &UploadOptions,
    ) -> Result<()> {
        // Determine actual remote path (temp or final)
        let (actual_remote_path, is_temp) = if options.atomic {
            // Use temporary path with .tmpupload suffix to avoid extra tmp file on reupload
            let temp_path = PathBuf::from(format!("{}.tmpupload", remote_path.display()));
            (temp_path, true)
        } else {
            (remote_path.to_path_buf(), false)
        };

        // Create parent directory if needed
        if let Some(parent) = actual_remote_path.parent() {
            if !parent.as_os_str().is_empty() {
                self.mkdir_p(parent, 0o755)?;
            }
        }

        // Create remote file with specified permissions
        let mut remote_file = if let Some(perms) = options.permissions {
            // Use open with flags to set permissions during creation
            self.sftp
                .open_mode(
                    &actual_remote_path,
                    ssh2::OpenFlags::WRITE | ssh2::OpenFlags::CREATE | ssh2::OpenFlags::TRUNCATE,
                    perms,
                    ssh2::OpenType::File,
                )
                .map_err(|e| {
                    SftpError::RemoteFileError(
                        actual_remote_path.clone(),
                        format!("Failed to create remote file: {}", e),
                    )
                })?
        } else {
            // Use default permissions (0o644)
            self.sftp.create(&actual_remote_path).map_err(|e| {
                SftpError::RemoteFileError(
                    actual_remote_path.clone(),
                    format!("Failed to create remote file: {}", e),
                )
            })?
        };

        // Upload data in chunks
        let mut buffer = vec![0u8; options.buffer_size];
        let mut uploaded = 0u64;

        loop {
            let n = reader.read(&mut buffer)?;

            if n == 0 {
                break; // EOF
            }

            remote_file.write_all(&buffer[..n]).map_err(|e| {
                SftpError::RemoteFileError(
                    actual_remote_path.clone(),
                    format!("Failed to write to remote file: {}", e),
                )
            })?;

            uploaded += n as u64;

            // Call progress callback
            if let Some(callback) = options.progress_callback {
                callback(uploaded, size);
            }
        }

        // Flush and close remote file explicitly
        remote_file.flush().map_err(|e| {
            SftpError::RemoteFileError(
                actual_remote_path.clone(),
                format!("Failed to flush remote file: {}", e),
            )
        })?;
        drop(remote_file);

        // Verify file size if requested
        if options.verify_size {
            let stat = self.sftp.stat(&actual_remote_path).map_err(|e| {
                SftpError::RemoteFileError(
                    actual_remote_path.clone(),
                    format!("Failed to stat remote file after upload: {}", e),
                )
            })?;

            let remote_size = stat.size.unwrap_or(0);
            if remote_size != size {
                // Clean up temp file if atomic upload
                if is_temp {
                    let _ = self.sftp.unlink(&actual_remote_path);
                }
                return Err(SftpError::SizeMismatch {
                    expected: size,
                    actual: remote_size,
                });
            }
        }

        // Atomic rename if using temporary file
        if is_temp {
            self.sftp
                .rename(&actual_remote_path, remote_path, None)
                .map_err(|e| {
                    SftpError::RemoteFileError(
                        remote_path.to_path_buf(),
                        format!("Failed to rename temp file to final path: {}", e),
                    )
                })?;
        }

        Ok(())
    }

    /// Upload a file from local path to remote path
    ///
    /// This method efficiently uploads large files using buffered I/O.
    /// If `atomic` is enabled, the file is first uploaded to a temporary location
    /// and then atomically renamed to the target path.
    pub fn upload_file(
        &self,
        local_path: &Path,
        remote_path: &Path,
        options: &UploadOptions,
    ) -> Result<()> {
        // Open local file
        let mut local_file = File::open(local_path)
            .map_err(|e| SftpError::LocalFileError(local_path.to_path_buf(), e))?;

        // Get file size
        let file_size = local_file
            .metadata()
            .map_err(|e| SftpError::LocalFileError(local_path.to_path_buf(), e))?
            .len();

        // Delegate to upload_stream
        self.upload_stream(&mut local_file, remote_path, file_size, options)
    }

    /// List files in a remote directory
    ///
    /// Returns a vector of filenames (not full paths) in the specified directory.
    pub fn list_files(&self, dir_path: &Path) -> Result<Vec<String>> {
        let entries = self.sftp.readdir(dir_path).map_err(|e| {
            SftpError::RemoteFileError(
                dir_path.to_path_buf(),
                format!("Failed to read directory: {}", e),
            )
        })?;

        let filenames: Vec<String> = entries
            .into_iter()
            .filter_map(|(path, stat)| {
                // Filter out directories, only return files
                if !stat.is_dir() {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();

        Ok(filenames)
    }

    /// Get file statistics for a remote file
    ///
    /// Returns file size and other metadata for the specified path.
    pub fn stat(&self, path: &Path) -> Result<ssh2::FileStat> {
        self.sftp.stat(path).map_err(|e| {
            SftpError::RemoteFileError(path.to_path_buf(), format!("Failed to stat file: {}", e))
        })
    }

    /// Download a file from remote path and return its contents as a Vec<u8>
    ///
    /// This method reads the entire file into memory. Use with caution for large files.
    pub fn download_file(&self, remote_path: &Path) -> Result<Vec<u8>> {
        let mut remote_file = self.sftp.open(remote_path).map_err(|e| {
            SftpError::RemoteFileError(
                remote_path.to_path_buf(),
                format!("Failed to open remote file: {}", e),
            )
        })?;

        let mut buffer = Vec::new();
        remote_file.read_to_end(&mut buffer).map_err(|e| {
            SftpError::RemoteFileError(
                remote_path.to_path_buf(),
                format!("Failed to read remote file: {}", e),
            )
        })?;

        Ok(buffer)
    }

    /// Remove a file from the remote server
    pub fn remove_file(&self, remote_path: &Path) -> Result<()> {
        self.sftp.unlink(remote_path).map_err(|e| {
            SftpError::RemoteFileError(
                remote_path.to_path_buf(),
                format!("Failed to remove file: {}", e),
            )
        })
    }

    /// Close the SFTP connection
    pub fn disconnect(self) -> Result<()> {
        drop(self.sftp);
        self.session.disconnect(None, "Closing connection", None)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sftp_config_creation() {
        let config = SftpConfig::with_password(
            "localhost".to_string(),
            22,
            "user".to_string(),
            "pass".to_string(),
        );

        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 22);
        assert_eq!(config.username, "user");
    }

    #[test]
    fn test_upload_options_default() {
        let opts = UploadOptions::default();
        assert_eq!(opts.buffer_size, 64 * 1024);
        assert!(opts.atomic);
        assert!(opts.verify_size);
        assert_eq!(opts.permissions, Some(0o644));
    }

    #[test]
    fn test_error_display() {
        let err = SftpError::ConnectionFailed("timeout".to_string());
        assert!(err.to_string().contains("Connection failed"));

        let err = SftpError::SizeMismatch {
            expected: 1000,
            actual: 900,
        };
        assert!(err.to_string().contains("1000"));
        assert!(err.to_string().contains("900"));
    }
}
