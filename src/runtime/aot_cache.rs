//! Content-addressable AOT cache for compiled WASM components.
//!
//! Stores pre-compiled components in `~/.mik/cache/aot/` using BLAKE3 content hashes
//! for cache keys. Provides automatic invalidation on wasmtime version changes
//! and LRU-based cleanup.
//!
//! # Cache Structure
//!
//! ```text
//! ~/.mik/
//!   cache/
//!     aot/
//!       v1/                           # Cache format version
//!         wasmtime-40.0.0/            # Wasmtime version isolation
//!           ab12cd34ef56.aot          # Content hash -> compiled artifact
//! ```

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};

/// Cache format version. Increment when cache format changes.
const CACHE_VERSION: &str = "v1";

/// Wasmtime version for cache isolation.
const WASMTIME_VERSION: &str = "wasmtime-40";

/// AOT cache configuration.
#[derive(Debug, Clone)]
pub struct AotCacheConfig {
    /// Maximum cache size in bytes (default: 1GB).
    pub max_size_bytes: u64,
    /// Bypass cache entirely (for hot-reload mode).
    pub bypass: bool,
}

impl Default for AotCacheConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 1024 * 1024 * 1024, // 1GB
            bypass: false,
        }
    }
}

/// Content-addressable AOT cache.
///
/// Caches compiled WASM components using BLAKE3 content hashes as keys.
/// Components are isolated by wasmtime version to ensure compatibility.
#[derive(Debug, Clone)]
pub struct AotCache {
    /// Cache directory (`~/.mik/cache/aot/v1/<wasmtime-version>/`).
    cache_dir: PathBuf,
    /// Configuration.
    config: AotCacheConfig,
}

impl AotCache {
    /// Create a new AOT cache with the given configuration.
    ///
    /// Creates the cache directory structure if it doesn't exist.
    pub fn new(config: AotCacheConfig) -> Result<Self> {
        let home = dirs::home_dir().context("Failed to get home directory")?;

        let cache_dir = home
            .join(".mik")
            .join("cache")
            .join("aot")
            .join(CACHE_VERSION)
            .join(WASMTIME_VERSION);

        fs::create_dir_all(&cache_dir).context("Failed to create AOT cache directory")?;

        Ok(Self { cache_dir, config })
    }

    /// Create an AOT cache that bypasses all caching (for hot-reload mode).
    pub fn bypass() -> Self {
        Self {
            cache_dir: PathBuf::new(),
            config: AotCacheConfig {
                bypass: true,
                ..Default::default()
            },
        }
    }

    /// Check if the cache is in bypass mode.
    pub fn is_bypass(&self) -> bool {
        self.config.bypass
    }

    /// Compute cache key from WASM bytes using BLAKE3.
    ///
    /// Returns a 32-character hex string (128-bit truncated hash).
    pub fn compute_key(wasm_bytes: &[u8]) -> String {
        let hash = blake3::hash(wasm_bytes);
        // Truncate to 128 bits (16 bytes = 32 hex chars) for shorter filenames
        hex::encode(&hash.as_bytes()[..16])
    }

    /// Get the path to a cached AOT artifact, if it exists.
    ///
    /// Returns `None` if cache is bypassed or artifact doesn't exist.
    pub fn get(&self, wasm_bytes: &[u8]) -> Option<PathBuf> {
        if self.config.bypass {
            return None;
        }

        let key = Self::compute_key(wasm_bytes);
        let aot_path = self.cache_dir.join(format!("{key}.aot"));

        if aot_path.exists() {
            Some(aot_path)
        } else {
            None
        }
    }

    /// Store a compiled artifact in the cache.
    ///
    /// Uses atomic write (temp file + rename) to prevent corruption.
    /// Returns the path to the cached artifact.
    pub fn put(&self, wasm_bytes: &[u8], compiled: &[u8]) -> Result<PathBuf> {
        if self.config.bypass {
            anyhow::bail!("Cannot store in cache when bypass is enabled");
        }

        let key = Self::compute_key(wasm_bytes);
        let aot_path = self.cache_dir.join(format!("{key}.aot"));
        let temp_path = self.cache_dir.join(format!("{key}.aot.tmp"));

        // Write atomically using temp file + rename
        fs::write(&temp_path, compiled).context("Failed to write cache temp file")?;

        fs::rename(&temp_path, &aot_path).context("Failed to rename cache file")?;

        // Trigger cleanup if needed (best-effort, don't fail on cleanup errors)
        if let Err(e) = self.maybe_cleanup() {
            tracing::debug!("AOT cache cleanup skipped: {e}");
        }

        Ok(aot_path)
    }

