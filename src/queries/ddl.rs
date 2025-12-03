use sea_query::{
    ColumnDef, ForeignKey, ForeignKeyAction, Index, PostgresQueryBuilder, SqliteQueryBuilder, Table,
};

use crate::schema::{Metadata, Sections, Segments};

/// CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)
pub fn create_metadata_table() -> String {
    Table::create()
        .table(Metadata::Table)
        .if_not_exists()
        .col(ColumnDef::new(Metadata::Key).string().primary_key())
        .col(ColumnDef::new(Metadata::Value).string().not_null())
        .to_string(SqliteQueryBuilder)
}

/// CREATE TABLE IF NOT EXISTS sections (
///     id INTEGER PRIMARY KEY,
///     start_timestamp_ms INTEGER NOT NULL
/// )
pub fn create_sections_table() -> String {
    Table::create()
        .table(Sections::Table)
        .if_not_exists()
        .col(ColumnDef::new(Sections::Id).big_integer().primary_key())
        .col(
            ColumnDef::new(Sections::StartTimestampMs)
                .big_integer()
                .not_null(),
        )
        .to_string(SqliteQueryBuilder)
}

/// CREATE TABLE IF NOT EXISTS segments (
///     id INTEGER PRIMARY KEY AUTOINCREMENT,
///     timestamp_ms INTEGER NOT NULL,
///     is_timestamp_from_source INTEGER NOT NULL DEFAULT 0,
///     audio_data BLOB NOT NULL,
///     section_id INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE,
///     duration_samples INTEGER NOT NULL
/// )
pub fn create_segments_table() -> String {
    Table::create()
        .table(Segments::Table)
        .if_not_exists()
        .col(
            ColumnDef::new(Segments::Id)
                .integer()
                .primary_key()
                .auto_increment(),
        )
        .col(
            ColumnDef::new(Segments::TimestampMs)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(Segments::IsTimestampFromSource)
                .integer()
                .not_null()
                .default(0),
        )
        .col(ColumnDef::new(Segments::AudioData).blob().not_null())
        .col(ColumnDef::new(Segments::SectionId).big_integer().not_null())
        .col(
            ColumnDef::new(Segments::DurationSamples)
                .integer()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Segments::Table, Segments::SectionId)
                .to(Sections::Table, Sections::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_string(SqliteQueryBuilder)
}

/// CREATE INDEX IF NOT EXISTS idx_segments_boundary ON segments(is_timestamp_from_source, timestamp_ms)
pub fn create_segments_boundary_index() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_segments_boundary")
        .table(Segments::Table)
        .col(Segments::IsTimestampFromSource)
        .col(Segments::TimestampMs)
        .to_string(SqliteQueryBuilder)
}

/// CREATE INDEX IF NOT EXISTS idx_segments_section_id ON segments(section_id)
pub fn create_segments_section_id_index() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_segments_section_id")
        .table(Segments::Table)
        .col(Segments::SectionId)
        .to_string(SqliteQueryBuilder)
}

/// CREATE INDEX IF NOT EXISTS idx_sections_start_timestamp ON sections(start_timestamp_ms)
pub fn create_sections_start_timestamp_index() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_sections_start_timestamp")
        .table(Sections::Table)
        .col(Sections::StartTimestampMs)
        .to_string(SqliteQueryBuilder)
}

// ============================================================================
// PostgreSQL variants
// ============================================================================

/// CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL) - PostgreSQL
pub fn create_metadata_table_pg() -> String {
    Table::create()
        .table(Metadata::Table)
        .if_not_exists()
        .col(ColumnDef::new(Metadata::Key).string().primary_key())
        .col(ColumnDef::new(Metadata::Value).string().not_null())
        .to_string(PostgresQueryBuilder)
}

/// CREATE TABLE IF NOT EXISTS sections - PostgreSQL
pub fn create_sections_table_pg() -> String {
    Table::create()
        .table(Sections::Table)
        .if_not_exists()
        .col(ColumnDef::new(Sections::Id).big_integer().primary_key())
        .col(
            ColumnDef::new(Sections::StartTimestampMs)
                .big_integer()
                .not_null(),
        )
        .to_string(PostgresQueryBuilder)
}

/// CREATE TABLE IF NOT EXISTS segments - PostgreSQL
/// Note: Uses BIGSERIAL instead of INTEGER AUTOINCREMENT
pub fn create_segments_table_pg() -> String {
    Table::create()
        .table(Segments::Table)
        .if_not_exists()
        .col(
            ColumnDef::new(Segments::Id)
                .big_integer()
                .primary_key()
                .auto_increment(), // Sea Query handles BIGSERIAL for PostgreSQL
        )
        .col(
            ColumnDef::new(Segments::TimestampMs)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(Segments::IsTimestampFromSource)
                .integer()
                .not_null()
                .default(0),
        )
        .col(ColumnDef::new(Segments::AudioData).binary().not_null()) // BYTEA in PostgreSQL
        .col(ColumnDef::new(Segments::SectionId).big_integer().not_null())
        .col(
            ColumnDef::new(Segments::DurationSamples)
                .big_integer()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Segments::Table, Segments::SectionId)
                .to(Sections::Table, Sections::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_string(PostgresQueryBuilder)
}

/// CREATE INDEX IF NOT EXISTS idx_segments_boundary - PostgreSQL
pub fn create_segments_boundary_index_pg() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_segments_boundary")
        .table(Segments::Table)
        .col(Segments::IsTimestampFromSource)
        .col(Segments::TimestampMs)
        .to_string(PostgresQueryBuilder)
}

/// CREATE INDEX IF NOT EXISTS idx_segments_section_id - PostgreSQL
pub fn create_segments_section_id_index_pg() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_segments_section_id")
        .table(Segments::Table)
        .col(Segments::SectionId)
        .to_string(PostgresQueryBuilder)
}

/// CREATE INDEX IF NOT EXISTS idx_sections_start_timestamp - PostgreSQL
pub fn create_sections_start_timestamp_index_pg() -> String {
    Index::create()
        .if_not_exists()
        .name("idx_sections_start_timestamp")
        .table(Sections::Table)
        .col(Sections::StartTimestampMs)
        .to_string(PostgresQueryBuilder)
}
