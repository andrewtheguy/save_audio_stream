//! PostgreSQL database module for receiver mode
//!
//! This module provides PostgreSQL-specific database operations for the receiver,
//! mirroring the structure of db.rs but using PgPool instead of SqlitePool.

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Executor, Row, Transaction};
use std::future::Future;
use std::str::FromStr;
use tokio::runtime::Runtime;

use crate::queries::{ddl, metadata, sections, segments};

type DynError = Box<dyn std::error::Error + Send + Sync>;

/// Synchronous PostgreSQL database wrapper that owns a runtime for blocking operations.
/// Follows the rust-postgres pattern of embedding runtime in the connection.
pub struct SyncDbPg {
    pool: PgPool,
    runtime: Runtime,
}

impl SyncDbPg {
    /// Connect to a PostgreSQL database with embedded runtime, creating the database if it doesn't exist
    ///
    /// # Arguments
    /// * `base_url` - Base PostgreSQL URL without database (e.g., postgres://user@host:5432)
    /// * `password` - Password for authentication
    /// * `database` - Database name (e.g., save_audio_am1430)
    pub fn connect(base_url: &str, password: &str, database: &str) -> Result<Self, DynError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let pool = runtime.block_on(open_postgres_connection_create_if_needed(base_url, password, database))?;
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
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Build a full PostgreSQL connection URL with password and database
///
/// Takes a base URL like `postgres://user@host:5432` and inserts the password
/// and appends the database name.
pub fn build_postgres_url(base_url: &str, password: &str, database: &str) -> Result<String, DynError> {
    // Parse the base URL to insert password
    let url = url::Url::parse(base_url)?;

    let user = url.username();
    let host = url.host_str().ok_or("Missing host in postgres_url")?;
    let port = url.port().unwrap_or(5432);

    // Build full URL with password and database
    let full_url = format!(
        "postgres://{}:{}@{}:{}/{}",
        user,
        urlencoding::encode(password),
        host,
        port,
        database
    );

    Ok(full_url)
}

/// Open a PostgreSQL connection pool
pub async fn open_postgres_connection(
    base_url: &str,
    password: &str,
    database: &str,
) -> Result<PgPool, DynError> {
    let full_url = build_postgres_url(base_url, password, database)?;

    let options = PgConnectOptions::from_str(&full_url)?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Create a PostgreSQL database if it doesn't exist
pub async fn create_database_if_not_exists(
    base_url: &str,
    password: &str,
    database: &str,
) -> Result<(), DynError> {
    // Connect to the default 'postgres' database to create the target database
    let admin_url = build_postgres_url(base_url, password, "postgres")?;
    let admin_options = PgConnectOptions::from_str(&admin_url)?;

    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await?;

    // Check if database exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)"
    )
    .bind(database)
    .fetch_one(&admin_pool)
    .await?;

    if !exists {
        // Create the database (note: CREATE DATABASE cannot use prepared statements)
        // We need to use a safe identifier here
        let create_sql = format!("CREATE DATABASE \"{}\"", database.replace('"', "\"\""));
        match sqlx::query(&create_sql).execute(&admin_pool).await {
            Ok(_) => {}
            Err(e) => {
                // Handle race condition: another process may have created the database
                // between our EXISTS check and CREATE. PostgreSQL error code 42P04 means
                // "duplicate_database" - the database already exists.
                let err_str = e.to_string();
                if !err_str.contains("already exists") && !err_str.contains("42P04") {
                    return Err(e.into());
                }
                // Database was created by another process, that's fine
            }
        }
    }

    Ok(())
}

/// Open a PostgreSQL connection pool, creating the database if it doesn't exist
pub async fn open_postgres_connection_create_if_needed(
    base_url: &str,
    password: &str,
    database: &str,
) -> Result<PgPool, DynError> {
    // First ensure the database exists
    create_database_if_not_exists(base_url, password, database).await?;

    // Then connect to it
    open_postgres_connection(base_url, password, database).await
}

/// Drop a PostgreSQL database if it exists
pub async fn drop_database_if_exists(
    base_url: &str,
    password: &str,
    database: &str,
) -> Result<(), DynError> {
    // Connect to the default 'postgres' database to drop the target database
    let admin_url = build_postgres_url(base_url, password, "postgres")?;
    let admin_options = PgConnectOptions::from_str(&admin_url)?;

    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await?;

    // Terminate existing connections to the database
    let terminate_sql = format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()",
        database.replace('\'', "''")
    );
    let _ = sqlx::query(&terminate_sql).execute(&admin_pool).await;

    // Drop the database
    let drop_sql = format!("DROP DATABASE IF EXISTS \"{}\"", database.replace('"', "\"\""));
    sqlx::query(&drop_sql)
        .execute(&admin_pool)
        .await?;

    Ok(())
}