    /// Remove a cached artifact by its WASM content.
    pub fn remove(&self, wasm_bytes: &[u8]) -> Result<bool> {
        if self.config.bypass {
            return Ok(false);
        }

        let key = Self::compute_key(wasm_bytes);
        let aot_path = self.cache_dir.join(format!("{key}.aot"));

        if aot_path.exists() {
            fs::remove_file(&aot_path).context("Failed to remove cache file")?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get cache statistics.
    pub fn stats(&self) -> Result<CacheStats> {
        if self.config.bypass {
            return Ok(CacheStats::default());
        }

        let mut total_size = 0u64;
        let mut entry_count = 0usize;

        for entry in fs::read_dir(&self.cache_dir).context("Failed to read cache directory")? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aot")
                && let Ok(metadata) = entry.metadata()
            {
                total_size += metadata.len();
                entry_count += 1;
            }
        }

        Ok(CacheStats {
            entry_count,
            total_size_bytes: total_size,
            max_size_bytes: self.config.max_size_bytes,
            cache_dir: self.cache_dir.clone(),
        })
    }

    /// Clean up cache using LRU based on file access time.
    ///
    /// Removes oldest entries until total size is under the limit.
    pub fn cleanup(&self) -> Result<CleanupStats> {
        if self.config.bypass {
            return Ok(CleanupStats::default());
        }

        let mut entries: Vec<CacheEntry> = Vec::new();

        // Collect all cache entries
        for entry in fs::read_dir(&self.cache_dir).context("Failed to read cache directory")? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aot")
                && let Ok(metadata) = entry.metadata()
            {
                let accessed = metadata
                    .accessed()
                    .or_else(|_| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);

                entries.push(CacheEntry {
                    path,
                    size: metadata.len(),
                    accessed,
                });
            }
        }

        // Sort by access time (oldest first)
        entries.sort_by_key(|e| e.accessed);

        // Calculate total size
        let total_size: u64 = entries.iter().map(|e| e.size).sum();

        // Remove oldest entries until under limit
        let mut removed = 0usize;
        let mut freed = 0u64;
        let mut current_size = total_size;

        while current_size > self.config.max_size_bytes && !entries.is_empty() {
            if let Some(entry) = entries.first() {
                if fs::remove_file(&entry.path).is_ok() {
                    freed += entry.size;
                    current_size -= entry.size;
                    removed += 1;
                }
                entries.remove(0);
            }
        }

        Ok(CleanupStats {
            entries_removed: removed,
            bytes_freed: freed,
            current_size_bytes: current_size,
        })
    }

    /// Clear all cached artifacts.
    pub fn clear(&self) -> Result<CleanupStats> {
        if self.config.bypass {
            return Ok(CleanupStats::default());
        }

        let mut removed = 0usize;
        let mut freed = 0u64;

        for entry in fs::read_dir(&self.cache_dir).context("Failed to read cache directory")? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aot")
                && let Ok(metadata) = entry.metadata()
                && fs::remove_file(&path).is_ok()
            {
                freed += metadata.len();
                removed += 1;
            }
        }

        Ok(CleanupStats {
            entries_removed: removed,
            bytes_freed: freed,
            current_size_bytes: 0,
        })
    }

    /// Trigger cleanup if cache size exceeds limit (best-effort).
    fn maybe_cleanup(&self) -> Result<()> {
        let stats = self.stats()?;

        if stats.total_size_bytes > self.config.max_size_bytes {
            let cleanup_stats = self.cleanup()?;
            tracing::debug!(
                "AOT cache cleanup: removed {} entries, freed {} bytes",
                cleanup_stats.entries_removed,
                cleanup_stats.bytes_freed
            );
        }

        Ok(())
    }
}

/// Internal cache entry for LRU sorting.
struct CacheEntry {
    path: PathBuf,
    size: u64,
    accessed: SystemTime,
}

/// Cache statistics.
#[derive(Debug, Default)]
pub struct CacheStats {
    /// Number of cached entries.
    pub entry_count: usize,
    /// Total size of cached entries in bytes.
    pub total_size_bytes: u64,
    /// Maximum cache size in bytes.
    pub max_size_bytes: u64,
    /// Cache directory path.
    pub cache_dir: PathBuf,
}

/// Cleanup operation statistics.
#[derive(Debug, Default)]
pub struct CleanupStats {
    /// Number of entries removed.
    pub entries_removed: usize,
    /// Bytes freed.
    pub bytes_freed: u64,
    /// Current cache size after cleanup.
    pub current_size_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_key() {
        let wasm = b"test wasm content";
        let key = AotCache::compute_key(wasm);

        // Key should be 32 hex chars (128 bits)
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));

        // Same content should produce same key
        let key2 = AotCache::compute_key(wasm);
        assert_eq!(key, key2);

        // Different content should produce different key
        let key3 = AotCache::compute_key(b"different content");
        assert_ne!(key, key3);
    }

    #[test]
    fn test_bypass_mode() {
        let cache = AotCache::bypass();

        assert!(cache.is_bypass());
        assert!(cache.get(b"test").is_none());
        assert!(cache.put(b"test", b"compiled").is_err());
    }
}
