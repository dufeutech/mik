//! Cache module for mik
//!
//! Provides caching infrastructure for various components including
//! schema caching and AOT compilation artifacts.

use std::path::PathBuf;

mod schema;
pub use schema::SchemaCache;
#[allow(unused_imports)]
pub use schema::SchemaStats;

/// Configuration for cache behavior
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum number of entries in the cache
    pub max_entries: usize,
    /// Maximum total size in bytes
    pub max_bytes: u64,
    /// Maximum age in days before entries are considered stale
    pub max_age_days: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 1000,
            max_bytes: 256 * 1024 * 1024, // 256 MB
            max_age_days: 30,
        }
    }
}

/// Returns the default cache directory for mik.
///
/// Uses the platform-specific cache directory (e.g., `~/.cache/mik` on Linux,
/// `~/Library/Caches/mik` on macOS, `%LOCALAPPDATA%/mik` on Windows).
/// Falls back to `.cache/mik` in the current directory if the system cache
/// directory cannot be determined.
#[allow(dead_code)]
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("mik")
}
