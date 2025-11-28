// Library interface for testing

use dashmap::DashMap;
use std::sync::{Arc, Mutex};

// Declare all modules
pub mod audio;
pub mod config;
pub mod constants;
pub mod credentials;
pub mod db;
pub mod db_postgres;
pub mod fmp4;
pub mod queries;
pub mod record;
pub mod schedule;
pub mod schema;
pub mod segment_wire;
pub mod serve;
pub mod serve_record;
pub mod sftp;
pub mod streaming;
pub mod sync;
pub mod webm;

// Re-export the expected database version for convenience
pub use constants::EXPECTED_DB_VERSION;

/// Per-show locks to prevent concurrent export and cleanup operations
/// Uses DashMap for concurrent access and Arc<Mutex<()>> for per-show blocking
pub type ShowLocks = Arc<DashMap<String, Arc<Mutex<()>>>>;

/// Get or create a lock for a specific show
/// Returns a cloned Arc to the show's mutex
pub fn get_show_lock(locks: &ShowLocks, show_name: &str) -> Arc<Mutex<()>> {
    locks
        .entry(show_name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}
