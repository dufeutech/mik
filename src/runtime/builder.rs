//! Host builder and configuration loading.
//!
//! This module provides the [`HostBuilder`] for creating configured [`Host`] instances.
//! Configuration can be loaded from mik.toml manifest files or set programmatically.

use crate::constants;
use crate::runtime::host_config::HostConfig;
use crate::runtime::{
    DEFAULT_CACHE_SIZE, DEFAULT_EXECUTION_TIMEOUT_SECS, DEFAULT_MAX_CACHE_MB,
    DEFAULT_MAX_CONCURRENT_REQUESTS, DEFAULT_MAX_PER_MODULE_REQUESTS, DEFAULT_MEMORY_LIMIT_BYTES,
    DEFAULT_SHUTDOWN_TIMEOUT_SECS,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::info;

use super::Host;

/// Default max request body size in MB.
const DEFAULT_MAX_BODY_SIZE_MB: usize = constants::MAX_BODY_SIZE_BYTES / (1024 * 1024);

/// Partial mik.toml manifest - only reads what the host needs.
#[derive(Debug, Deserialize)]
struct PartialManifest {
    #[serde(default)]
    server: ManifestServerConfig,
}

/// Server configuration from mik.toml [server] section.
#[derive(Debug, Default, Deserialize)]
struct ManifestServerConfig {
    #[serde(default = "default_auto")]
    auto: bool,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_modules_dir")]
    modules: String,
    #[serde(default)]
    r#static: Option<String>,
    #[serde(default)]
    cache_size: usize,
    #[serde(default)]
    max_cache_mb: usize,
    #[serde(default = "default_execution_timeout_secs")]
    execution_timeout_secs: u64,
    #[serde(default = "default_memory_limit_bytes")]
    memory_limit_bytes: usize,
    #[serde(default)]
    max_concurrent_requests: usize,
    #[serde(default = "default_max_body_size_mb")]
    max_body_size_mb: usize,
    #[serde(default)]
    max_per_module_requests: usize,
    #[serde(default = "default_shutdown_timeout_secs")]
    shutdown_timeout_secs: u64,
    #[serde(default)]
    logging: bool,
    #[serde(default)]
    http_allowed: Vec<String>,
    #[serde(default)]
    scripts: Option<String>,
}

const fn default_auto() -> bool {
    true
}

const fn default_port() -> u16 {
    constants::DEFAULT_PORT
}

fn default_modules_dir() -> String {
    format!("{}/", constants::DEFAULT_MODULES_DIR)
}

const fn default_execution_timeout_secs() -> u64 {
    DEFAULT_EXECUTION_TIMEOUT_SECS
}

const fn default_memory_limit_bytes() -> usize {
    DEFAULT_MEMORY_LIMIT_BYTES
}

const fn default_max_body_size_mb() -> usize {
    DEFAULT_MAX_BODY_SIZE_MB
}

const fn default_shutdown_timeout_secs() -> u64 {
    DEFAULT_SHUTDOWN_TIMEOUT_SECS
}

/// Auto-detected system configuration.
#[derive(Debug)]
struct SystemConfig {
    /// Total system RAM in bytes.
    total_memory_bytes: u64,
    /// Number of CPU cores.
    cpu_cores: usize,
    /// Auto-detected cache size (modules).
    cache_size: usize,
    /// Auto-detected cache memory in bytes.
    max_cache_bytes: usize,
    /// Auto-detected max concurrent requests.
    max_concurrent_requests: usize,
    /// Auto-detected max per-module requests.
    max_per_module_requests: usize,
}

impl SystemConfig {
    /// Detect system resources and compute optimal configuration.
    fn detect() -> Self {
        use sysinfo::System;

        let mut sys = System::new();
        sys.refresh_memory();

        let total_memory_bytes = sys.total_memory();
        let cpu_cores = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);

        // Cache size: ~1 module per 10MB of RAM, capped at 1000
        let cache_size = ((total_memory_bytes / (10 * 1024 * 1024)) as usize).clamp(10, 1000);

        // Cache memory: 10% of RAM, capped at 1GB
        let max_cache_bytes =
            ((total_memory_bytes / 10) as usize).clamp(64 * 1024 * 1024, 1024 * 1024 * 1024);

        // Max concurrent: 100-200 per core, minimum 100
        let max_concurrent_requests = (cpu_cores * 150).clamp(100, 10000);

        // Per-module: 10% of max concurrent, minimum 10
        let max_per_module_requests = (max_concurrent_requests / 10).clamp(10, 500);

        Self {
            total_memory_bytes,
            cpu_cores,
            cache_size,
            max_cache_bytes,
            max_concurrent_requests,
            max_per_module_requests,
        }
    }

    /// Log the detected configuration.
    fn log(&self) {
        let ram_gb = self.total_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let cache_mb = self.max_cache_bytes / (1024 * 1024);
        info!(
            "Auto-configured for system: {} cores, {:.1} GB RAM",
            self.cpu_cores, ram_gb
        );
        info!(
            "  cache_size: {} modules, max_cache: {} MB",
            self.cache_size, cache_mb
        );
        info!(
            "  max_concurrent: {}, max_per_module: {}",
            self.max_concurrent_requests, self.max_per_module_requests
        );
    }
}

