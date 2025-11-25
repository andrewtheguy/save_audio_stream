use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{Executor, Row, Transaction};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::runtime::Runtime;

use crate::queries::{ddl, metadata, sections, segments};

// Re-export SqlitePool for convenience
pub use sqlx::sqlite::SqlitePool as Pool;

type DynError = Box<dyn std::error::Error + Send + Sync>;

/// Synchronous database wrapper that owns a runtime for blocking operations.
/// Follows the rust-postgres pattern of embedding runtime in the connection.
/// This avoids creating a new Runtime for every sync operation.
pub struct SyncDb {
    pool: SqlitePool,
    runtime: Runtime,
}

impl SyncDb {
    /// Open a read-write database connection with embedded runtime
    pub fn connect(db_path: &Path) -> Result<Self, DynError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let pool = runtime.block_on(open_database_connection(db_path))?;
        Ok(Self { pool, runtime })
    }

    /// Open a read-only database connection with embedded runtime
    pub fn connect_readonly(db_path: impl AsRef<Path>) -> Result<Self, DynError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let pool = runtime.block_on(open_readonly_connection(db_path))?;
        Ok(Self { pool, runtime })
    }

    /// Open a read-only immutable database connection with embedded runtime
    pub fn connect_readonly_immutable(db_path: impl AsRef<Path>) -> Result<Self, DynError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let pool = runtime.block_on(open_readonly_connection_immutable(db_path))?;
        Ok(Self { pool, runtime })
    }

    /// Block on an async future using the embedded runtime
    pub fn block_on<F, T>(&self, fut: F) -> Result<T, DynError>
    where
        F: Future<Output = Result<T, DynError>>,
    {
        self.runtime.block_on(fut)
    }

    /// Get a reference to the underlying pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Get the database path for a given output directory and name
pub fn get_db_path(output_dir: &Path, name: &str) -> PathBuf {
    output_dir.join(format!("{}.sqlite", name))
}

/// Open a database connection pool with a full path (for read-write access)
/// Enables WAL mode and foreign keys
pub async fn open_database_connection(db_path: &Path) -> Result<SqlitePool, DynError> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let options = SqliteConnectOptions::from_str(&db_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Open a read-only database connection pool
/// Uses explicit read-only mode for safety
/// Foreign keys are not enabled as no modifications are allowed
pub async fn open_readonly_connection(db_path: impl AsRef<Path>) -> Result<SqlitePool, DynError> {
    let db_url = format!("sqlite://{}?mode=ro", db_path.as_ref().display());

    let options = SqliteConnectOptions::from_str(&db_url)?.read_only(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Open a read-only database connection pool with immutable flag
///
/// WARNING: Only use this for databases on read-only media or network filesystems
/// where the database file cannot be changed by ANY process. Using immutable mode
/// on a database that can be modified will cause SQLITE_CORRUPT errors or incorrect
/// query results. This disables all locking and change detection.
///
/// See: https://www.sqlite.org/uri.html#uriimmutable
pub async fn open_readonly_connection_immutable(
    db_path: impl AsRef<Path>,
) -> Result<SqlitePool, DynError> {
    let db_url = format!(
        "sqlite://{}?mode=ro&immutable=1",
        db_path.as_ref().display()
    );

    let options = SqliteConnectOptions::from_str(&db_url)?
        .read_only(true)
        .immutable(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Create a temporary file-backed database connection pool for testing
/// Enables foreign keys for CASCADE delete testing.
/// Returns (pool, guard) - the guard must be kept alive to prevent the temp file from being deleted.
#[allow(dead_code)]
pub async fn create_test_connection_in_temporary_file(
) -> Result<(SqlitePool, tempfile::TempDir), DynError> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("test.sqlite");
    let dsn = format!("sqlite://{}", db_path.display());

    let options = SqliteConnectOptions::from_str(&dsn)?
        .foreign_keys(true)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    Ok((pool, temp_dir))
}

/// Update or insert a metadata key-value pair
/// Uses INSERT OR REPLACE to handle both new and existing keys
pub async fn upsert_metadata<'e, E>(executor: E, key: &str, value: &str) -> Result<(), DynError>
where
    E: Executor<'e, Database = sqlx::Sqlite>,
{
    let sql = metadata::upsert(key, value);
    sqlx::query(&sql).execute(executor).await?;
    Ok(())
}

/// Initialize database schema (tables and indexes)
/// This consolidates DDL operations used across the codebase
pub async fn init_database_schema(pool: &SqlitePool) -> Result<(), DynError> {
    // Create tables using SeaQuery DDL
    sqlx::query(&ddl::create_metadata_table())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_sections_table())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_segments_table())
        .execute(pool)
        .await?;

    // Create indexes
    sqlx::query(&ddl::create_segments_boundary_index())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_segments_section_id_index())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_sections_start_timestamp_index())
        .execute(pool)
        .await?;

    // Enable WAL mode (PRAGMA - raw SQL since SeaQuery doesn't support it)
    sqlx::query("PRAGMA journal_mode=WAL").execute(pool).await?;

    Ok(())
}

