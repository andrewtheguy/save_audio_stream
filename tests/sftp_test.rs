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

/// Helper to verify upload was successful and no temporary files are left behind
/// Checks:
/// 1. The final file exists at the target path
/// 2. The final file has the expected size
/// 3. No temp file is left behind
fn verify_upload_success(client: &SftpClient, remote_path: &Path, expected_size: u64) {
    let remote_dir = remote_path.parent().unwrap_or(Path::new("."));
    let expected_temp_name = format!(
        "{}.tmpupload",
        remote_path.file_name().unwrap().to_str().unwrap()
    );
    let expected_final_name = remote_path.file_name().unwrap().to_str().unwrap();

    let files = client
        .list_files(remote_dir)
        .expect("Failed to list remote directory");

    println!("Files in directory: {:?}", files);
    println!("Expected final file: {}", expected_final_name);
    println!("Expected temp file: {}", expected_temp_name);

    // Check 1: Verify temp file doesn't exist
    let has_temp_file = files.iter().any(|f| f == &expected_temp_name);
    if has_temp_file {
        println!("⚠️  Temp file found: {}", expected_temp_name);
    }
    assert!(
        !has_temp_file,
        "Found temp file '{}' after upload (should have been renamed)",
        expected_temp_name
    );
    println!("✓ No temp file '{}' left behind", expected_temp_name);

    // Check 2: Verify final file exists
    let has_final_file = files.iter().any(|f| f == expected_final_name);
    assert!(
        has_final_file,
        "Final file '{}' not found in directory (upload may have failed to rename)",
        expected_final_name
    );
    println!("✓ Final file '{}' exists", expected_final_name);

    // Check 3: Verify file size
    let stat = client
        .stat(remote_path)
        .expect("Failed to stat final file");
    let actual_size = stat.size.unwrap_or(0);
    assert_eq!(
        actual_size, expected_size,
        "File size mismatch: expected {} bytes, got {} bytes",
        expected_size, actual_size
    );
    println!("✓ File size verified: {} bytes", actual_size);
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

    // Verify upload success and no temp files left behind
    verify_upload_success(&client, remote_path, 1024);

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

    assert!(options.atomic, "Atomic mode should be enabled by default");

    options.progress_callback = Some(progress_callback);

    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to upload large file");

    println!("✓ Large file uploaded successfully");

    // Verify upload success and no temp files left behind
    verify_upload_success(&client, remote_path, 10 * 1024 * 1024);

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

    // Verify upload success and no temp files left behind
    verify_upload_success(&client, remote_path, 2048);

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

    // Verify upload success and no temp files left behind (should be none for non-atomic)
    verify_upload_success(&client, remote_path, 512);

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
fn test_sftp_upload_atomic() {
    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_atomic.txt");
    create_test_file(&local_file, 4096).unwrap(); // 4KB file

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload file with atomic mode enabled (default)
    let remote_path = Path::new("test/atomic_file.txt");
    let mut options = UploadOptions::default();
    assert!(options.atomic, "Atomic mode should be enabled by default");
    options.verify_size = true; // Ensure size verification is enabled

    // First upload
    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to upload file atomically");

    println!("✓ Atomic upload completed successfully");

    // Verify upload success and no temp files left behind
    verify_upload_success(&client, remote_path, 4096);

    // Verify file exists and has correct size by re-uploading to same path
    // This tests that atomic rename worked correctly
    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to re-upload file atomically (should overwrite)");

    println!("✓ Atomic re-upload (overwrite) successful");

    // Verify again that upload succeeded and no temp files exist after re-upload
    verify_upload_success(&client, remote_path, 4096);

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
