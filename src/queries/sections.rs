use sea_query::{Expr, OnConflict, Order, Query, SqliteQueryBuilder};

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

/// SELECT id, start_timestamp_ms, is_exported_to_remote FROM sections WHERE id = ?
pub fn select_by_id(id: i64) -> String {
    Query::select()
        .columns([
            Sections::Id,
            Sections::StartTimestampMs,
            Sections::IsExportedToRemote,
        ])
        .from(Sections::Table)
        .and_where(Expr::col(Sections::Id).eq(id))
        .to_string(SqliteQueryBuilder)
}

/// UPDATE sections SET is_exported_to_remote = 1 WHERE id = ?
pub fn mark_exported(id: i64) -> String {
    Query::update()
        .table(Sections::Table)
        .value(Sections::IsExportedToRemote, 1)
        .and_where(Expr::col(Sections::Id).eq(id))
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, start_timestamp_ms FROM sections WHERE is_exported_to_remote IS NULL OR is_exported_to_remote = 0
pub fn select_unexported() -> String {
    Query::select()
        .columns([Sections::Id, Sections::StartTimestampMs])
        .from(Sections::Table)
        .cond_where(
            Expr::col(Sections::IsExportedToRemote)
                .is_null()
                .or(Expr::col(Sections::IsExportedToRemote).eq(0)),
        )
        .to_string(SqliteQueryBuilder)
}

/// SELECT id FROM sections WHERE (is_exported_to_remote IS NULL OR is_exported_to_remote = 0) AND id != ?
pub fn select_unexported_excluding(exclude_id: i64) -> String {
    Query::select()
        .column(Sections::Id)
        .from(Sections::Table)
        .cond_where(
            Expr::col(Sections::IsExportedToRemote)
                .is_null()
                .or(Expr::col(Sections::IsExportedToRemote).eq(0)),
        )
        .and_where(Expr::col(Sections::Id).ne(exclude_id))
        .to_string(SqliteQueryBuilder)
}

/// SELECT id FROM sections WHERE is_exported_to_remote IS NULL OR is_exported_to_remote = 0
pub fn select_unexported_ids() -> String {
    Query::select()
        .column(Sections::Id)
        .from(Sections::Table)
        .cond_where(
            Expr::col(Sections::IsExportedToRemote)
                .is_null()
                .or(Expr::col(Sections::IsExportedToRemote).eq(0)),
        )
        .to_string(SqliteQueryBuilder)
}
