use save_audio_stream::sftp::{SftpClient, SftpConfig, UploadOptions};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

/// Helper to create a test file with given size
fn create_test_file(path: &Path, size_bytes: usize) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    let data = vec![b'A'; size_bytes];
    file.write_all(&data)?;
    Ok(())
}

/// Progress callback for testing
fn progress_callback(uploaded: u64, total: u64) {
    println!(
        "Upload progress: {}/{} bytes ({:.1}%)",
        uploaded,
        total,
        (uploaded as f64 / total as f64) * 100.0
    );
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_small_file() {
    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_small.txt");
    create_test_file(&local_file, 1024).unwrap(); // 1KB file

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload file with default options
    let remote_path = Path::new("test/small_file.txt");
    let options = UploadOptions::default();

    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to upload file");

    println!("✓ Small file uploaded successfully");

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_large_file() {
    // Create a temporary directory and large file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_large.bin");
    create_test_file(&local_file, 10 * 1024 * 1024).unwrap(); // 10MB file

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload file with progress callback
    let remote_path = Path::new("test/large_file.bin");
    let mut options = UploadOptions::default();
    options.progress_callback = Some(progress_callback);

    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to upload large file");

    println!("✓ Large file uploaded successfully");

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_nested_directory() {
    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_nested.txt");
    create_test_file(&local_file, 2048).unwrap(); // 2KB file

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload file to nested directory (will create directories automatically)
    let remote_path = Path::new("test/level1/level2/level3/nested_file.txt");
    let options = UploadOptions::default();

    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to upload to nested directory");

    println!("✓ File uploaded to nested directory successfully");

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_non_atomic() {
    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_non_atomic.txt");
    create_test_file(&local_file, 512).unwrap();

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload file without atomic mode
    let remote_path = Path::new("test/non_atomic_file.txt");
    let mut options = UploadOptions::default();
    options.atomic = false;

    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to upload non-atomically");

    println!("✓ Non-atomic upload successful");

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_mkdir_p() {
    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Create nested directories
    let dir_path = Path::new("test/mkdir_test/level1/level2/level3");
    client
        .mkdir_p(dir_path, 0o755)
        .expect("Failed to create directories");

    println!("✓ Nested directories created successfully");

    // Try creating the same path again (should succeed - idempotent)
    client
        .mkdir_p(dir_path, 0o755)
        .expect("Failed to create directories (second time)");

    println!("✓ Directory creation is idempotent");

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_connection_failure() {
    // Try to connect to non-existent server
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        9999, // Non-existent port
        "demo".to_string(),
        "demo".to_string(),
    );

    let result = SftpClient::connect(&config);
    assert!(result.is_err(), "Should fail to connect to non-existent server");

    if let Err(e) = result {
        println!("✓ Expected connection error: {}", e);
    }
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_auth_failure() {
    // Try to connect with wrong password
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "wrong_password".to_string(),
    );

    let result = SftpClient::connect(&config);
    assert!(result.is_err(), "Should fail with wrong password");

    if let Err(e) = result {
        println!("✓ Expected authentication error: {}", e);
    }
}
