use sea_query::{Expr, OnConflict, Order, PostgresQueryBuilder, Query, SqliteQueryBuilder};

use crate::schema::Sections;

/// INSERT INTO sections (id, start_timestamp_ms) VALUES (?, ?)
pub fn insert(id: i64, start_timestamp_ms: i64) -> String {
    Query::insert()
        .into_table(Sections::Table)
        .columns([Sections::Id, Sections::StartTimestampMs])
        .values_panic([id.into(), start_timestamp_ms.into()])
        .to_string(SqliteQueryBuilder)
}

/// INSERT OR IGNORE INTO sections (id, start_timestamp_ms) VALUES (?, ?)
pub fn insert_or_ignore(id: i64, start_timestamp_ms: i64) -> String {
    Query::insert()
        .into_table(Sections::Table)
        .columns([Sections::Id, Sections::StartTimestampMs])
        .values_panic([id.into(), start_timestamp_ms.into()])
        .on_conflict(OnConflict::new().do_nothing().to_owned())
        .to_string(SqliteQueryBuilder)
}

/// DELETE FROM sections WHERE start_timestamp_ms < ? AND id != ?
pub fn delete_old_sections(cutoff_ms: i64, keeper_section_id: i64) -> String {
    Query::delete()
        .from_table(Sections::Table)
        .and_where(Expr::col(Sections::StartTimestampMs).lt(cutoff_ms))
        .and_where(Expr::col(Sections::Id).ne(keeper_section_id))
        .to_string(SqliteQueryBuilder)
}

/// SELECT id FROM sections WHERE start_timestamp_ms < ? ORDER BY start_timestamp_ms DESC LIMIT 1
pub fn select_latest_before_cutoff(cutoff_ms: i64) -> String {
    Query::select()
        .column(Sections::Id)
        .from(Sections::Table)
        .and_where(Expr::col(Sections::StartTimestampMs).lt(cutoff_ms))
        .order_by(Sections::StartTimestampMs, Order::Desc)
        .limit(1)
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, start_timestamp_ms FROM sections ORDER BY id
pub fn select_all() -> String {
    Query::select()
        .columns([Sections::Id, Sections::StartTimestampMs])
        .from(Sections::Table)
        .order_by(Sections::Id, Order::Asc)
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, start_timestamp_ms FROM sections WHERE start_timestamp_ms >= ? ORDER BY id
pub fn select_all_after_cutoff(cutoff_ms: i64) -> String {
    Query::select()
        .columns([Sections::Id, Sections::StartTimestampMs])
        .from(Sections::Table)
        .and_where(Expr::col(Sections::StartTimestampMs).gte(cutoff_ms))
        .order_by(Sections::Id, Order::Asc)
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, start_timestamp_ms FROM sections WHERE id = ?
pub fn select_by_id(id: i64) -> String {
    Query::select()
        .columns([Sections::Id, Sections::StartTimestampMs])
        .from(Sections::Table)
        .and_where(Expr::col(Sections::Id).eq(id))
        .to_string(SqliteQueryBuilder)
}

// ============================================================================
// PostgreSQL variants
// ============================================================================

/// INSERT INTO sections (id, start_timestamp_ms) VALUES (?, ?) - PostgreSQL
pub fn insert_pg(id: i64, start_timestamp_ms: i64) -> String {
    Query::insert()
        .into_table(Sections::Table)
        .columns([Sections::Id, Sections::StartTimestampMs])
        .values_panic([id.into(), start_timestamp_ms.into()])
        .to_string(PostgresQueryBuilder)
}

/// INSERT INTO sections ... ON CONFLICT (id) DO NOTHING - PostgreSQL
pub fn insert_or_ignore_pg(id: i64, start_timestamp_ms: i64) -> String {
    Query::insert()
        .into_table(Sections::Table)
        .columns([Sections::Id, Sections::StartTimestampMs])
        .values_panic([id.into(), start_timestamp_ms.into()])
        .on_conflict(OnConflict::column(Sections::Id).do_nothing().to_owned())
        .to_string(PostgresQueryBuilder)
}

/// DELETE FROM sections WHERE start_timestamp_ms < ? AND id != ? - PostgreSQL
pub fn delete_old_sections_pg(cutoff_ms: i64, keeper_section_id: i64) -> String {
    Query::delete()
        .from_table(Sections::Table)
        .and_where(Expr::col(Sections::StartTimestampMs).lt(cutoff_ms))
        .and_where(Expr::col(Sections::Id).ne(keeper_section_id))
        .to_string(PostgresQueryBuilder)
}

/// SELECT id FROM sections WHERE start_timestamp_ms < ? ORDER BY start_timestamp_ms DESC LIMIT 1 - PostgreSQL
pub fn select_latest_before_cutoff_pg(cutoff_ms: i64) -> String {
    Query::select()
        .column(Sections::Id)
        .from(Sections::Table)
        .and_where(Expr::col(Sections::StartTimestampMs).lt(cutoff_ms))
        .order_by(Sections::StartTimestampMs, Order::Desc)
        .limit(1)
        .to_string(PostgresQueryBuilder)
}

/// SELECT id, start_timestamp_ms FROM sections ORDER BY id - PostgreSQL
pub fn select_all_pg() -> String {
    Query::select()
        .columns([Sections::Id, Sections::StartTimestampMs])
        .from(Sections::Table)
        .order_by(Sections::Id, Order::Asc)
        .to_string(PostgresQueryBuilder)
}

/// SELECT id, start_timestamp_ms FROM sections WHERE id = ? - PostgreSQL
pub fn select_by_id_pg(id: i64) -> String {
    Query::select()
        .columns([Sections::Id, Sections::StartTimestampMs])
        .from(Sections::Table)
        .and_where(Expr::col(Sections::Id).eq(id))
        .to_string(PostgresQueryBuilder)
}
