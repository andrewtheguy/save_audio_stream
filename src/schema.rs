use sea_query::Iden;

/// Metadata table - key-value store for database configuration
#[derive(Iden)]
pub enum Metadata {
    Table,
    Key,
    Value,
}

/// Sections table - recording sessions/boundaries
#[derive(Iden)]
pub enum Sections {
    Table,
    Id,
    StartTimestampMs,
    IsExportedToRemote,
}

/// Segments table - individual audio chunks
#[derive(Iden)]
pub enum Segments {
    Table,
    Id,
    TimestampMs,
    IsTimestampFromSource,
    AudioData,
    SectionId,
    DurationSamples,
}
