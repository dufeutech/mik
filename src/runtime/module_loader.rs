//! Module loading and caching for WASM components.
//!
//! This module provides:
//! - [`CachedComponent`]: A WASM component with cached size information
//! - [`ModuleCache`]: LRU cache with byte-aware eviction using moka
//! - [`load_component`]: Load a WASM component from disk with AOT caching

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use moka::sync::Cache as MokaCache;
use tracing::{debug, info};
use wasmtime::Engine;
use wasmtime::component::Component;

use crate::runtime::aot_cache::AotCache;
use crate::runtime::error;
use crate::runtime::security;

/// Component with cached size information for byte-aware eviction.
pub(crate) struct CachedComponent {
    pub(crate) component: Arc<Component>,
    pub(crate) size_bytes: usize,
}

/// Module cache with byte-aware eviction using moka.
/// Uses weigher function to ensure total bytes don't exceed limit.
pub(crate) type ModuleCache = MokaCache<String, Arc<CachedComponent>>;

/// Create a new module cache with byte-aware eviction.
///
/// # Arguments
/// * `max_cache_bytes` - Maximum total size in bytes for cached components
///
/// # Returns
/// A configured moka cache with LRU eviction based on component size
pub(crate) fn create_module_cache(max_cache_bytes: usize) -> ModuleCache {
    MokaCache::builder()
        .max_capacity(max_cache_bytes as u64)
        .weigher(|_key: &String, value: &Arc<CachedComponent>| -> u32 {
            value.size_bytes.min(u32::MAX as usize) as u32
        })
        .time_to_idle(Duration::from_secs(3600))
        .build()
}

/// Load a WASM component from disk.
///
/// This function:
/// 1. Sanitizes the module name to prevent path traversal
/// 2. Checks the in-memory cache first
/// 3. Falls back to AOT cache if available
/// 4. Compiles from source bytes if no cache hit
/// 5. Stores compiled artifacts in both caches
///
/// # Arguments
/// * `name` - The module name (without .wasm extension)
/// * `modules_dir` - Directory containing WASM modules
/// * `engine` - Wasmtime engine for compilation
/// * `aot_cache` - AOT cache for compiled artifacts
/// * `cache` - In-memory module cache
///
/// # Returns
/// An Arc-wrapped Component ready for instantiation
///
/// # Errors
/// - If the module name is invalid (path traversal attempt)
/// - If the module file doesn't exist
/// - If compilation fails
#[allow(unsafe_code)] // SAFETY: Component::deserialize_file requires unsafe for AOT cache
pub(crate) async fn load_component(
    name: &str,
    modules_dir: &Path,
    engine: &Engine,
    aot_cache: &AotCache,
    cache: &ModuleCache,
) -> Result<Arc<Component>> {
    // Security: sanitize module name to prevent path traversal
    let sanitized_name = security::sanitize_module_name(name).map_err(|e| {
        error::Error::InvalidRequest(format!("Invalid module name '{name}': {e}")).into_anyhow()
    })?;

    // Check cache first (no lock needed - moka is thread-safe)
    if let Some(cached) = cache.get(&sanitized_name) {
        debug!("Cache hit: {}", sanitized_name);
        return Ok(cached.component.clone());
    }

    // Load from disk (async I/O)
    let path = modules_dir.join(format!("{sanitized_name}.wasm"));
    if !tokio::fs::try_exists(&path).await? {
        return Err(error::Error::module_not_found(&sanitized_name).into_anyhow());
    }

    // Get file size for byte-aware cache eviction (async I/O)
    let file_size = tokio::fs::metadata(&path)
        .await
        .map(|m| m.len() as usize)
        .unwrap_or(0);

    info!("Loading module: {} ({} bytes)", sanitized_name, file_size);

    // Read WASM bytes for content-addressable caching
    let wasm_bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let engine = engine.clone();
    let aot_cache = aot_cache.clone();

    // CPU-intensive component compilation - use spawn_blocking to avoid blocking the runtime
    let component = tokio::task::spawn_blocking(move || -> anyhow::Result<Component> {
        // Try content-addressable AOT cache first (unless in hot-reload mode)
        if let Some(cached_path) = aot_cache.get(&wasm_bytes) {
            // SAFETY: We compiled this file ourselves with the same engine configuration
            match unsafe { Component::deserialize_file(&engine, &cached_path) } {
                Ok(component) => {
                    tracing::debug!("AOT cache hit: {}", cached_path.display());
                    return Ok(component);
                },
                Err(e) => {
                    // AOT cache invalid (e.g., engine version changed), recompile
                    tracing::warn!("AOT cache invalid, recompiling: {}", e);
                    // Remove invalid cache entry
                    let _ = aot_cache.remove(&wasm_bytes);
                },
            }
        }

        // Compile from bytes
        let component = Component::from_binary(&engine, &wasm_bytes)?;

        // Store in content-addressable cache (unless in hot-reload mode)
        if !aot_cache.is_bypass() {
            match component.serialize() {
                Ok(serialized) => match aot_cache.put(&wasm_bytes, &serialized) {
                    Ok(path) => {
                        tracing::debug!("Cached AOT: {}", path.display());
                    },
                    Err(e) => {
                        tracing::warn!("Failed to cache AOT: {}", e);
                    },
                },
                Err(e) => {
                    tracing::warn!("Failed to serialize for AOT cache: {}", e);
                },
            }
        }

        Ok(component)
    })
    .await
    .context("Task join failed")?
    .with_context(|| format!("Failed to load {}", path.display()))?;

    let component = Arc::new(component);

    // Cache it with size tracking (moka handles eviction automatically)
    let cached_component = Arc::new(CachedComponent {
        component: component.clone(),
        size_bytes: file_size,
    });
    cache.insert(sanitized_name.clone(), cached_component);

    debug!(
        "Cache stats: {} entries, ~{} bytes total",
        cache.entry_count(),
        cache.weighted_size()
    );

    Ok(component)
}

/// Get cache statistics.
///
/// # Arguments
/// * `cache` - The module cache to get stats from
///
/// # Returns
/// A tuple of (entry_count, total_bytes)
pub(crate) fn cache_stats(cache: &ModuleCache) -> (u64, u64) {
    (cache.entry_count(), cache.weighted_size())
}

/// Get list of cached module names.
///
/// # Arguments
/// * `cache` - The module cache to enumerate
///
/// # Returns
/// A vector of module names currently in cache
pub(crate) fn get_cached_module_names(cache: &ModuleCache) -> Vec<String> {
    cache.run_pending_tasks();
    let cache_size = cache.entry_count() as usize;
    let mut modules = Vec::with_capacity(cache_size);
    for (key, _) in cache {
        modules.push((*key).clone());
    }
    modules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_module_cache() {
        let cache = create_module_cache(1024 * 1024); // 1MB
        assert_eq!(cache.entry_count(), 0);
        assert_eq!(cache.weighted_size(), 0);
    }

    #[test]
    fn test_cache_stats_empty() {
        let cache = create_module_cache(1024 * 1024);
        let (count, bytes) = cache_stats(&cache);
        assert_eq!(count, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn test_get_cached_module_names_empty() {
        let cache = create_module_cache(1024 * 1024);
        let names = get_cached_module_names(&cache);
        assert!(names.is_empty());
    }
}
