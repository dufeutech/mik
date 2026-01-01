//! Module cache management.
//!
//! This module provides the module cache and component loading functionality:
//! - `CachedComponent`: Component with size tracking for byte-aware eviction
//! - `ModuleCache`: LRU cache using moka with byte-based eviction
//! - Module loading with AOT cache integration

use super::SharedState;
use super::error;
use super::security;
use anyhow::{Context, Result};
use moka::sync::Cache as MokaCache;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, info};
use wasmtime::component::Component;

/// Component with cached size information for byte-aware eviction.
pub(crate) struct CachedComponent {
    pub(crate) component: Arc<Component>,
    pub(crate) size_bytes: usize,
}

/// Module cache with byte-aware eviction using moka.
/// Uses weigher function to ensure total bytes don't exceed limit.
pub(crate) type ModuleCache = MokaCache<String, Arc<CachedComponent>>;

impl SharedState {
    /// Get or create a semaphore for a specific module.
    pub(crate) fn get_module_semaphore(&self, module_name: &str) -> Arc<Semaphore> {
        // Fast path: read-only check without allocation
        {
            let semaphores = self.module_semaphores.lock();
            if let Some(sem) = semaphores.get(module_name) {
                return sem.clone();
            }
        }
        // Slow path: allocate and insert
        let mut semaphores = self.module_semaphores.lock();
        // Double-check after re-acquiring lock
        if let Some(sem) = semaphores.get(module_name) {
            return sem.clone();
        }
        debug!(
            "Creating semaphore for module '{}' with limit {}",
            module_name, self.config.max_per_module_requests
        );
        let sem = Arc::new(Semaphore::new(self.config.max_per_module_requests));
        semaphores.insert(module_name.to_string(), sem.clone());
        sem
    }

    /// Get or load a module by name (async to avoid blocking the runtime).
    #[allow(unsafe_code)] // SAFETY: Component::deserialize_file requires unsafe for AOT cache
    pub(crate) async fn get_or_load(&self, name: &str) -> Result<Arc<Component>> {
        // Security: sanitize module name to prevent path traversal
        let sanitized_name = security::sanitize_module_name(name).map_err(|e| {
            error::Error::InvalidRequest(format!("Invalid module name '{name}': {e}")).into_anyhow()
        })?;

        // Check cache first (no lock needed - moka is thread-safe)
        if let Some(cached) = self.cache.get(&sanitized_name) {
            debug!("Cache hit: {}", sanitized_name);
            return Ok(cached.component.clone());
        }

        // Load from disk (async I/O)
        let path = self.modules_dir.join(format!("{sanitized_name}.wasm"));
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

        let engine = self.engine.clone();
        let aot_cache = self.aot_cache.clone();

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
        self.cache.insert(sanitized_name.clone(), cached_component);

        debug!(
            "Cache stats: {} entries, ~{} bytes total",
            self.cache.entry_count(),
            self.cache.weighted_size()
        );

        Ok(component)
    }
}
