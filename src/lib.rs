// Library interface for testing

// Declare all modules
pub mod audio;
pub mod config;
pub mod constants;
pub mod fmp4;
pub mod record;
pub mod schedule;
pub mod serve;
pub mod streaming;
pub mod sync;
pub mod webm;

// Re-export the expected database version for convenience
pub use constants::EXPECTED_DB_VERSION;
