use std::error::Error;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use ssh2::{Session, Sftp};
use std::io::Write;

/// Create directory recursively, like `mkdir -p`
fn mkdir_p(sftp: &Sftp, path: &Path) -> Result<(), Box<dyn Error>> {
    let mut current = PathBuf::new();

    for component in path.components() {
        current.push(component);

        // Try to create directory, ignore error if it already exists
        match sftp.mkdir(&current, 0o755) {
            Ok(_) => println!("Created directory: {}", current.display()),
            Err(e) => {
                // Check if directory already exists by trying to stat it
                match sftp.stat(&current) {
                    Ok(stat) => {
                        if stat.is_dir() {
                            println!("Directory already exists: {}", current.display());
                        } else {
                            return Err(format!("Path exists but is not a directory: {}", current.display()).into());
                        }
                    }
                    Err(_) => {
                        // Directory doesn't exist and we couldn't create it
                        return Err(format!("Failed to create directory {}: {}", current.display(), e).into());
                    }
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    println!("Connecting to SFTP server at localhost:1123...");

    let tcp = TcpStream::connect("localhost:1123")?;
    let mut sess = Session::new()?;
    sess.set_tcp_stream(tcp);
    sess.handshake()?;
    sess.userauth_password("demo", "demo")?;

    let sftp = sess.sftp()?;

    // Get current working directory
    let cwd = sftp.realpath(Path::new("."))?;
    println!("Current SFTP directory: {}", cwd.display());

    // Create nested directories like mkdir -p
    let dir_path = Path::new("test/test2");
    println!("\nCreating nested directories: {}", dir_path.display());
    mkdir_p(&sftp, dir_path)?;

    // Create and write file in nested directory
    let file_path = Path::new("test/test2/upload.txt");
    println!("\nCreating file: {}", file_path.display());
    let mut file = sftp.create(file_path)?;

    let content = format!("Test upload at {}", chrono::Local::now());
    file.write_all(content.as_bytes())?;
    println!("File written successfully");

    // Verify file was created
    let stat = sftp.stat(file_path)?;
    println!("File size: {} bytes", stat.size.unwrap_or(0));

    println!("\n✓ SFTP upload test completed successfully!");

    Ok(())
}
