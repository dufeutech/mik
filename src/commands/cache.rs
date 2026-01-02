//! Cache management commands.
//!
//! Provides commands for managing caches:
//! - `mik cache info` - Display cache statistics and location
//! - `mik cache clean` - Remove stale entries to free disk space
//! - `mik cache clear` - Remove all cached entries
//!
//! Manages two caches:
//! - **AOT cache**: Pre-compiled WASM components for faster startup
//! - **OCI cache**: Downloaded registry artifacts (content-addressable)

use anyhow::Result;
use std::fs;
use std::path::PathBuf;

use crate::CacheAction;
use crate::cache::SchemaCache;
use crate::runtime::aot_cache::{AotCache, AotCacheConfig};

/// Get OCI cache directory.
fn get_oci_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("mik").join("oci").join("blobs"))
}

/// Get OCI cache statistics.
fn get_oci_cache_stats() -> (u64, u64) {
    let Some(cache_dir) = get_oci_cache_dir() else {
        return (0, 0);
    };

    if !cache_dir.exists() {
        return (0, 0);
    }

    let mut entry_count = 0u64;
    let mut total_size = 0u64;

    // Walk through sha256/ subdirectory
    if let Ok(entries) = fs::read_dir(cache_dir.join("sha256")) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                entry_count += 1;
                if let Ok(meta) = entry.metadata() {
                    total_size += meta.len();
                }
            }
        }
    }

    (entry_count, total_size)
}

/// Clear OCI cache.
fn clear_oci_cache() -> (u64, u64) {
    let Some(cache_dir) = get_oci_cache_dir() else {
        return (0, 0);
    };

    if !cache_dir.exists() {
        return (0, 0);
    }

    let (count, size) = get_oci_cache_stats();

    if fs::remove_dir_all(&cache_dir).is_ok() {
        (count, size)
    } else {
        (0, 0)
    }
}

/// Get schema cache directory.
fn get_schema_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("mik").join("schemas"))
}

/// Execute cache management command.
pub fn execute(action: CacheAction) -> Result<()> {
    let aot_cache = AotCache::new(AotCacheConfig::default())?;
    let schema_cache = SchemaCache::default_location();

    match action {
        CacheAction::Info => {
            // AOT cache stats
            let stats = aot_cache.stats()?;
            println!("AOT Cache (compiled modules)");
            println!("============================");
            println!("Location:    {}", stats.cache_dir.display());
            println!("Entries:     {}", stats.entry_count);
            println!("Total size:  {} MB", stats.total_size_bytes / (1024 * 1024));
            println!("Max size:    {} MB", stats.max_size_bytes / (1024 * 1024));
            println!(
                "Usage:       {:.1}%",
                (stats.total_size_bytes as f64 / stats.max_size_bytes as f64) * 100.0
            );

            // Schema cache stats
            println!();
            println!("Schema Cache (JSON schemas)");
            println!("===========================");
            if let Some(ref cache) = schema_cache {
                let schema_stats = cache.stats();
                if let Some(dir) = get_schema_cache_dir() {
                    println!("Location:    {}", dir.display());
                }
                println!("Entries:     {}", schema_stats.entries);
                println!("Total size:  {} KB", schema_stats.total_bytes / 1024);
            } else {
                println!("(unavailable - no cache directory)");
            }

            // OCI cache stats
            let (oci_count, oci_size) = get_oci_cache_stats();
            println!();
            println!("OCI Cache (registry artifacts)");
            println!("==============================");
            if let Some(dir) = get_oci_cache_dir() {
                println!("Location:    {}", dir.display());
            }
            println!("Entries:     {oci_count}");
            println!("Total size:  {} KB", oci_size / 1024);
        },
        CacheAction::Clean { max_size_mb } => {
            // Create AOT cache with custom max size for cleanup
            let config = AotCacheConfig {
                max_size_bytes: max_size_mb * 1024 * 1024,
                bypass: false,
            };
            let aot_cache_sized = AotCache::new(config)?;
            let aot_stats = aot_cache_sized.cleanup()?;

            // Clean schema cache (remove entries older than 30 days)
            let schema_removed = if let Some(ref cache) = schema_cache {
                cache.clean(30).unwrap_or(0)
            } else {
                0
            };

            let total_removed = aot_stats.entries_removed + schema_removed;

            if total_removed == 0 {
                println!("Caches are already within size limits. Nothing to clean.");
            } else {
                println!("Caches cleaned successfully");
                println!("  AOT entries removed:    {}", aot_stats.entries_removed);
                println!("  Schema entries removed: {schema_removed}");
                println!(
                    "  AOT space freed:        {} MB",
                    aot_stats.bytes_freed / (1024 * 1024)
                );
            }
        },
        CacheAction::Clear => {
            // Clear AOT cache
            let aot_stats = aot_cache.clear()?;

            // Clear schema cache (get stats before clearing)
            let (schema_count, schema_size) = if let Some(ref cache) = schema_cache {
                let stats = cache.stats();
                let count = stats.entries;
                let size = stats.total_bytes;
                let _ = cache.clear();
                (count as u64, size)
            } else {
                (0, 0)
            };

            // Clear OCI cache
            let (oci_count, oci_size) = clear_oci_cache();

            let total_removed = aot_stats.entries_removed as u64 + schema_count + oci_count;
            let total_freed = aot_stats.bytes_freed + schema_size + oci_size;

            if total_removed == 0 {
                println!("Caches are already empty.");
            } else {
                println!("Caches cleared successfully");
                println!("  AOT entries removed:    {}", aot_stats.entries_removed);
                println!("  Schema entries removed: {schema_count}");
                println!("  OCI entries removed:    {oci_count}");
                println!(
                    "  Total space freed:      {} MB",
                    total_freed / (1024 * 1024)
                );
            }
        },
    }

    Ok(())
}
