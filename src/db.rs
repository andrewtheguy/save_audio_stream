use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Get the database path for a given output directory and name
pub fn get_db_path(output_dir: &str, name: &str) -> String {
    format!("{}/{}.sqlite", output_dir, name)
}

/// Open a database connection with a full path (for read-write access)
/// Enables WAL mode and foreign keys
pub fn open_database_connection(
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
#[allow(dead_code)]
pub fn create_test_connection_in_memory() -> Connection {
    let conn = Connection::open_in_memory().expect("Failed to create in-memory database");
    conn.execute("PRAGMA foreign_keys = ON", [])
        .expect("Failed to enable foreign keys");
    conn
}

/// Open a file-based database connection for test verification
/// Enables foreign keys for verification queries
#[allow(dead_code)]
pub fn open_test_connection(db_path: &Path) -> Connection {
    let conn = Connection::open(db_path).expect("Failed to open test database");
    conn.execute("PRAGMA foreign_keys = ON", [])
        .expect("Failed to enable foreign keys");
    conn
}

/// Update or insert a metadata key-value pair
/// Uses INSERT OR REPLACE to handle both new and existing keys
pub fn upsert_metadata(
    conn: &Connection,
    key: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
        [key, value],
    )?;
    Ok(())
}
