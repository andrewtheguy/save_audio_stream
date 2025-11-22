use rand::Rng;

/// Expected database schema version
/// All databases must use this version for compatibility
pub const EXPECTED_DB_VERSION: &str = "4";

/// Generate a unique database ID
/// Used for both source and target databases with the same format
pub fn generate_db_unique_id() -> String {
    format!(
        "db_{}",
        rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(12)
            .map(char::from)
            .collect::<String>()
    )
}
