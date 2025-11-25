use sea_query::{Expr, Func, Order, PostgresQueryBuilder, Query, SqliteQueryBuilder};

use crate::schema::{Sections, Segments};

/// INSERT INTO segments (timestamp_ms, is_timestamp_from_source, section_id, audio_data, duration_samples)
/// VALUES (?, ?, ?, ?, ?)
pub fn insert(
    timestamp_ms: i64,
    is_timestamp_from_source: bool,
    section_id: i64,
    audio_data: &[u8],
    duration_samples: i64,
) -> String {
    Query::insert()
        .into_table(Segments::Table)
        .columns([
            Segments::TimestampMs,
            Segments::IsTimestampFromSource,
            Segments::SectionId,
            Segments::AudioData,
            Segments::DurationSamples,
        ])
        .values_panic([
            timestamp_ms.into(),
            (is_timestamp_from_source as i32).into(),
            section_id.into(),
            audio_data.to_vec().into(),
            duration_samples.into(),
        ])
        .to_string(SqliteQueryBuilder)
}

/// INSERT INTO segments (id, timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples)
/// VALUES (?, ?, ?, ?, ?, ?)
/// Used for sync where we want to preserve the original ID
pub fn insert_with_id(
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    audio_data: &[u8],
    section_id: i64,
    duration_samples: i64,
) -> String {
    Query::insert()
        .into_table(Segments::Table)
        .columns([
            Segments::Id,
            Segments::TimestampMs,
            Segments::IsTimestampFromSource,
            Segments::AudioData,
            Segments::SectionId,
            Segments::DurationSamples,
        ])
        .values_panic([
            id.into(),
            timestamp_ms.into(),
            is_timestamp_from_source.into(),
            audio_data.to_vec().into(),
            section_id.into(),
            duration_samples.into(),
        ])
        .to_string(SqliteQueryBuilder)
}

/// SELECT EXISTS(SELECT 1 FROM segments WHERE section_id = ?)
pub fn exists_for_section(section_id: i64) -> String {
    let subquery = Query::select()
        .expr(Expr::val(1))
        .from(Segments::Table)
        .and_where(Expr::col(Segments::SectionId).eq(section_id))
        .to_owned();

    Query::select()
        .expr(Expr::exists(subquery))
        .to_string(SqliteQueryBuilder)
}

/// SELECT MIN(id), MAX(id) FROM segments
pub fn select_min_max_id() -> String {
    Query::select()
        .expr(Func::min(Expr::col(Segments::Id)))
        .expr(Func::max(Expr::col(Segments::Id)))
        .from(Segments::Table)
        .to_string(SqliteQueryBuilder)
}

/// SELECT MAX(id) FROM segments
pub fn select_max_id() -> String {
    Query::select()
        .expr(Func::max(Expr::col(Segments::Id)))
        .from(Segments::Table)
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, duration_samples FROM segments WHERE id >= ? AND id <= ? ORDER BY id
pub fn select_range_for_playlist(start_id: i64, end_id: i64) -> String {
    Query::select()
        .columns([Segments::Id, Segments::DurationSamples])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).gte(start_id))
        .and_where(Expr::col(Segments::Id).lte(end_id))
        .order_by(Segments::Id, Order::Asc)
        .to_string(SqliteQueryBuilder)
}

/// SELECT audio_data FROM segments WHERE id = ?
pub fn select_audio_by_id(id: i64) -> String {
    Query::select()
        .column(Segments::AudioData)
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).eq(id))
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples
/// FROM segments WHERE id >= ? AND id <= ? ORDER BY id LIMIT ?
pub fn select_range_with_limit(start_id: i64, end_id: i64, limit: u64) -> String {
    Query::select()
        .columns([
            Segments::Id,
            Segments::TimestampMs,
            Segments::IsTimestampFromSource,
            Segments::AudioData,
            Segments::SectionId,
            Segments::DurationSamples,
        ])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).gte(start_id))
        .and_where(Expr::col(Segments::Id).lte(end_id))
        .order_by(Segments::Id, Order::Asc)
        .limit(limit)
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, audio_data FROM segments WHERE section_id = ? ORDER BY id
pub fn select_by_section_id(section_id: i64) -> String {
    Query::select()
        .columns([Segments::Id, Segments::AudioData])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::SectionId).eq(section_id))
        .order_by(Segments::Id, Order::Asc)
        .to_string(SqliteQueryBuilder)
}