/// Initialize database schema for PostgreSQL
/// Creates tables and indexes using PostgreSQL-specific DDL
pub async fn init_database_schema_pg(pool: &PgPool) -> Result<(), DynError> {
    // Create tables using PostgreSQL DDL
    sqlx::query(&ddl::create_metadata_table_pg())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_sections_table_pg())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_segments_table_pg())
        .execute(pool)
        .await?;

    // Create indexes
    sqlx::query(&ddl::create_segments_boundary_index_pg())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_segments_section_id_index_pg())
        .execute(pool)
        .await?;
    sqlx::query(&ddl::create_sections_start_timestamp_index_pg())
        .execute(pool)
        .await?;

    // No PRAGMA needed for PostgreSQL - it uses MVCC by default

    Ok(())
}

/// Update or insert a metadata key-value pair (PostgreSQL version)
pub async fn upsert_metadata_pg<'e, E>(
    executor: E,
    key: &str,
    value: &str,
) -> Result<(), DynError>
where
    E: Executor<'e, Database = sqlx::Postgres>,
{
    let sql = metadata::upsert_pg(key, value);
    sqlx::query(&sql).execute(executor).await?;
    Ok(())
}

/// Query a single metadata value by key (PostgreSQL version)
pub async fn query_metadata_pg<'e, E>(
    executor: E,
    key: &str,
) -> Result<Option<String>, DynError>
where
    E: Executor<'e, Database = sqlx::Postgres>,
{
    let sql = metadata::select_by_key_pg(key);
    let result = sqlx::query(&sql)
        .fetch_optional(executor)
        .await?;

    Ok(result.map(|row| row.get::<String, _>(0)))
}

/// Insert a new metadata key-value pair (PostgreSQL version)
pub async fn insert_metadata_pg<'e, E>(
    executor: E,
    key: &str,
    value: &str,
) -> Result<(), DynError>
where
    E: Executor<'e, Database = sqlx::Postgres>,
{
    let sql = metadata::insert_pg(key, value);
    sqlx::query(&sql).execute(executor).await?;
    Ok(())
}

/// Execute a raw SQL statement (PostgreSQL version)
pub async fn execute_pg(pool: &PgPool, sql: &str) -> Result<u64, DynError> {
    let result = sqlx::query(sql).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Query a single optional row value (PostgreSQL version)
pub async fn query_one_optional_pg<T>(pool: &PgPool, sql: &str) -> Result<Option<T>, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    let result = sqlx::query_scalar::<_, T>(sql)
        .fetch_optional(pool)
        .await?;
    Ok(result)
}

/// Query a single row (PostgreSQL version)
pub async fn query_one_pg<T>(pool: &PgPool, sql: &str) -> Result<T, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    let result = sqlx::query_scalar::<_, T>(sql)
        .fetch_one(pool)
        .await?;
    Ok(result)
}