/// Query a single metadata value by key
pub async fn query_metadata<'e, E>(executor: E, key: &str) -> Result<Option<String>, DynError>
where
    E: Executor<'e, Database = sqlx::Sqlite>,
{
    let sql = metadata::select_by_key(key);
    let result = sqlx::query(&sql).fetch_optional(executor).await?;

    Ok(result.map(|row| row.get::<String, _>(0)))
}

/// Insert a new metadata key-value pair
pub async fn insert_metadata<'e, E>(executor: E, key: &str, value: &str) -> Result<(), DynError>
where
    E: Executor<'e, Database = sqlx::Sqlite>,
{
    let sql = metadata::insert(key, value);
    sqlx::query(&sql).execute(executor).await?;
    Ok(())
}

/// Execute a raw SQL statement and return the number of rows affected
pub async fn execute(pool: &SqlitePool, sql: &str) -> Result<u64, DynError> {
    let result = sqlx::query(sql).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Query a single optional row value (scalar)
pub async fn query_one_optional<T>(pool: &SqlitePool, sql: &str) -> Result<Option<T>, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    let result = sqlx::query_scalar::<_, T>(sql).fetch_optional(pool).await?;
    Ok(result)
}

/// Query a single row (scalar)
pub async fn query_one<T>(pool: &SqlitePool, sql: &str) -> Result<T, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    let result = sqlx::query_scalar::<_, T>(sql).fetch_one(pool).await?;
    Ok(result)
}