/// SELECT id, audio_data FROM segments WHERE id >= ? AND id <= ? ORDER BY id
pub fn select_by_id_range(start_id: i64, end_id: i64) -> String {
    Query::select()
        .columns([Segments::Id, Segments::AudioData])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).gte(start_id))
        .and_where(Expr::col(Segments::Id).lte(end_id))
        .order_by(Segments::Id, Order::Asc)
        .to_string(SqliteQueryBuilder)
}

/// SELECT MIN(id), MAX(id) FROM segments WHERE section_id = ?
pub fn select_min_max_id_for_section(section_id: i64) -> String {
    Query::select()
        .expr(Func::min(Expr::col(Segments::Id)))
        .expr(Func::max(Expr::col(Segments::Id)))
        .from(Segments::Table)
        .and_where(Expr::col(Segments::SectionId).eq(section_id))
        .to_string(SqliteQueryBuilder)
}

/// SELECT MAX(id), COUNT(id) FROM segments WHERE section_id = ?
pub fn select_max_and_count_for_section(section_id: i64) -> String {
    Query::select()
        .expr(Func::max(Expr::col(Segments::Id)))
        .expr(Func::count(Expr::col(Segments::Id)))
        .from(Segments::Table)
        .and_where(Expr::col(Segments::SectionId).eq(section_id))
        .to_string(SqliteQueryBuilder)
}

/// SELECT s.id, s.start_timestamp_ms, MIN(seg.id) as start_segment_id, MAX(seg.id) as end_segment_id
/// FROM sections s
/// LEFT JOIN segments seg ON s.id = seg.section_id
/// GROUP BY s.id
/// ORDER BY s.start_timestamp_ms
pub fn select_sessions_with_join() -> String {
    Query::select()
        .column((Sections::Table, Sections::Id))
        .column((Sections::Table, Sections::StartTimestampMs))
        .expr_as(
            Func::min(Expr::col((Segments::Table, Segments::Id))),
            sea_query::Alias::new("start_segment_id"),
        )
        .expr_as(
            Func::max(Expr::col((Segments::Table, Segments::Id))),
            sea_query::Alias::new("end_segment_id"),
        )
        .from(Sections::Table)
        .left_join(
            Segments::Table,
            Expr::col((Sections::Table, Sections::Id))
                .equals((Segments::Table, Segments::SectionId)),
        )
        .group_by_col((Sections::Table, Sections::Id))
        .order_by((Sections::Table, Sections::StartTimestampMs), Order::Asc)
        .to_string(SqliteQueryBuilder)
}

// ============================================================================
// PostgreSQL variants
// ============================================================================

/// INSERT INTO segments with explicit ID - PostgreSQL
/// Used for sync where we want to preserve the original ID
pub fn insert_with_id_pg(
    id: i64,
    timestamp_ms: i64,
    is_timestamp_from_source: i32,
    audio_data: &[u8],
    section_id: i64,
    duration_samples: i64,
) -> String {
    Query::insert()
        .into_table(Segments::Table)
        .columns([
            Segments::Id,
            Segments::TimestampMs,
            Segments::IsTimestampFromSource,
            Segments::AudioData,
            Segments::SectionId,
            Segments::DurationSamples,
        ])
        .values_panic([
            id.into(),
            timestamp_ms.into(),
            is_timestamp_from_source.into(),
            audio_data.to_vec().into(),
            section_id.into(),
            duration_samples.into(),
        ])
        .to_string(PostgresQueryBuilder)
}

/// SELECT EXISTS(SELECT 1 FROM segments WHERE section_id = ?) - PostgreSQL
pub fn exists_for_section_pg(section_id: i64) -> String {
    let subquery = Query::select()
        .expr(Expr::val(1))
        .from(Segments::Table)
        .and_where(Expr::col(Segments::SectionId).eq(section_id))
        .to_owned();

    Query::select()
        .expr(Expr::exists(subquery))
        .to_string(PostgresQueryBuilder)
}