/// Insert a section row (PostgreSQL version)
pub async fn insert_section_pg(pool: &PgPool, id: i64, start_timestamp_ms: i64) -> Result<(), DynError> {
    let sql = sections::insert_pg(id, start_timestamp_ms);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Insert a section row if it does not already exist (PostgreSQL version)
pub async fn insert_section_or_ignore_pg(pool: &PgPool, id: i64, start_timestamp_ms: i64) -> Result<(), DynError> {
    let sql = sections::insert_or_ignore_pg(id, start_timestamp_ms);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Delete sections older than the cutoff while keeping the specified id (PostgreSQL version)
pub async fn delete_old_sections_pg(pool: &PgPool, cutoff_ms: i64, keeper_section_id: i64) -> Result<u64, DynError> {
    let sql = sections::delete_old_sections_pg(cutoff_ms, keeper_section_id);
    let result = sqlx::query(&sql).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Insert a segment row with explicit ID (PostgreSQL version, used by sync)
pub async fn insert_segment_with_id_pg(
    pool: &PgPool,
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    audio_data: &[u8],
    section_id: i64,
    duration_samples: i64,
) -> Result<(), DynError> {
    let sql = segments::insert_with_id_pg(id, timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Check if segments exist for a section id (PostgreSQL version)
pub async fn segments_exist_for_section_pg(pool: &PgPool, section_id: i64) -> Result<bool, DynError> {
    let sql = segments::exists_for_section_pg(section_id);
    let result: Option<bool> = sqlx::query_scalar(&sql)
        .fetch_optional(pool)
        .await?;
    Ok(result.unwrap_or(false))
}

/// Update a metadata key to a new value (PostgreSQL version)
pub async fn update_metadata_pg(pool: &PgPool, key: &str, value: &str) -> Result<(), DynError> {
    let sql = metadata::update_pg(key, value);
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// Determine whether a metadata key exists (PostgreSQL version)
pub async fn metadata_exists_pg(pool: &PgPool, key: &str) -> Result<bool, DynError> {
    let sql = metadata::exists_pg(key);
    let result: Option<i32> = sqlx::query_scalar(&sql)
        .fetch_optional(pool)
        .await?;
    Ok(result.is_some())
}

/// Get the latest section id before a cutoff timestamp (PostgreSQL version)
pub async fn get_latest_section_before_cutoff_pg(pool: &PgPool, cutoff_ms: i64) -> Result<Option<i64>, DynError> {
    let sql = sections::select_latest_before_cutoff_pg(cutoff_ms);
    let result: Option<i64> = sqlx::query_scalar(&sql)
        .fetch_optional(pool)
        .await?;
    Ok(result)
}

/// Run multiple operations inside a transaction (PostgreSQL version)
pub async fn with_transaction_pg<F, Fut, T>(pool: &PgPool, f: F) -> Result<T, DynError>
where
    F: FnOnce(&mut Transaction<'_, sqlx::Postgres>) -> Fut,
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
// Sync wrapper functions for use in blocking code
// These use SyncDbPg's embedded runtime to block on async operations
// ============================================================================

/// Sync wrapper: Initialize database schema (PostgreSQL)
pub fn init_database_schema_pg_sync(db: &SyncDbPg) -> Result<(), DynError> {
    db.block_on(init_database_schema_pg(db.pool()))
}

/// Sync wrapper: Query metadata (PostgreSQL)
pub fn query_metadata_pg_sync(db: &SyncDbPg, key: &str) -> Result<Option<String>, DynError> {
    db.block_on(query_metadata_pg(db.pool(), key))
}

/// Sync wrapper: Insert metadata (PostgreSQL)
pub fn insert_metadata_pg_sync(db: &SyncDbPg, key: &str, value: &str) -> Result<(), DynError> {
    db.block_on(insert_metadata_pg(db.pool(), key, value))
}

/// Sync wrapper: Upsert metadata (PostgreSQL)
pub fn upsert_metadata_pg_sync(db: &SyncDbPg, key: &str, value: &str) -> Result<(), DynError> {
    db.block_on(upsert_metadata_pg(db.pool(), key, value))
}

/// Sync wrapper: Execute a raw SQL query (PostgreSQL)
pub fn execute_pg_sync(db: &SyncDbPg, sql: &str) -> Result<u64, DynError> {
    db.block_on(execute_pg(db.pool(), sql))
}

/// Sync wrapper: Query a single optional row value (PostgreSQL)
pub fn query_one_optional_pg_sync<T>(db: &SyncDbPg, sql: &str) -> Result<Option<T>, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    db.block_on(query_one_optional_pg(db.pool(), sql))
}

/// Sync wrapper: Query a single row (PostgreSQL)
pub fn query_one_pg_sync<T>(db: &SyncDbPg, sql: &str) -> Result<T, DynError>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    db.block_on(query_one_pg(db.pool(), sql))
}

/// Sync wrapper: Insert a section (PostgreSQL)
pub fn insert_section_pg_sync(db: &SyncDbPg, id: i64, start_timestamp_ms: i64) -> Result<(), DynError> {
    db.block_on(insert_section_pg(db.pool(), id, start_timestamp_ms))
}

/// Sync wrapper: Insert or ignore a section (PostgreSQL)
pub fn insert_section_or_ignore_pg_sync(db: &SyncDbPg, id: i64, start_timestamp_ms: i64) -> Result<(), DynError> {
    db.block_on(insert_section_or_ignore_pg(db.pool(), id, start_timestamp_ms))
}

/// Sync wrapper: Delete old sections (PostgreSQL)
pub fn delete_old_sections_pg_sync(db: &SyncDbPg, cutoff_ms: i64, keeper_section_id: i64) -> Result<u64, DynError> {
    db.block_on(delete_old_sections_pg(db.pool(), cutoff_ms, keeper_section_id))
}

/// Sync wrapper: Insert a segment with explicit ID (PostgreSQL)
pub fn insert_segment_with_id_pg_sync(
    db: &SyncDbPg,
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    audio_data: &[u8],
    section_id: i64,
    duration_samples: i64,
) -> Result<(), DynError> {
    db.block_on(insert_segment_with_id_pg(db.pool(), id, timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples))
}

/// Sync wrapper: Check if segments exist for a section (PostgreSQL)
pub fn segments_exist_for_section_pg_sync(db: &SyncDbPg, section_id: i64) -> Result<bool, DynError> {
    db.block_on(segments_exist_for_section_pg(db.pool(), section_id))
}

/// Sync wrapper: Update metadata (PostgreSQL)
pub fn update_metadata_pg_sync(db: &SyncDbPg, key: &str, value: &str) -> Result<(), DynError> {
    db.block_on(update_metadata_pg(db.pool(), key, value))
}

/// Sync wrapper: Check if metadata key exists (PostgreSQL)
pub fn metadata_exists_pg_sync(db: &SyncDbPg, key: &str) -> Result<bool, DynError> {
    db.block_on(metadata_exists_pg(db.pool(), key))
}

/// Sync wrapper: Get latest section before cutoff (PostgreSQL)
pub fn get_latest_section_before_cutoff_pg_sync(db: &SyncDbPg, cutoff_ms: i64) -> Result<Option<i64>, DynError> {
    db.block_on(get_latest_section_before_cutoff_pg(db.pool(), cutoff_ms))
}
