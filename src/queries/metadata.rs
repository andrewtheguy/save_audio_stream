use sea_query::{Expr, OnConflict, Query, SqliteQueryBuilder};

use crate::schema::Metadata;

/// SELECT value FROM metadata WHERE key = ?
pub fn select_by_key(key: &str) -> String {
    Query::select()
        .column(Metadata::Value)
        .from(Metadata::Table)
        .and_where(Expr::col(Metadata::Key).eq(key))
        .to_string(SqliteQueryBuilder)
}

/// INSERT INTO metadata (key, value) VALUES (?, ?)
pub fn insert(key: &str, value: &str) -> String {
    Query::insert()
        .into_table(Metadata::Table)
        .columns([Metadata::Key, Metadata::Value])
        .values_panic([key.into(), value.into()])
        .to_string(SqliteQueryBuilder)
}

/// INSERT OR REPLACE INTO metadata (key, value) VALUES (?, ?)
pub fn upsert(key: &str, value: &str) -> String {
    Query::insert()
        .into_table(Metadata::Table)
        .columns([Metadata::Key, Metadata::Value])
        .values_panic([key.into(), value.into()])
        .on_conflict(
            OnConflict::column(Metadata::Key)
                .update_column(Metadata::Value)
                .to_owned(),
        )
        .to_string(SqliteQueryBuilder)
}

/// UPDATE metadata SET value = ? WHERE key = ?
pub fn update(key: &str, value: &str) -> String {
    Query::update()
        .table(Metadata::Table)
        .value(Metadata::Value, value)
        .and_where(Expr::col(Metadata::Key).eq(key))
        .to_string(SqliteQueryBuilder)
}

/// SELECT 1 FROM metadata WHERE key = ? (for existence check)
pub fn exists(key: &str) -> String {
    Query::select()
        .expr(Expr::val(1))
        .from(Metadata::Table)
        .and_where(Expr::col(Metadata::Key).eq(key))
        .to_string(SqliteQueryBuilder)
}
