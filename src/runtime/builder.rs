//! Runtime builder and configuration loading.
//!
//! This module provides [`RuntimeBuilder`] for creating configured runtime instances.
//!
//! # Examples
//!
//! ## From Manifest File
//!
//! ```no_run
//! use mik::runtime::Runtime;
//!
//! # fn example() -> anyhow::Result<()> {
//! let runtime = Runtime::builder()
//!     .from_manifest_file("mik.toml")?
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Programmatic Configuration
//!
//! ```no_run
//! use mik::runtime::Runtime;
//!
//! # fn example() -> anyhow::Result<()> {
//! let runtime = Runtime::builder()
//!     .modules_dir("modules/")
//!     .cache_size(100)
//!     .execution_timeout(std::time::Duration::from_secs(30))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## From Manifest Struct
//!
//! ```no_run
//! use mik::runtime::Runtime;
//! use mik::manifest::Manifest;
//!
//! # fn example() -> anyhow::Result<()> {
//! let manifest = Manifest::default();
//! let runtime = Runtime::builder()
//!     .from_manifest(&manifest)
//!     .build()?;
//! # Ok(())
//! # }
//! ```

use crate::constants;
use crate::manifest::{Manifest, ServerConfig};
use crate::runtime::host_config::HostConfig;
use crate::runtime::{
    DEFAULT_CACHE_SIZE, DEFAULT_EXECUTION_TIMEOUT_SECS, DEFAULT_MAX_CACHE_MB,
    DEFAULT_MAX_CONCURRENT_REQUESTS, DEFAULT_MAX_PER_MODULE_REQUESTS, DEFAULT_MEMORY_LIMIT_BYTES,
    DEFAULT_SHUTDOWN_TIMEOUT_SECS,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::info;

use super::{Host, Runtime};

/// Default max request body size in MB.
const DEFAULT_MAX_BODY_SIZE_MB: usize = constants::MAX_BODY_SIZE_BYTES / (1024 * 1024);

/// Partial mik.toml manifest - only reads what the host needs.
#[derive(Debug, Deserialize)]
struct PartialManifest {
    #[serde(default)]
    server: TomlServerConfig,
}

/// Server configuration from mik.toml [server] section.
/// Note: This is separate from `crate::manifest::ServerConfig` as it includes
/// additional parsing helpers like serde defaults.
#[derive(Debug, Default, Deserialize)]
struct TomlServerConfig {
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

/// Builder for creating a [`Runtime`] instance.
///
/// This is the recommended way to create a `Runtime`. The `RuntimeBuilder` creates
/// a `Runtime` that doesn't bind to a network address, making it suitable for
/// both CLI use (via [`Server`](super::Server)) and embedding in applications.
///
/// # Examples
///
/// ## From Manifest File
///
/// ```no_run
/// use mik::runtime::Runtime;
///
/// # fn example() -> anyhow::Result<()> {
/// let runtime = Runtime::builder()
///     .from_manifest_file("mik.toml")?
///     .build()?;
/// # Ok(())
/// # }
/// ```
///
/// ## From Manifest Struct
///
/// ```no_run
/// use mik::runtime::Runtime;
/// use mik::manifest::Manifest;
///
/// # fn example() -> anyhow::Result<()> {
/// let manifest = Manifest::default();
/// let runtime = Runtime::builder()
///     .from_manifest(&manifest)
///     .build()?;
/// # Ok(())
/// # }
/// ```
///
/// ## Programmatic Configuration
///
/// ```no_run
/// use mik::runtime::Runtime;
/// use std::time::Duration;
///
/// # fn example() -> anyhow::Result<()> {
/// let runtime = Runtime::builder()
///     .modules_dir("modules/")
///     .cache_size(100)
///     .execution_timeout(Duration::from_secs(30))
///     .max_concurrent_requests(500)
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
#[must_use]
pub struct RuntimeBuilder {
    config: HostConfig,
}

impl RuntimeBuilder {
    /// Create a new builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load configuration from a manifest file.
    ///
    /// This reads the `[server]` section from a mik.toml file and applies
    /// the configuration to this builder. Subsequent builder methods can
    /// override specific settings.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::runtime::Runtime;
    ///
    /// # fn example() -> anyhow::Result<()> {
    /// let runtime = Runtime::builder()
    ///     .from_manifest_file("mik.toml")?
    ///     .port(8080)  // Override the port
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The manifest file cannot be read
    /// - The manifest contains invalid TOML syntax
    #[allow(clippy::wrong_self_convention)] // Builder method, not a From impl
    pub fn from_manifest_file(self, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let manifest: PartialManifest = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        Ok(self.apply_manifest_server_config(&manifest.server))
    }

    /// Load configuration from a `Manifest` struct.
    ///
    /// This allows programmatic construction of the manifest without
    /// reading from a file, useful for embedded applications.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::runtime::Runtime;
    /// use mik::manifest::{Manifest, ServerConfig};
    ///
    /// # fn example() -> anyhow::Result<()> {
    /// let manifest = Manifest {
    ///     server: ServerConfig {
    ///         port: 8080,
    ///         modules: "my-modules/".to_string(),
    ///         ..Default::default()
    ///     },
    ///     ..Default::default()
    /// };
    ///
    /// let runtime = Runtime::builder()
    ///     .from_manifest(&manifest)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    #[allow(clippy::wrong_self_convention)] // Builder method, not a From impl
    pub fn from_manifest(self, manifest: &Manifest) -> Self {
        self.from_server_config(&manifest.server)
    }

    /// Load configuration from a `ServerConfig` struct.
    ///
    /// This is useful when you only have the server section of the manifest.
    #[allow(clippy::wrong_self_convention)] // Builder method, not a From impl
    pub fn from_server_config(mut self, server: &ServerConfig) -> Self {
        // Apply auto-detection for values that are 0 (meaning "auto")
        let (cache_size, max_cache_bytes, max_concurrent_requests, max_per_module_requests) =
            if server.auto {
                let sys = SystemConfig::detect();
                sys.log();

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

        self.config = HostConfig {
            modules_path: PathBuf::from(&server.modules),
            cache_size,
            max_cache_bytes,
            static_dir: server.r#static.clone().map(PathBuf::from),
            port: server.port,
            execution_timeout_secs: server.execution_timeout_secs,
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            max_concurrent_requests,
            max_body_size_bytes: server.max_body_size_mb * 1024 * 1024,
            max_per_module_requests,
            shutdown_timeout_secs: server.shutdown_timeout_secs,
            logging_enabled: server.logging,
            http_allowed: server.http_allowed.clone(),
            scripts_dir: server.scripts.clone().map(PathBuf::from),
            hot_reload: false,
            aot_cache_max_mb: 0,
            fuel_budget: None,
        };

        self
    }

    /// Apply configuration from a `TomlServerConfig` (internal helper).
    fn apply_manifest_server_config(mut self, server: &TomlServerConfig) -> Self {
        // Apply auto-detection for values that are 0 (meaning "auto")
        let (cache_size, max_cache_bytes, max_concurrent_requests, max_per_module_requests) =
            if server.auto {
                let sys = SystemConfig::detect();
                sys.log();

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

        self.config = HostConfig {
            modules_path: PathBuf::from(&server.modules),
            cache_size,
            max_cache_bytes,
            static_dir: server.r#static.clone().map(PathBuf::from),
            port: server.port,
            execution_timeout_secs: server.execution_timeout_secs,
            memory_limit_bytes: server.memory_limit_bytes,
            max_concurrent_requests,
            max_body_size_bytes: server.max_body_size_mb * 1024 * 1024,
            max_per_module_requests,
            shutdown_timeout_secs: server.shutdown_timeout_secs,
            logging_enabled: server.logging,
            http_allowed: server.http_allowed.clone(),
            scripts_dir: server.scripts.clone().map(PathBuf::from),
            hot_reload: false,
            aot_cache_max_mb: 0,
            fuel_budget: None,
        };

        self
    }

    /// Try to load from mik.toml if it exists, otherwise use defaults.
    #[allow(clippy::wrong_self_convention)] // Builder method, not a From impl
    pub fn from_manifest_file_or_default(self) -> Self {
        let path = std::path::Path::new("mik.toml");
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(path)
            && let Ok(manifest) = toml::from_str::<PartialManifest>(&content)
        {
            return self.apply_manifest_server_config(&manifest.server);
        }
        self
    }

    /// Set the modules directory or single component path.
    pub fn modules_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.modules_path = path.into();
        self
    }

    /// Set the LRU cache size (number of modules).
    pub const fn cache_size(mut self, size: usize) -> Self {
        self.config.cache_size = size;
        self
    }

    /// Set the maximum cache memory in bytes.
    pub const fn max_cache_bytes(mut self, bytes: usize) -> Self {
        self.config.max_cache_bytes = bytes;
        self
    }

    /// Set the static files directory.
    pub fn static_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.static_dir = Some(path.into());
        self
    }

    /// Set the scripts directory for JS orchestration.
    pub fn scripts_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.scripts_dir = Some(path.into());
        self
    }

    /// Set the port (for reference, not used by Runtime directly).
    pub const fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set the execution timeout.
    pub fn execution_timeout(mut self, timeout: Duration) -> Self {
        self.config.execution_timeout_secs = timeout.as_secs();
        self
    }

    /// Set the execution timeout in seconds.
    pub const fn execution_timeout_secs(mut self, timeout_secs: u64) -> Self {
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

    /// Set the graceful shutdown timeout.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout_secs = timeout.as_secs();
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

    /// Set the fuel budget per request for CPU limiting.
    pub const fn fuel_budget(mut self, budget: u64) -> Self {
        self.config.fuel_budget = Some(budget);
        self
    }

    /// Get the configured port.
    pub const fn get_port(&self) -> u16 {
        self.config.port
    }

    /// Get a reference to the configuration.
    pub const fn config(&self) -> &HostConfig {
        &self.config
    }

    /// Build the runtime with the configured settings.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The Wasmtime engine fails to initialize
    /// - The modules directory does not exist
    /// - A single-component path is specified but the file cannot be loaded
    pub fn build(self) -> Result<Runtime> {
        let host = Host::new(self.config)?;
        Ok(Runtime::from_host(host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_builder_default() {
        let builder = RuntimeBuilder::new();
        assert_eq!(builder.config.port, constants::DEFAULT_PORT);
    }

    #[test]
    fn test_runtime_builder_modules_dir() {
        let builder = RuntimeBuilder::new().modules_dir("custom/modules");
        assert_eq!(builder.config.modules_path, PathBuf::from("custom/modules"));
    }

    #[test]
    fn test_runtime_builder_cache_size() {
        let builder = RuntimeBuilder::new().cache_size(200);
        assert_eq!(builder.config.cache_size, 200);
    }

    #[test]
    fn test_runtime_builder_execution_timeout() {
        let builder = RuntimeBuilder::new().execution_timeout(Duration::from_secs(60));
        assert_eq!(builder.config.execution_timeout_secs, 60);
    }

    #[test]
    fn test_runtime_builder_from_manifest() {
        let manifest = Manifest::default();
        let builder = RuntimeBuilder::new().from_manifest(&manifest);
        assert_eq!(builder.config.port, constants::DEFAULT_PORT);
    }

    #[test]
    fn test_runtime_builder_chaining() {
        let builder = RuntimeBuilder::new()
            .modules_dir("modules/")
            .cache_size(100)
            .port(8080)
            .execution_timeout(Duration::from_secs(30))
            .max_concurrent_requests(500)
            .hot_reload(true);

        assert_eq!(builder.config.modules_path, PathBuf::from("modules/"));
        assert_eq!(builder.config.cache_size, 100);
        assert_eq!(builder.config.port, 8080);
        assert_eq!(builder.config.execution_timeout_secs, 30);
        assert_eq!(builder.config.max_concurrent_requests, 500);
        assert!(builder.config.hot_reload);
    }
}