/// Insert a section row
pub async fn insert_section(
    pool: &SqlitePool,
    id: i64,
    start_timestamp_ms: i64,
) -> Result<(), DynError> {
    let sql = sections::insert(id, start_timestamp_ms);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Insert a section row if it does not already exist
pub async fn insert_section_or_ignore(
    pool: &SqlitePool,
    id: i64,
    start_timestamp_ms: i64,
) -> Result<(), DynError> {
    let sql = sections::insert_or_ignore(id, start_timestamp_ms);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Delete sections older than the cutoff while keeping the specified id
pub async fn delete_old_sections(
    pool: &SqlitePool,
    cutoff_ms: i64,
    keeper_section_id: i64,
) -> Result<u64, DynError> {
    let sql = sections::delete_old_sections(cutoff_ms, keeper_section_id);
    let result = sqlx::query(&sql).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Insert a segment row
pub async fn insert_segment(
    pool: &SqlitePool,
    timestamp_ms: i64,
    is_timestamp_from_source: bool,
    section_id: i64,
    audio_data: &[u8],
    duration_samples: i64,
) -> Result<(), DynError> {
    let sql = segments::insert(
        timestamp_ms,
        is_timestamp_from_source,
        section_id,
        audio_data,
        duration_samples,
    );
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Insert a segment row with an explicit id (used by sync)
pub async fn insert_segment_with_id(
    pool: &SqlitePool,
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    audio_data: &[u8],
    section_id: i64,
    duration_samples: i64,
) -> Result<(), DynError> {
    let sql = segments::insert_with_id(
        id,
        timestamp_ms,
        is_timestamp_from_source,
        audio_data,
        section_id,
        duration_samples,
    );
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Check if segments exist for a section id
pub async fn segments_exist_for_section(
    pool: &SqlitePool,
    section_id: i64,
) -> Result<bool, DynError> {
    let sql = segments::exists_for_section(section_id);
    let result: Option<i32> = sqlx::query_scalar(&sql).fetch_optional(pool).await?;
    Ok(result.map(|v| v != 0).unwrap_or(false))
}

/// Update a metadata key to a new value
pub async fn update_metadata(pool: &SqlitePool, key: &str, value: &str) -> Result<(), DynError> {
    let sql = metadata::update(key, value);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Determine whether a metadata key exists
pub async fn metadata_exists(pool: &SqlitePool, key: &str) -> Result<bool, DynError> {
    let sql = metadata::exists(key);
    let result: Option<i32> = sqlx::query_scalar(&sql).fetch_optional(pool).await?;
    Ok(result.is_some())
}

/// Get the latest section id before a cutoff timestamp
pub async fn get_latest_section_before_cutoff(
    pool: &SqlitePool,
    cutoff_ms: i64,
) -> Result<Option<i64>, DynError> {
    let sql = sections::select_latest_before_cutoff(cutoff_ms);
    let result: Option<i64> = sqlx::query_scalar(&sql).fetch_optional(pool).await?;
    Ok(result)
}

/// Run multiple operations inside a transaction from async code
pub async fn with_transaction<F, Fut, T>(pool: &SqlitePool, f: F) -> Result<T, DynError>
where
    F: FnOnce(&mut Transaction<'_, sqlx::Sqlite>) -> Fut,
    Fut: Future<Output = Result<T, DynError>>,
{
    let mut tx = pool.begin().await?;
    let result = f(&mut tx).await;

    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(err) => {
            tx.rollback().await?;
            Err(err)
        }
    }
}

// ============================================================================
// Sync wrapper functions for use in blocking code (record.rs, sync.rs)
// These use SyncDb's embedded runtime to block on async operations
// ============================================================================

/// Sync wrapper: Initialize database schema
pub fn init_database_schema_sync(db: &SyncDb) -> Result<(), DynError> {
    db.block_on(init_database_schema(db.pool()))
}

/// Sync wrapper: Query metadata
pub fn query_metadata_sync(db: &SyncDb, key: &str) -> Result<Option<String>, DynError> {
    db.block_on(query_metadata(db.pool(), key))
}

/// Sync wrapper: Insert metadata
pub fn insert_metadata_sync(db: &SyncDb, key: &str, value: &str) -> Result<(), DynError> {
    db.block_on(insert_metadata(db.pool(), key, value))
}

/// Sync wrapper: Upsert metadata
pub fn upsert_metadata_sync(db: &SyncDb, key: &str, value: &str) -> Result<(), DynError> {
    db.block_on(upsert_metadata(db.pool(), key, value))
}

/// Sync wrapper: Execute a raw SQL query
pub fn execute_sync(db: &SyncDb, sql: &str) -> Result<u64, DynError> {
    db.block_on(execute(db.pool(), sql))
}

/// Sync wrapper: Query a single optional row value
pub fn query_one_optional_sync<T>(db: &SyncDb, sql: &str) -> Result<Option<T>, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    db.block_on(query_one_optional(db.pool(), sql))
}

/// Sync wrapper: Query a single row (returns error if not found)
pub fn query_one_sync<T>(db: &SyncDb, sql: &str) -> Result<T, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    db.block_on(query_one(db.pool(), sql))
}

/// Sync wrapper: Insert a section
pub fn insert_section_sync(db: &SyncDb, id: i64, start_timestamp_ms: i64) -> Result<(), DynError> {
    db.block_on(insert_section(db.pool(), id, start_timestamp_ms))
}

/// Sync wrapper: Insert or ignore a section (for sync)
pub fn insert_section_or_ignore_sync(
    db: &SyncDb,
    id: i64,
    start_timestamp_ms: i64,
) -> Result<(), DynError> {
    db.block_on(insert_section_or_ignore(db.pool(), id, start_timestamp_ms))
}

/// Sync wrapper: Delete old sections
pub fn delete_old_sections_sync(
    db: &SyncDb,
    cutoff_ms: i64,
    keeper_section_id: i64,
) -> Result<u64, DynError> {
    db.block_on(delete_old_sections(db.pool(), cutoff_ms, keeper_section_id))
}

/// Sync wrapper: Insert a segment
pub fn insert_segment_sync(
    db: &SyncDb,
    timestamp_ms: i64,
    is_timestamp_from_source: bool,
    section_id: i64,
    audio_data: &[u8],
    duration_samples: i64,
) -> Result<(), DynError> {
    db.block_on(insert_segment(
        db.pool(),
        timestamp_ms,
        is_timestamp_from_source,
        section_id,
        audio_data,
        duration_samples,
    ))
}

/// Sync wrapper: Insert a segment with explicit ID (for sync)
pub fn insert_segment_with_id_sync(
    db: &SyncDb,
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    audio_data: &[u8],
    section_id: i64,
    duration_samples: i64,
) -> Result<(), DynError> {
    db.block_on(insert_segment_with_id(
        db.pool(),
        id,
        timestamp_ms,
        is_timestamp_from_source,
        audio_data,
        section_id,
        duration_samples,
    ))
}

/// Sync wrapper: Check if segments exist for a section
pub fn segments_exist_for_section_sync(db: &SyncDb, section_id: i64) -> Result<bool, DynError> {
    db.block_on(segments_exist_for_section(db.pool(), section_id))
}

/// Sync wrapper: Update metadata
pub fn update_metadata_sync(db: &SyncDb, key: &str, value: &str) -> Result<(), DynError> {
    db.block_on(update_metadata(db.pool(), key, value))
}

/// Sync wrapper: Check if metadata key exists
pub fn metadata_exists_sync(db: &SyncDb, key: &str) -> Result<bool, DynError> {
    db.block_on(metadata_exists(db.pool(), key))
}

/// Sync wrapper: Get latest section before cutoff
pub fn get_latest_section_before_cutoff_sync(
    db: &SyncDb,
    cutoff_ms: i64,
) -> Result<Option<i64>, DynError> {
    db.block_on(get_latest_section_before_cutoff(db.pool(), cutoff_ms))
}

/// Sync wrapper for running multiple operations in a transaction
pub fn with_transaction_sync<F, T>(db: &SyncDb, f: F) -> Result<T, DynError>
where
    F: FnOnce(&mut Transaction<'_, sqlx::Sqlite>) -> Result<T, DynError>,
{
    db.block_on(async {
        let mut tx = db.pool().begin().await?;
        let result = f(&mut tx);

        match result {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(err) => {
                tx.rollback().await?;
                Err(err)
            }
        }
    })
}
