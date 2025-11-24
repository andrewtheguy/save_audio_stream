use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use url::Url;

/// Get the database path for a given output directory and name
pub fn get_db_path(output_dir: &Path, name: &str) -> PathBuf {
    output_dir.join(format!("{}.sqlite", name))
}

/// Open a database connection with a full path (for read-write access)
/// Enables WAL mode and foreign keys
pub fn open_database_connection(db_path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Open a read-only database connection
/// Uses explicit read-only mode for safety
/// Foreign keys are not enabled as no modifications are allowed
/// The `immutable` parameter controls whether immutable=1 is set.
///
/// WARNING: Only set immutable=true for databases on read-only media or network filesystems
/// where the database cannot be modified. Setting immutable on a database that can change
/// will cause SQLITE_CORRUPT errors or incorrect query results.
/// See: https://www.sqlite.org/uri.html#uriimmutable
fn open_readonly_connection_with_options(
    db_path: impl AsRef<Path>,
    immutable: bool,
) -> Result<Connection, Box<dyn std::error::Error>> {
    // Convert to absolute path if needed (from_file_path requires absolute paths)
    let abs_path = if db_path.as_ref().is_absolute() {
        db_path.as_ref().to_path_buf()
    } else {
        std::env::current_dir()?.join(db_path.as_ref())
    };

    let mut uri = Url::from_file_path(&abs_path)
        .map_err(|_| format!("unable to convert path {:?} to file URI", abs_path))?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro");

    if immutable {
        uri.query_pairs_mut()
            .append_pair("immutable", "1");
    }

    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(uri.as_str(), flags)?;
    Ok(conn)
}

/// Open a read-only database connection without immutable flag
/// Use this for databases that may be actively written to (have WAL files)
pub fn open_readonly_connection(
    db_path: impl AsRef<Path>,
) -> Result<Connection, Box<dyn std::error::Error>> {
    open_readonly_connection_with_options(db_path, false)
}

/// Open a read-only database connection with immutable=1 flag
///
/// WARNING: Only use this for databases on read-only media or network filesystems
/// where the database file cannot be changed by ANY process. Using immutable mode
/// on a database that can be modified will cause SQLITE_CORRUPT errors or incorrect
/// query results. This disables all locking and change detection.
///
/// See: https://www.sqlite.org/uri.html#uriimmutable
pub fn open_readonly_connection_immutable(
    db_path: impl AsRef<Path>,
) -> Result<Connection, Box<dyn std::error::Error>> {
    open_readonly_connection_with_options(db_path, true)
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

/// Initialize database schema (tables and indexes)
/// This consolidates DDL operations used across the codebase
pub fn init_database_schema(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    // Create tables
    conn.execute(
        "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sections (
            id INTEGER PRIMARY KEY,
            start_timestamp_ms INTEGER NOT NULL,
            is_exported_to_remote INTEGER
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
            audio_data BLOB NOT NULL,
            section_id INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE,
            duration_samples INTEGER NOT NULL
        )",
        [],
    )?;

    // Create indexes for efficient queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_segments_boundary
         ON segments(is_timestamp_from_source, timestamp_ms)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_segments_section_id
         ON segments(section_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sections_start_timestamp
         ON sections(start_timestamp_ms)",
        [],
    )?;

    // Enable WAL mode
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    Ok(())
}
