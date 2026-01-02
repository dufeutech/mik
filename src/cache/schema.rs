//! BLAKE3 content-addressed file cache for JSON schemas.
//!
//! Provides a filesystem-based cache for JSON schemas using content-addressed
//! storage with BLAKE3 hashes.
//!
//! # Cache Structure
//!
//! ```text
//! ~/.cache/mik/schemas/
//!   <blake3-hash>   # Content hash -> cached schema
//! ```
//!
//! # Example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use mik::cache::SchemaCache;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let cache = SchemaCache::new(PathBuf::from("~/.cache/mik/schemas"));
//!
//! // Cache a schema
//! cache.put("my-module.wasm", "{\"type\": \"object\"}").await?;
//!
//! // Retrieve cached schema
//! if let Some(schema) = cache.get("my-module.wasm", "{\"type\": \"object\"}").await {
//!     println!("Cached schema: {schema}");
//! }
//!
//! // Get statistics
//! let stats = cache.stats();
//! println!("Entries: {}, Bytes: {}", stats.entries, stats.total_bytes);
//!
//! // Clean old entries
//! let removed = cache.clean(30)?;
//! println!("Removed {removed} old entries");
//!
//! // Clear all
//! cache.clear()?;
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

/// Statistics about the schema cache.
#[derive(Debug, Clone, Default)]
pub struct SchemaStats {
    /// Number of cached entries.
    pub entries: usize,
    /// Total size of cached schemas in bytes.
    pub total_bytes: u64,
}

/// BLAKE3 content-addressed cache for JSON schemas.
///
/// Uses the hash of `module_path + content` as the cache key,
/// ensuring that any change to either the module path or content
/// results in a new cache entry.
#[derive(Debug, Clone)]
pub struct SchemaCache {
    /// Directory containing cached schemas.
    cache_dir: PathBuf,
}

impl SchemaCache {
    /// Create a new schema cache with the specified cache directory.
    ///
    /// The directory will be created if it doesn't exist when storing schemas.
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Create a schema cache using the default location: `~/.cache/mik/schemas/`
    ///
    /// Returns `None` if the cache directory cannot be determined.
    #[must_use]
    pub fn default_location() -> Option<Self> {
        let cache_base = dirs::cache_dir()?;
        Some(Self::new(cache_base.join("mik").join("schemas")))
    }

    /// Compute the cache key from module path and content using BLAKE3.
    ///
    /// Returns a 64-character hex string (full 256-bit hash).
    #[allow(dead_code)]
    fn compute_key(module_path: &str, content: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(module_path.as_bytes());
        hasher.update(content.as_bytes());
        let hash = hasher.finalize();
        hex::encode(hash.as_bytes())
    }

    /// Get the filesystem path for a cached schema.
    #[allow(dead_code)]
    fn cache_path(&self, module_path: &str, content: &str) -> PathBuf {
        let key = Self::compute_key(module_path, content);
        self.cache_dir.join(key)
    }

    /// Retrieve a cached schema if it exists.
    ///
    /// Returns `Some(schema)` if a cached schema exists for the given
    /// module path and content combination, or `None` if not cached.
    #[allow(dead_code)]
    pub async fn get(&self, module_path: &str, content: &str) -> Option<String> {
        let path = self.cache_path(module_path, content);

        tokio::fs::read_to_string(&path).await.ok()
    }

    /// Store a schema in the cache.
    ///
    /// Creates the cache directory if it doesn't exist.
    /// Uses atomic write (temp file + rename) to prevent corruption.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be created or
    /// the file cannot be written.
    #[allow(dead_code)]
    pub async fn put(&self, module_path: &str, content: &str) -> Result<()> {
        // Ensure cache directory exists
        tokio::fs::create_dir_all(&self.cache_dir)
            .await
            .context("Failed to create schema cache directory")?;

        let key = Self::compute_key(module_path, content);
        let cache_path = self.cache_dir.join(&key);
        let temp_path = self.cache_dir.join(format!("{key}.tmp"));

        // Write atomically using temp file + rename
        tokio::fs::write(&temp_path, content)
            .await
            .context("Failed to write schema cache temp file")?;

        tokio::fs::rename(&temp_path, &cache_path)
            .await
            .context("Failed to rename schema cache file")?;

        Ok(())
    }