/// SELECT MIN(id), MAX(id) FROM segments - PostgreSQL
pub fn select_min_max_id_pg() -> String {
    Query::select()
        .expr(Func::min(Expr::col(Segments::Id)))
        .expr(Func::max(Expr::col(Segments::Id)))
        .from(Segments::Table)
        .to_string(PostgresQueryBuilder)
}

/// SELECT MAX(id) FROM segments - PostgreSQL
pub fn select_max_id_pg() -> String {
    Query::select()
        .expr(Func::max(Expr::col(Segments::Id)))
        .from(Segments::Table)
        .to_string(PostgresQueryBuilder)
}

/// SELECT id, timestamp_ms, is_timestamp_from_source, audio_data, section_id, duration_samples
/// FROM segments WHERE id >= ? AND id <= ? ORDER BY id LIMIT ? - PostgreSQL
pub fn select_range_with_limit_pg(start_id: i64, end_id: i64, limit: u64) -> String {
    Query::select()
        .columns([
            Segments::Id,
            Segments::TimestampMs,
            Segments::IsTimestampFromSource,
            Segments::AudioData,
            Segments::SectionId,
            Segments::DurationSamples,
        ])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).gte(start_id))
        .and_where(Expr::col(Segments::Id).lte(end_id))
        .order_by(Segments::Id, Order::Asc)
        .limit(limit)
        .to_string(PostgresQueryBuilder)
}

/// SELECT id, audio_data FROM segments WHERE section_id = ? ORDER BY id - PostgreSQL
pub fn select_by_section_id_pg(section_id: i64) -> String {
    Query::select()
        .columns([Segments::Id, Segments::AudioData])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::SectionId).eq(section_id))
        .order_by(Segments::Id, Order::Asc)
        .to_string(PostgresQueryBuilder)
}

/// SELECT id, audio_data FROM segments WHERE id >= ? AND id <= ? ORDER BY id - PostgreSQL
pub fn select_by_id_range_pg(start_id: i64, end_id: i64) -> String {
    Query::select()
        .columns([Segments::Id, Segments::AudioData])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).gte(start_id))
        .and_where(Expr::col(Segments::Id).lte(end_id))
        .order_by(Segments::Id, Order::Asc)
        .to_string(PostgresQueryBuilder)
}

/// SELECT id, duration_samples FROM segments WHERE id >= ? AND id <= ? ORDER BY id - PostgreSQL
pub fn select_range_for_playlist_pg(start_id: i64, end_id: i64) -> String {
    Query::select()
        .columns([Segments::Id, Segments::DurationSamples])
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).gte(start_id))
        .and_where(Expr::col(Segments::Id).lte(end_id))
        .order_by(Segments::Id, Order::Asc)
        .to_string(PostgresQueryBuilder)
}

/// SELECT audio_data FROM segments WHERE id = ? - PostgreSQL
pub fn select_audio_by_id_pg(id: i64) -> String {
    Query::select()
        .column(Segments::AudioData)
        .from(Segments::Table)
        .and_where(Expr::col(Segments::Id).eq(id))
        .to_string(PostgresQueryBuilder)
}

/// SELECT s.id, s.start_timestamp_ms, MIN(seg.id) as start_segment_id, MAX(seg.id) as end_segment_id
/// FROM sections s
/// LEFT JOIN segments seg ON s.id = seg.section_id
/// GROUP BY s.id
/// ORDER BY s.start_timestamp_ms - PostgreSQL
pub fn select_sessions_with_join_pg() -> String {
    Query::select()
        .column((Sections::Table, Sections::Id))
        .column((Sections::Table, Sections::StartTimestampMs))
        .expr_as(
            Func::min(Expr::col((Segments::Table, Segments::Id))),
            sea_query::Alias::new("start_segment_id"),
        )
        .expr_as(
            Func::max(Expr::col((Segments::Table, Segments::Id))),
            sea_query::Alias::new("end_segment_id"),
        )
        .from(Sections::Table)
        .left_join(
            Segments::Table,
            Expr::col((Sections::Table, Sections::Id))
                .equals((Segments::Table, Segments::SectionId)),
        )
        .group_by_col((Sections::Table, Sections::Id))
        .order_by((Sections::Table, Sections::StartTimestampMs), Order::Asc)
        .to_string(PostgresQueryBuilder)
}