/// Builder for creating a [`Host`].
///
/// Provides a fluent API for configuring the WASI HTTP runtime.
///
/// # Examples
///
/// Basic usage with programmatic configuration:
///
/// ```no_run
/// use mik::runtime::HostBuilder;
/// use std::net::SocketAddr;
///
/// # async fn example() -> anyhow::Result<()> {
/// let host = HostBuilder::new()
///     .modules_dir("modules/")
///     .port(3000)
///     .cache_size(100)
///     .execution_timeout(30)
///     .build()?;
///
/// let addr: SocketAddr = "127.0.0.1:3000".parse()?;
/// host.serve(addr).await?;
/// # Ok(())
/// # }
/// ```
///
/// Loading from a manifest file:
///
/// ```no_run
/// use mik::runtime::HostBuilder;
///
/// # fn example() -> anyhow::Result<()> {
/// let host = HostBuilder::from_manifest("mik.toml")?
///     .port(8080)  // Override port from manifest
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
#[must_use]
pub struct HostBuilder {
    config: HostConfig,
}

#[allow(dead_code)]
impl HostBuilder {
    /// Create a new builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder from a mik.toml manifest file.
    ///
    /// Reads the `[server]` section from the manifest:
    /// ```toml
    /// [server]
    /// port = 3000
    /// modules = "modules/"
    /// static = "collected-static/"
    /// cache_size = 10
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The manifest file cannot be read (IO error)
    /// - The manifest contains invalid TOML syntax
    /// - Required fields are missing or have invalid types
    pub fn from_manifest(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let manifest: PartialManifest = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        let server = manifest.server;

        // Apply auto-detection for values that are 0 (meaning "auto")
        let (cache_size, max_cache_bytes, max_concurrent_requests, max_per_module_requests) =
            if server.auto {
                let sys = SystemConfig::detect();
                sys.log();

                // Use explicit values if set (non-zero), otherwise use auto-detected
                let cache_size = if server.cache_size > 0 {
                    server.cache_size
                } else {
                    sys.cache_size
                };
                let max_cache_bytes = if server.max_cache_mb > 0 {
                    server.max_cache_mb * 1024 * 1024
                } else {
                    sys.max_cache_bytes
                };
                let max_concurrent = if server.max_concurrent_requests > 0 {
                    server.max_concurrent_requests
                } else {
                    sys.max_concurrent_requests
                };
                let max_per_module = if server.max_per_module_requests > 0 {
                    server.max_per_module_requests
                } else {
                    sys.max_per_module_requests
                };

                (cache_size, max_cache_bytes, max_concurrent, max_per_module)
            } else {
                // Auto disabled: use explicit values or static defaults
                let cache_size = if server.cache_size > 0 {
                    server.cache_size
                } else {
                    DEFAULT_CACHE_SIZE
                };
                let max_cache_bytes = if server.max_cache_mb > 0 {
                    server.max_cache_mb * 1024 * 1024
                } else {
                    DEFAULT_MAX_CACHE_MB * 1024 * 1024
                };
                let max_concurrent = if server.max_concurrent_requests > 0 {
                    server.max_concurrent_requests
                } else {
                    DEFAULT_MAX_CONCURRENT_REQUESTS
                };
                let max_per_module = if server.max_per_module_requests > 0 {
                    server.max_per_module_requests
                } else {
                    DEFAULT_MAX_PER_MODULE_REQUESTS
                };

                (cache_size, max_cache_bytes, max_concurrent, max_per_module)
            };

        let config = HostConfig {
            modules_path: PathBuf::from(&server.modules),
            cache_size,
            max_cache_bytes,
            static_dir: server.r#static.map(PathBuf::from),
            port: server.port,
            execution_timeout_secs: server.execution_timeout_secs,
            memory_limit_bytes: server.memory_limit_bytes,
            max_concurrent_requests,
            max_body_size_bytes: server.max_body_size_mb * 1024 * 1024,
            max_per_module_requests,
            shutdown_timeout_secs: server.shutdown_timeout_secs,
            logging_enabled: server.logging,
            http_allowed: server.http_allowed,
            scripts_dir: server.scripts.clone().map(PathBuf::from),
            hot_reload: false,   // Can be overridden via builder
            aot_cache_max_mb: 0, // Use default (1GB)
            fuel_budget: None,   // None = use DEFAULT_FUEL_BUDGET
        };

        // Debug: log scripts configuration
        if let Some(ref scripts) = server.scripts {
            tracing::debug!("Scripts directory configured: {}", scripts);
        }

        Ok(Self { config })
    }

