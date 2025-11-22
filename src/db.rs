use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Open a file-based database connection for production use
/// Enables WAL mode and foreign keys
pub fn open_database_connection(
    output_dir: &str,
    name: &str,
) -> Result<Connection, Box<dyn std::error::Error>> {
    let db_path = format!("{}/{}.sqlite", output_dir, name);
    let conn = Connection::open(&db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    println!("SQLite database: {}", db_path);
    Ok(conn)
}

/// Open a database connection with a full path (for sync operations)
/// Enables WAL mode and foreign keys
pub fn open_database_with_path(
    db_path: &Path,
) -> Result<Connection, Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Open a read-only database connection (for web server handlers)
/// Uses explicit read-only mode for safety
/// Foreign keys are not enabled as no modifications are allowed
pub fn open_readonly_connection(
    db_path: impl AsRef<Path>,
) -> Result<Connection, Box<dyn std::error::Error>> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    Ok(conn)
}

/// Create an in-memory database connection for testing
/// Enables foreign keys for CASCADE delete testing
pub fn create_test_connection_in_memory() -> Connection {
    let conn = Connection::open_in_memory().expect("Failed to create in-memory database");
    conn.execute("PRAGMA foreign_keys = ON", [])
        .expect("Failed to enable foreign keys");
    conn
}

/// Open a file-based database connection for test verification
/// Enables foreign keys for verification queries
pub fn open_test_connection(db_path: &Path) -> Connection {
    let conn = Connection::open(db_path).expect("Failed to open test database");
    conn.execute("PRAGMA foreign_keys = ON", [])
        .expect("Failed to enable foreign keys");
    conn
}