    /// Get cache statistics.
    ///
    /// Returns the number of cached entries and total size in bytes.
    /// Errors during enumeration are silently ignored, resulting in
    /// potentially incomplete statistics.
    pub fn stats(&self) -> SchemaStats {
        let mut stats = SchemaStats::default();

        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return stats;
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip temp files and non-files
            if path.extension().is_some() || !path.is_file() {
                continue;
            }

            if let Ok(metadata) = entry.metadata() {
                stats.entries += 1;
                stats.total_bytes += metadata.len();
            }
        }

        stats
    }

    /// Remove cache entries older than the specified age.
    ///
    /// Returns the number of entries removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be read.
    pub fn clean(&self, max_age_days: u64) -> Result<usize> {
        let max_age = Duration::from_secs(max_age_days * 24 * 60 * 60);
        let now = SystemTime::now();
        let mut removed = 0;

        let entries =
            std::fs::read_dir(&self.cache_dir).context("Failed to read schema cache directory")?;

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip temp files and non-files
            if path.extension().is_some() || !path.is_file() {
                continue;
            }

            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            // Use modification time to determine age
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now.duration_since(modified).unwrap_or(Duration::ZERO);

            if age > max_age && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Remove all cached schemas.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be read.
    pub fn clear(&self) -> Result<()> {
        let entries = match std::fs::read_dir(&self.cache_dir) {
            Ok(entries) => entries,
            // Directory doesn't exist, nothing to clear
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).context("Failed to read schema cache directory"),
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip non-files (shouldn't exist, but be safe)
            if !path.is_file() {
                continue;
            }

            // Best effort removal, ignore individual file errors
            let _ = std::fs::remove_file(&path);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_key_deterministic() {
        let key1 = SchemaCache::compute_key("module.wasm", "content");
        let key2 = SchemaCache::compute_key("module.wasm", "content");
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_compute_key_different_module_path() {
        let key1 = SchemaCache::compute_key("module1.wasm", "content");
        let key2 = SchemaCache::compute_key("module2.wasm", "content");
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_compute_key_different_content() {
        let key1 = SchemaCache::compute_key("module.wasm", "content1");
        let key2 = SchemaCache::compute_key("module.wasm", "content2");
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_compute_key_length() {
        let key = SchemaCache::compute_key("module.wasm", "content");
        // Full BLAKE3 hash is 32 bytes = 64 hex characters
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_stats_empty_cache() {
        let cache = SchemaCache::new(PathBuf::from("/nonexistent/path"));
        let stats = cache.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[test]
    fn test_clear_nonexistent_directory() {
        let cache = SchemaCache::new(PathBuf::from("/nonexistent/path"));
        // Should not error on nonexistent directory
        assert!(cache.clear().is_ok());
    }

    #[tokio::test]
    async fn test_put_and_get() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cache = SchemaCache::new(temp_dir.path().to_path_buf());

        let module_path = "test/module.wasm";
        let content = r#"{"type": "object"}"#;

        // Initially not cached
        assert!(cache.get(module_path, content).await.is_none());

        // Store schema
        cache.put(module_path, content).await.unwrap();

        // Now cached
        let cached = cache.get(module_path, content).await;
        assert_eq!(cached, Some(content.to_string()));

        // Stats reflect the entry
        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.total_bytes, content.len() as u64);
    }

    #[tokio::test]
    async fn test_clear() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cache = SchemaCache::new(temp_dir.path().to_path_buf());

        // Add some entries
        cache.put("module1.wasm", "schema1").await.unwrap();
        cache.put("module2.wasm", "schema2").await.unwrap();

        let stats = cache.stats();
        assert_eq!(stats.entries, 2);

        // Clear all
        cache.clear().unwrap();

        let stats = cache.stats();
        assert_eq!(stats.entries, 0);
    }

    #[test]
    fn test_default_location() {
        // This test verifies the default location logic works
        // It may return None on systems without a cache directory
        let cache = SchemaCache::default_location();
        if let Some(cache) = cache {
            assert!(cache.cache_dir.ends_with("schemas"));
        }
    }
}