    /// Try to load from mik.toml if it exists, otherwise use defaults.
    pub fn from_manifest_or_default() -> Self {
        Self::from_manifest("mik.toml").unwrap_or_default()
    }

    /// Set the modules directory or single component path.
    pub fn modules_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.modules_path = path.into();
        self
    }

    /// Set the LRU cache size.
    pub const fn cache_size(mut self, size: usize) -> Self {
        self.config.cache_size = size;
        self
    }

    /// Set the static files directory.
    pub fn static_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.static_dir = Some(path.into());
        self
    }

    /// Set the port (used when not overridden by CLI).
    pub const fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set the execution timeout in seconds.
    pub const fn execution_timeout(mut self, timeout_secs: u64) -> Self {
        self.config.execution_timeout_secs = timeout_secs;
        self
    }

    /// Set the memory limit per request in bytes.
    pub const fn memory_limit(mut self, limit_bytes: usize) -> Self {
        self.config.memory_limit_bytes = limit_bytes;
        self
    }

    /// Set the maximum concurrent requests.
    pub const fn max_concurrent_requests(mut self, max: usize) -> Self {
        self.config.max_concurrent_requests = max;
        self
    }

    /// Set the maximum request body size in bytes.
    pub const fn max_body_size(mut self, max_bytes: usize) -> Self {
        self.config.max_body_size_bytes = max_bytes;
        self
    }

    /// Set the maximum concurrent requests per module.
    pub const fn max_per_module_requests(mut self, max: usize) -> Self {
        self.config.max_per_module_requests = max;
        self
    }

    /// Enable wasi:logging for WASM modules.
    pub const fn logging(mut self, enabled: bool) -> Self {
        self.config.logging_enabled = enabled;
        self
    }

    /// Set the allowed hosts for outgoing HTTP requests.
    pub fn http_allowed(mut self, hosts: Vec<String>) -> Self {
        self.config.http_allowed = hosts;
        self
    }

    /// Enable hot-reload mode (bypasses persistent AOT cache).
    pub const fn hot_reload(mut self, enabled: bool) -> Self {
        self.config.hot_reload = enabled;
        self
    }

    /// Set the maximum AOT cache size in MB.
    pub const fn aot_cache_max_mb(mut self, max_mb: usize) -> Self {
        self.config.aot_cache_max_mb = max_mb;
        self
    }

    /// Get the configured port.
    pub const fn get_port(&self) -> u16 {
        self.config.port
    }

    /// Build the host with the configured settings.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The Wasmtime engine fails to initialize
    /// - The modules directory does not exist
    /// - A single-component path is specified but the file cannot be loaded
    pub fn build(self) -> Result<Host> {
        Host::new(self.config)
    }
}
