use save_audio_stream::sftp::{SftpClient, SftpConfig, UploadOptions};
use std::io::Cursor;
use std::path::Path;
use tempfile::TempDir;

/// Progress callback for testing
fn progress_callback(uploaded: u64, total: u64) {
    println!(
        "Upload progress: {}/{} bytes ({:.1}%)",
        uploaded,
        total,
        (uploaded as f64 / total as f64) * 100.0
    );
}

/// Helper to verify upload was successful with mandatory CRC32 validation
/// Checks:
/// 1. The final file exists at the target path
/// 2. The final file has the expected size
/// 3. No temp file is left behind
/// 4. CRC32 checksum matches original data
fn verify_upload_success(
    client: &SftpClient,
    remote_path: &Path,
    expected_size: u64,
    expected_data: &[u8],
) {
    use crc32fast::Hasher;

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

    // Check 4: Download and verify CRC32 checksum
    let downloaded = client
        .download_file(remote_path)
        .expect("Failed to download file for CRC32 validation");

    // Calculate expected CRC32
    let mut expected_hasher = Hasher::new();
    expected_hasher.update(expected_data);
    let expected_crc32 = expected_hasher.finalize();

    // Calculate actual CRC32
    let mut actual_hasher = Hasher::new();
    actual_hasher.update(&downloaded);
    let actual_crc32 = actual_hasher.finalize();

    println!("Expected CRC32: 0x{:08X}", expected_crc32);
    println!("Actual CRC32:   0x{:08X}", actual_crc32);

    assert_eq!(
        actual_crc32, expected_crc32,
        "CRC32 checksum mismatch! Data corruption detected."
    );
    println!("✓ CRC32 checksum verified");
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_small_file() {
    // Create test data
    let test_data = vec![b'A'; 1024]; // 1KB file

    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_small.txt");
    std::fs::write(&local_file, &test_data).unwrap();

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

    // Verify upload success with CRC32 validation
    verify_upload_success(&client, remote_path, 1024, &test_data);

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_large_file() {
    // Create large test data (10MB)
    let size = 10 * 1024 * 1024;
    let test_data = vec![b'A'; size];

    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_large.bin");
    std::fs::write(&local_file, &test_data).unwrap();

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

    // Verify upload success with CRC32 validation
    verify_upload_success(&client, remote_path, size as u64, &test_data);

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_nested_directory() {
    // Create test data
    let test_data = vec![b'A'; 2048]; // 2KB file

    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_nested.txt");
    std::fs::write(&local_file, &test_data).unwrap();

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

    // Verify upload success with CRC32 validation
    verify_upload_success(&client, remote_path, 2048, &test_data);

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_multiple_files_nested_directory() {
    // Create two different test files
    let test_data_1: Vec<u8> = (0..1024)
        .map(|i| ((i * 3 + 5) % 256) as u8)
        .collect();
    let test_data_2: Vec<u8> = (0..2048)
        .map(|i| ((i * 7 + 11) % 256) as u8)
        .collect();

    // Create temporary files
    let temp_dir = TempDir::new().unwrap();
    let local_file_1 = temp_dir.path().join("file1.bin");
    let local_file_2 = temp_dir.path().join("file2.bin");
    std::fs::write(&local_file_1, &test_data_1).unwrap();
    std::fs::write(&local_file_2, &test_data_2).unwrap();

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload both files to the same nested directory
    let remote_path_1 = Path::new("test/multi/level1/level2/first_file.bin");
    let remote_path_2 = Path::new("test/multi/level1/level2/second_file.bin");
    let options = UploadOptions::default();

    client
        .upload_file(&local_file_1, remote_path_1, &options)
        .expect("Failed to upload first file");

    println!("✓ First file uploaded to nested directory");

    client
        .upload_file(&local_file_2, remote_path_2, &options)
        .expect("Failed to upload second file");

    println!("✓ Second file uploaded to same nested directory");

    // Verify both uploads with CRC32 validation
    verify_upload_success(&client, remote_path_1, 1024, &test_data_1);
    verify_upload_success(&client, remote_path_2, 2048, &test_data_2);

    // Verify both files exist in the same directory
    let remote_dir = remote_path_1.parent().unwrap();
    let files = client
        .list_files(remote_dir)
        .expect("Failed to list directory");

    println!("Files in nested directory: {:?}", files);

    assert!(
        files.contains(&"first_file.bin".to_string()),
        "First file not found in directory"
    );
    assert!(
        files.contains(&"second_file.bin".to_string()),
        "Second file not found in directory"
    );
    assert_eq!(files.len(), 2, "Expected exactly 2 files in directory");

    println!("✓ Both files verified in nested directory");

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_non_atomic() {
    // Create test data
    let test_data = vec![b'A'; 512];

    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_non_atomic.txt");
    std::fs::write(&local_file, &test_data).unwrap();

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

    // Verify upload success with CRC32 validation (should be none for non-atomic)
    verify_upload_success(&client, remote_path, 512, &test_data);

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
    // Create test data
    let test_data = vec![b'A'; 4096]; // 4KB file

    // Create a temporary directory and file
    let temp_dir = TempDir::new().unwrap();
    let local_file = temp_dir.path().join("test_atomic.txt");
    std::fs::write(&local_file, &test_data).unwrap();

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

    // Verify upload success with CRC32 validation
    verify_upload_success(&client, remote_path, 4096, &test_data);

    // Verify file exists and has correct size by re-uploading to same path
    // This tests that atomic rename worked correctly
    client
        .upload_file(&local_file, remote_path, &options)
        .expect("Failed to re-upload file atomically (should overwrite)");

    println!("✓ Atomic re-upload (overwrite) successful");

    // Verify again that upload succeeded with CRC32 validation after re-upload
    verify_upload_success(&client, remote_path, 4096, &test_data);

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

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_from_memory() {
    // Create test data in memory
    let test_data = b"Hello, SFTP! This is data uploaded from memory.";
    let data_size = test_data.len() as u64;

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload from memory using Cursor
    let remote_path = Path::new("test/memory_upload.txt");
    let mut cursor = Cursor::new(test_data);
    let options = UploadOptions::default();

    client
        .upload_stream(&mut cursor, remote_path, data_size, &options)
        .expect("Failed to upload from memory");

    println!("✓ Data uploaded from memory successfully");

    // Verify upload success with CRC32 validation
    verify_upload_success(&client, remote_path, data_size, test_data);

    // Cleanup
    client.disconnect().unwrap();
}

#[test]
#[ignore] // Requires SFTP server running on localhost:2222
fn test_sftp_upload_large_data_from_memory() {
    // Create 5MB of test data
    let size = 5 * 1024 * 1024;
    let test_data: Vec<u8> = (0..size)
        .map(|i| ((i * 31 + 17) % 256) as u8)
        .collect();

    println!("Large data size: {} bytes ({} MB)", size, size / 1024 / 1024);

    // Configure SFTP connection
    let config = SftpConfig::with_password(
        "localhost".to_string(),
        2222,
        "demo".to_string(),
        "demo".to_string(),
    );

    // Connect to SFTP server
    let client = SftpClient::connect(&config).expect("Failed to connect");

    // Upload from memory with progress callback
    let remote_path = Path::new("test/large_memory_upload.bin");
    let mut cursor = Cursor::new(&test_data);
    let mut options = UploadOptions::default();
    options.progress_callback = Some(progress_callback);

    client
        .upload_stream(&mut cursor, remote_path, size as u64, &options)
        .expect("Failed to upload large data from memory");

    println!("✓ Large data uploaded from memory successfully");

    // Verify upload success with CRC32 validation
    verify_upload_success(&client, remote_path, size as u64, &test_data);

    // Cleanup
    client.disconnect().unwrap();
}
