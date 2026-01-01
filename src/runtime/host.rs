//! Internal WASM host initialization.
//!
//! This module contains the `Host` struct which manages wasmtime engine setup,
//! epoch interruption threads, and module loading configuration.

use super::aot_cache;
use super::error;
use super::host_config::HostConfig;
use super::host_state::HostState;
use super::reliability;
use super::{CachedComponent, ModuleCache, SharedState};
use crate::constants;
use anyhow::{Context, Result};
use moka::sync::Cache as MokaCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig};

/// Internal WASM host that manages the wasmtime engine and module cache.
///
/// This is an internal type used by [`RuntimeBuilder`](super::RuntimeBuilder) to set up the wasmtime
/// environment. Use [`Runtime`](super::Runtime) for the public API.
pub(crate) struct Host {
    pub(crate) shared: Arc<SharedState>,
    /// Shutdown signal for the epoch incrementer thread.
    pub(crate) epoch_shutdown: Arc<AtomicBool>,
}

impl Host {
    /// Create the wasmtime engine with pooling allocator configuration.
    fn create_engine(config: &HostConfig) -> Result<Engine> {
        let mut wasm_config = Config::new();
        wasm_config.wasm_component_model(true);
        wasm_config.async_support(true);
        wasm_config.epoch_interruption(true);
        wasm_config.consume_fuel(true);
        wasm_config.parallel_compilation(true);
        wasm_config.async_stack_zeroing(true);

        let mut pool_config = PoolingAllocationConfig::default();
        pool_config.total_component_instances(config.max_concurrent_requests as u32);
        pool_config.total_stacks(config.max_concurrent_requests as u32);
        pool_config.max_component_instance_size(2 * 1024 * 1024);
        pool_config.max_memory_size(config.memory_limit_bytes);
        pool_config.max_memories_per_component(10);
        pool_config.max_tables_per_component(10);
        wasm_config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool_config));

        Engine::new(&wasm_config).context("Failed to create wasmtime engine")
    }

    /// Start the background epoch incrementer thread.
    fn start_epoch_thread(engine: &Engine) -> Arc<AtomicBool> {
        let epoch_shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_epoch = epoch_shutdown.clone();
        let engine_for_epoch = engine.clone();
        std::thread::spawn(move || {
            while !shutdown_for_epoch.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
                engine_for_epoch.increment_epoch();
            }
        });
        epoch_shutdown
    }

    /// Determine module mode (single component or directory) and load if single.
    fn determine_module_mode(
        config: &HostConfig,
        engine: &Engine,
    ) -> Result<(PathBuf, Option<Arc<Component>>, Option<String>)> {
        if config.modules_path.is_file() {
            info!("Single component mode: {}", config.modules_path.display());
            let component = Component::from_file(engine, &config.modules_path)
                .context("Failed to load component")?;

            let name = config
                .modules_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map_or_else(
                    || "component".to_string(),
                    |s| s.strip_suffix("-composed").unwrap_or(s).to_string(),
                );

            let modules_dir = config
                .modules_path
                .parent()
                .unwrap_or(&config.modules_path)
                .to_path_buf();

            Ok((modules_dir, Some(Arc::new(component)), Some(name)))
        } else if config.modules_path.is_dir() {
            info!("Multi-module mode: {}", config.modules_path.display());
            info!(
                "Modules will be loaded on-demand (cache size: {})",
                config.cache_size
            );

            let available: Vec<_> = std::fs::read_dir(&config.modules_path)?
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "wasm") {
                        path.file_stem().and_then(|s| s.to_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect();

            if available.is_empty() {
                return Err(error::Error::Config(format!(
                    "No .wasm files found in {}",
                    config.modules_path.display()
                ))
                .into_anyhow());
            }

            info!("Available modules: {}", available.join(", "));
            Ok((config.modules_path.clone(), None, None))
        } else {
            Err(error::Error::Config(format!(
                "Path does not exist: {}",
                config.modules_path.display()
            ))
            .into_anyhow())
        }
    }

    /// Create the AOT cache based on configuration.
    fn create_aot_cache(config: &HostConfig) -> Result<aot_cache::AotCache> {
        if config.hot_reload {
            info!("Hot-reload mode: AOT cache bypassed");
            return Ok(aot_cache::AotCache::bypass());
        }

        let max_bytes = if config.aot_cache_max_mb > 0 {
            (config.aot_cache_max_mb as u64) * 1024 * 1024
        } else {
            constants::DEFAULT_AOT_CACHE_SIZE_BYTES
        };

        let cache = aot_cache::AotCache::new(aot_cache::AotCacheConfig {
            max_size_bytes: max_bytes,
            bypass: false,
        })?;

        info!(
            "AOT cache: ~/.mik/cache/aot/ (max {}MB)",
            max_bytes / 1024 / 1024
        );
        Ok(cache)
    }

    /// Log enabled capabilities.
    fn log_capabilities(config: &HostConfig) {
        if config.logging_enabled {
            info!("Capability: wasi:logging enabled");
        }
        if !config.http_allowed.is_empty() {
            if config.http_allowed.iter().any(|h| h == "*") {
                info!("Capability: wasi:http/outgoing-handler enabled (all hosts)");
            } else {
                info!(
                    "Capability: wasi:http/outgoing-handler enabled ({} hosts)",
                    config.http_allowed.len()
                );
            }
        }
    }

    /// Create a new host with the given configuration.
    pub fn new(config: HostConfig) -> Result<Self> {
        config
            .validate()
            .with_context(|| "Invalid host configuration")?;

        let engine = Self::create_engine(&config)?;
        let epoch_shutdown = Self::start_epoch_thread(&engine);

        let mut linker: Linker<HostState> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;

        // Create moka cache with byte-aware eviction
        let cache: ModuleCache = MokaCache::builder()
            .max_capacity(config.max_cache_bytes as u64)
            .weigher(|_key: &String, value: &Arc<CachedComponent>| -> u32 {
                value.size_bytes.min(u32::MAX as usize) as u32
            })
            .time_to_idle(Duration::from_secs(constants::DEFAULT_AOT_CACHE_TTI_SECS))
            .build();

        let (modules_dir, single_component, single_component_name) =
            Self::determine_module_mode(&config, &engine)?;

        // Validate static directory if provided
        let static_dir = config.static_dir.clone().filter(|dir| {
            if dir.is_dir() {
                info!("Static files: {} -> /static/", dir.display());
                true
            } else {
                warn!("Static directory not found: {}", dir.display());
                false
            }
        });

        Self::log_capabilities(&config);
        let aot_cache = Self::create_aot_cache(&config)?;

        // Resolve fuel budget: use configured value or default
        let fuel_budget = config.fuel_budget.unwrap_or(constants::DEFAULT_FUEL_BUDGET);

        let shared = Arc::new(SharedState {
            engine,
            linker,
            modules_dir,
            cache,
            single_component,
            single_component_name,
            static_dir,
            execution_timeout: Duration::from_secs(config.execution_timeout_secs),
            memory_limit_bytes: config.memory_limit_bytes,
            max_body_size_bytes: config.max_body_size_bytes,
            shutdown: Arc::new(AtomicBool::new(false)),
            request_counter: AtomicU64::new(0),
            circuit_breaker: reliability::CircuitBreaker::new(),
            request_semaphore: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            module_semaphores: Mutex::new(HashMap::new()),
            http_allowed: Arc::new(config.http_allowed.clone()),
            scripts_dir: config.scripts_dir.clone(),
            aot_cache,
            fuel_budget,
            config,
        });

        Ok(Self {
            shared,
            epoch_shutdown,
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Signal the epoch incrementer thread to stop
        self.epoch_shutdown.store(true, Ordering::Relaxed);
    }
}
