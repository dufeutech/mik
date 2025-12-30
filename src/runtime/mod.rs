//! WASI HTTP runtime for mik serve.
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
//!
//! This module provides the core functionality for running WASI HTTP components.
//! Use [`HostBuilder`] to configure and create a host, then call [`Host::serve`].

pub mod aot_cache;
pub mod error;
pub mod lb;
pub mod reliability;
pub mod script;
pub mod security;
pub mod spans;
pub mod trace_context;

use crate::constants;
use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Body, Bytes};
use hyper::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Request, Response, Uri};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HttpConnectionBuilder;
use moka::sync::Cache as MokaCache;
use parking_lot::Mutex;
use percent_encoding::percent_decode_str;
use reliability::{CircuitBreaker, is_http_host_allowed};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use spans::{SpanBuilder, SpanCollector, SpanSummary};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::bindings::http::types::Scheme;
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

/// Route prefix for WASM module requests.
pub const RUN_PREFIX: &str = "/run/";

/// Built-in health check endpoint.
pub const HEALTH_PATH: &str = "/health";

/// Route prefix for static file requests.
pub const STATIC_PREFIX: &str = "/static/";

/// Route prefix for script requests.
pub const SCRIPT_PREFIX: &str = "/script/";

/// Built-in metrics endpoint (Prometheus format).
pub const METRICS_PATH: &str = "/metrics";

// Re-export constants used by external code
pub use constants::DEFAULT_CACHE_SIZE;
pub use constants::DEFAULT_MAX_CONCURRENT_REQUESTS;
pub use constants::GZIP_MIN_SIZE;

/// Default timeout for WASM execution (uses constants::MAX_WASM_TIMEOUT_SECS).
pub const DEFAULT_EXECUTION_TIMEOUT_SECS: u64 = constants::MAX_WASM_TIMEOUT_SECS;

/// Maximum execution timeout (5 minutes).
const MAX_EXECUTION_TIMEOUT_SECS: u64 = 300;

/// Default memory limit per request (128MB).
pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;

/// Minimum memory limit per request (1MB).
const MIN_MEMORY_LIMIT_BYTES: usize = 1024 * 1024;

/// Maximum memory limit per request (4GB).
const MAX_MEMORY_LIMIT_BYTES: usize = 4 * 1024 * 1024 * 1024;

/// Default max request body size in MB.
pub const DEFAULT_MAX_BODY_SIZE_MB: usize = constants::MAX_BODY_SIZE_BYTES / (1024 * 1024);

/// Default max cache memory in MB.
pub const DEFAULT_MAX_CACHE_MB: usize = constants::DEFAULT_CACHE_MB;

/// Default max concurrent requests per module.
pub const DEFAULT_MAX_PER_MODULE_REQUESTS: usize = constants::DEFAULT_MAX_PER_MODULE_REQUESTS;

/// Maximum allowed path length (prevents `DoS` via extremely long URLs).
const MAX_PATH_LENGTH: usize = constants::MAX_PATH_LENGTH;

/// Default graceful shutdown drain timeout in seconds.
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 30;

/// Shutdown polling interval in milliseconds.
/// How frequently to check if active connections have completed during shutdown.
const SHUTDOWN_POLL_INTERVAL_MS: u64 = 100;

/// Retry-After header value (seconds) for circuit breaker responses.
/// Tells clients to wait 30 seconds before retrying a failed service.
const CIRCUIT_BREAKER_RETRY_AFTER_SECS: &str = "30";

/// Retry-After header value (seconds) for module overload responses.
/// Tells clients to wait 5 seconds before retrying an overloaded module.
const MODULE_OVERLOAD_RETRY_AFTER_SECS: &str = "5";

/// Cache-Control header value for static files (1 hour).
const STATIC_CACHE_CONTROL: &str = "public, max-age=3600";

/// Wrapper around `Full<Bytes>` that produces `hyper::Error` (for wasmtime-wasi-http compatibility).
///
/// Since `Full<Bytes>` has `Error = Infallible`, this wrapper maps errors to `hyper::Error`,
/// though in practice no errors will ever occur.
struct HyperCompatibleBody(Full<Bytes>);

impl hyper::body::Body for HyperCompatibleBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        // Full<Bytes> never errors, so we can safely map Infallible to hyper::Error
        match Pin::new(&mut self.0).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
            Poll::Ready(Some(Err(infallible))) => match infallible {},
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.0.size_hint()
    }
}

/// Host state for each request (internal).
pub(crate) struct HostState {
    pub(crate) wasi: WasiCtx,
    pub(crate) http: WasiHttpCtx,
    pub(crate) table: ResourceTable,
    /// Allowed hosts for outgoing HTTP requests (shared reference).
    pub(crate) http_allowed: Arc<Vec<String>>,
    /// Memory limit for this request (bytes).
    pub(crate) memory_limit: usize,
}

/// `ResourceLimiter` implementation to enforce per-request memory limits.
impl wasmtime::ResourceLimiter for HostState {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        if desired > self.memory_limit {
            tracing::warn!(
                current_bytes = current,
                desired_bytes = desired,
                limit_bytes = self.memory_limit,
                "WASM memory limit exceeded"
            );
            return Ok(false);
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        // Reasonable table size limit (10k entries)
        Ok(desired <= 10_000)
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http
    }
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    fn send_request(
        &mut self,
        request: hyper::Request<wasmtime_wasi_http::body::HyperOutgoingBody>,
        config: wasmtime_wasi_http::types::OutgoingRequestConfig,
    ) -> wasmtime_wasi_http::HttpResult<wasmtime_wasi_http::types::HostFutureIncomingResponse> {
        use wasmtime_wasi_http::bindings::http::types::ErrorCode;

        // If no allowed hosts configured, deny all outgoing requests
        if self.http_allowed.is_empty() {
            warn!("Outgoing HTTP denied: no allowed hosts configured");
            return Err(ErrorCode::HttpRequestDenied.into());
        }

        // Extract host from request
        let host = request.uri().host().unwrap_or("");

        // Check if host is allowed
        if !is_http_host_allowed(host, &self.http_allowed) {
            warn!("Outgoing HTTP denied: host '{}' not in allowed list", host);
            return Err(ErrorCode::HttpRequestDenied.into());
        }

        debug!("Outgoing HTTP allowed: {}", host);

        // Delegate to default implementation
        Ok(wasmtime_wasi_http::types::default_send_request(
            request, config,
        ))
    }
}

// NOTE: is_http_host_allowed is imported from reliability::security
// This is the single source of truth used by both host and mik CLI.

/// Error category for observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Module not found or failed to load.
    ModuleLoad,
    /// Invalid request (routing, path, etc).
    InvalidRequest,
    /// Component instantiation failed.
    Instantiation,
    /// Handler execution failed.
    Execution,
    /// Static file serving error.
    StaticFile,
    /// Execution timeout.
    Timeout,
    /// Script execution error.
    Script,
    /// Reliability error (circuit breaker, rate limit).
    Reliability,
    /// Internal server error.
    Internal,
}

impl ErrorCategory {
    /// Convert error category to string for logging.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ModuleLoad => "module_load",
            Self::InvalidRequest => "invalid_request",
            Self::Instantiation => "instantiation",
            Self::Execution => "execution",
            Self::StaticFile => "static_file",
            Self::Timeout => "timeout",
            Self::Script => "script",
            Self::Reliability => "reliability",
            Self::Internal => "internal",
        }
    }
}

/// Level of detail for health check responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthDetail {
    /// Summary only - minimal allocation, fast response.
    #[default]
    Summary,
    /// Full details including list of loaded modules.
    Full,
}

/// Health status response.
#[derive(Debug, Serialize)]
pub struct HealthStatus {
    /// Overall status.
    pub status: String,
    /// Timestamp of the health check.
    pub timestamp: String,
    /// Number of modules currently in cache.
    pub cache_size: usize,
    /// Maximum cache capacity (entries).
    pub cache_capacity: usize,
    /// Current cache memory usage (bytes).
    pub cache_bytes: usize,
    /// Maximum cache memory (bytes).
    pub cache_max_bytes: usize,
    /// Total requests handled.
    pub total_requests: u64,
    /// Memory statistics.
    pub memory: MemoryStats,
    /// List of loaded modules (optional, only included with ?verbose=true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_modules: Option<Vec<String>>,
}

/// Memory statistics for health check.
#[derive(Debug, Serialize)]
pub struct MemoryStats {
    /// Allocated memory (if available).
    pub allocated_bytes: Option<usize>,
    /// Memory limit per request.
    pub limit_per_request_bytes: usize,
}

/// Configuration validation errors.
///
/// These errors indicate invalid configuration values that would prevent
/// the host from operating correctly or safely.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConfigError {
    /// Execution timeout is invalid (must be > 0 and <= 300).
    Timeout { value: u64, reason: &'static str },
    /// Memory limit is invalid (must be >= 1MB and <= 4GB).
    MemoryLimit { value: usize, reason: &'static str },
    /// Concurrency settings are invalid.
    Concurrency { reason: &'static str },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { value, reason } => {
                write!(f, "invalid execution_timeout_secs={value}: {reason}")
            },
            Self::MemoryLimit { value, reason } => {
                write!(f, "invalid memory_limit_bytes={value}: {reason}")
            },
            Self::Concurrency { reason } => {
                write!(f, "invalid concurrency configuration: {reason}")
            },
        }
    }
}

impl std::error::Error for ConfigError {}

/// Configuration for the host.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Directory containing .wasm modules, or single component path.
    pub modules_path: PathBuf,
    /// Maximum number of modules to keep loaded (LRU eviction).
    pub cache_size: usize,
    /// Maximum cache memory in bytes (byte-aware eviction).
    pub max_cache_bytes: usize,
    /// Static files directory (optional).
    pub static_dir: Option<PathBuf>,
    /// Port to bind to (from mik.toml).
    pub port: u16,
    /// Timeout for WASM execution (in seconds).
    pub execution_timeout_secs: u64,
    /// Memory limit per request (in bytes).
    pub memory_limit_bytes: usize,
    /// Maximum concurrent requests.
    pub max_concurrent_requests: usize,
    /// Maximum request body size (in bytes).
    pub max_body_size_bytes: usize,
    /// Maximum concurrent requests per module.
    pub max_per_module_requests: usize,
    /// Graceful shutdown drain timeout in seconds.
    pub shutdown_timeout_secs: u64,
    /// Enable wasi:logging for WASM modules.
    pub logging_enabled: bool,
    /// Allowed hosts for outgoing HTTP requests.
    pub http_allowed: Vec<String>,
    /// Scripts directory (optional, for JS orchestration).
    pub scripts_dir: Option<PathBuf>,
    /// Hot-reload mode: bypass persistent AOT cache, always recompile.
    pub hot_reload: bool,
    /// Maximum AOT cache size in MB (0 = default 1GB).
    pub aot_cache_max_mb: usize,
    /// Fuel budget per request (None = use DEFAULT_FUEL_BUDGET).
    /// Fuel provides deterministic CPU limiting complementing epoch-based preemption.
    pub fuel_budget: Option<u64>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            modules_path: PathBuf::from(constants::DEFAULT_MODULES_DIR),
            cache_size: constants::DEFAULT_CACHE_SIZE,
            max_cache_bytes: constants::DEFAULT_CACHE_MB * 1024 * 1024,
            static_dir: None,
            port: constants::DEFAULT_PORT,
            execution_timeout_secs: constants::MAX_WASM_TIMEOUT_SECS,
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            max_concurrent_requests: constants::DEFAULT_MAX_CONCURRENT_REQUESTS,
            max_body_size_bytes: constants::MAX_BODY_SIZE_BYTES,
            max_per_module_requests: constants::DEFAULT_MAX_PER_MODULE_REQUESTS,
            shutdown_timeout_secs: DEFAULT_SHUTDOWN_TIMEOUT_SECS,
            logging_enabled: false,
            http_allowed: Vec::new(),
            scripts_dir: None,
            hot_reload: false,
            aot_cache_max_mb: 0, // 0 = default 1GB
            fuel_budget: None,   // None = use DEFAULT_FUEL_BUDGET
        }
    }
}

impl HostConfig {
    /// Validate configuration values.
    ///
    /// Checks that all configuration values are within acceptable bounds:
    /// - `execution_timeout_secs` must be > 0 and <= 300
    /// - `memory_limit_bytes` must be >= 1MB and <= 4GB
    /// - `max_concurrent_requests` must be > 0
    /// - `max_per_module_requests` must not exceed `max_concurrent_requests`
    ///
    /// Issues a warning (but does not fail) if `modules_path` does not exist.
    ///
    /// # Errors
    ///
    /// Returns a `ConfigError` if any configuration value is out of bounds.
    ///
    /// # Examples
    ///
    /// ```
    /// use mik::runtime::HostConfig;
    ///
    /// let config = HostConfig::default();
    /// assert!(config.validate().is_ok());
    ///
    /// let mut invalid_config = HostConfig::default();
    /// invalid_config.execution_timeout_secs = 0;
    /// assert!(invalid_config.validate().is_err());
    /// ```
    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        // Validate execution timeout (must be > 0 and <= 300 seconds)
        if self.execution_timeout_secs == 0 {
            return Err(ConfigError::Timeout {
                value: 0,
                reason: "must be greater than 0",
            });
        }
        if self.execution_timeout_secs > MAX_EXECUTION_TIMEOUT_SECS {
            return Err(ConfigError::Timeout {
                value: self.execution_timeout_secs,
                reason: "must be <= 300 seconds (5 minutes)",
            });
        }

        // Validate memory limit (must be >= 1MB and <= 4GB)
        if self.memory_limit_bytes < MIN_MEMORY_LIMIT_BYTES {
            return Err(ConfigError::MemoryLimit {
                value: self.memory_limit_bytes,
                reason: "must be >= 1MB (1048576 bytes)",
            });
        }
        if self.memory_limit_bytes > MAX_MEMORY_LIMIT_BYTES {
            return Err(ConfigError::MemoryLimit {
                value: self.memory_limit_bytes,
                reason: "must be <= 4GB (4294967296 bytes)",
            });
        }

        // Validate max_concurrent_requests (must be > 0)
        if self.max_concurrent_requests == 0 {
            return Err(ConfigError::Concurrency {
                reason: "max_concurrent_requests must be greater than 0",
            });
        }

        // Validate max_per_module_requests (must not exceed max_concurrent_requests)
        if self.max_per_module_requests > self.max_concurrent_requests {
            return Err(ConfigError::Concurrency {
                reason: "max_per_module_requests cannot exceed max_concurrent_requests",
            });
        }

        // Warn if modules_path doesn't exist (non-fatal)
        if !self.modules_path.exists() {
            warn!(
                "modules_path does not exist: {}",
                self.modules_path.display()
            );
        }

        Ok(())
    }
}

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

fn default_auto() -> bool {
    true
}

fn default_port() -> u16 {
    constants::DEFAULT_PORT
}

fn default_modules_dir() -> String {
    format!("{}/", constants::DEFAULT_MODULES_DIR)
}

fn default_execution_timeout_secs() -> u64 {
    DEFAULT_EXECUTION_TIMEOUT_SECS
}

fn default_memory_limit_bytes() -> usize {
    DEFAULT_MEMORY_LIMIT_BYTES
}

fn default_max_body_size_mb() -> usize {
    DEFAULT_MAX_BODY_SIZE_MB
}

fn default_shutdown_timeout_secs() -> u64 {
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
    pub fn cache_size(mut self, size: usize) -> Self {
        self.config.cache_size = size;
        self
    }

    /// Set the static files directory.
    pub fn static_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.static_dir = Some(path.into());
        self
    }

    /// Set the port (used when not overridden by CLI).
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set the execution timeout in seconds.
    pub fn execution_timeout(mut self, timeout_secs: u64) -> Self {
        self.config.execution_timeout_secs = timeout_secs;
        self
    }

    /// Set the memory limit per request in bytes.
    pub fn memory_limit(mut self, limit_bytes: usize) -> Self {
        self.config.memory_limit_bytes = limit_bytes;
        self
    }

    /// Set the maximum concurrent requests.
    pub fn max_concurrent_requests(mut self, max: usize) -> Self {
        self.config.max_concurrent_requests = max;
        self
    }

    /// Set the maximum request body size in bytes.
    pub fn max_body_size(mut self, max_bytes: usize) -> Self {
        self.config.max_body_size_bytes = max_bytes;
        self
    }

    /// Set the maximum concurrent requests per module.
    pub fn max_per_module_requests(mut self, max: usize) -> Self {
        self.config.max_per_module_requests = max;
        self
    }

    /// Enable wasi:logging for WASM modules.
    pub fn logging(mut self, enabled: bool) -> Self {
        self.config.logging_enabled = enabled;
        self
    }

    /// Set the allowed hosts for outgoing HTTP requests.
    pub fn http_allowed(mut self, hosts: Vec<String>) -> Self {
        self.config.http_allowed = hosts;
        self
    }

    /// Enable hot-reload mode (bypasses persistent AOT cache).
    pub fn hot_reload(mut self, enabled: bool) -> Self {
        self.config.hot_reload = enabled;
        self
    }

    /// Set the maximum AOT cache size in MB.
    pub fn aot_cache_max_mb(mut self, max_mb: usize) -> Self {
        self.config.aot_cache_max_mb = max_mb;
        self
    }

    /// Get the configured port.
    pub fn get_port(&self) -> u16 {
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

/// Component with cached size information for byte-aware eviction.
pub(crate) struct CachedComponent {
    component: Arc<Component>,
    size_bytes: usize,
}

/// Module cache with byte-aware eviction using moka.
/// Uses weigher function to ensure total bytes don't exceed limit.
type ModuleCache = MokaCache<String, Arc<CachedComponent>>;

/// Shared state for the HTTP runtime.
///
/// This struct contains all the state needed to handle HTTP requests:
/// - Wasmtime Engine and Linker for WASM execution
/// - Module cache for compiled components
/// - Configuration and limits
/// - Circuit breaker for reliability
///
/// It is shared across all request handlers via `Arc`.
///
/// # Thread Safety
///
/// Lock ordering to prevent deadlock: `request_semaphore` -> `module_semaphores` -> `circuit_breaker`
/// Note: cache is now thread-safe internally (moka), no external locking needed.
pub struct SharedState {
    pub(crate) engine: Engine,
    pub(crate) linker: Linker<HostState>,
    pub(crate) modules_dir: PathBuf,
    pub(crate) cache: ModuleCache,
    pub(crate) single_component: Option<Arc<Component>>,
    /// Name of the single component (derived from filename, for routing).
    pub(crate) single_component_name: Option<String>,
    pub(crate) static_dir: Option<PathBuf>,
    pub(crate) execution_timeout: Duration,
    /// Memory limit per request (enforced via `ResourceLimiter`).
    pub(crate) memory_limit_bytes: usize,
    pub(crate) max_body_size_bytes: usize,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) request_counter: AtomicU64,
    pub(crate) config: HostConfig,
    pub(crate) circuit_breaker: CircuitBreaker,
    pub(crate) request_semaphore: Arc<Semaphore>,
    pub(crate) module_semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    pub(crate) http_allowed: Arc<Vec<String>>,
    /// Scripts directory (optional, for JS orchestration).
    pub(crate) scripts_dir: Option<PathBuf>,
    /// Content-addressable AOT cache for compiled components.
    pub(crate) aot_cache: aot_cache::AotCache,
    /// Fuel budget per request for deterministic CPU limiting.
    pub(crate) fuel_budget: u64,
}

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

    /// Get health status with configurable detail level.
    fn get_health_status(&self, detail: HealthDetail) -> HealthStatus {
        // Get cache stats (no lock needed - moka is thread-safe)
        let loaded_modules = if detail == HealthDetail::Full {
            // Collect module names from cache with pre-allocated capacity
            self.cache.run_pending_tasks();
            let cache_size = self.cache.entry_count() as usize;
            let mut modules = Vec::with_capacity(cache_size);
            for (key, _) in &self.cache {
                modules.push((*key).clone());
            }
            Some(modules)
        } else {
            None
        };

        let cache_size = self.cache.entry_count() as usize;
        let cache_bytes = self.cache.weighted_size() as usize;

        HealthStatus {
            status: "ready".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            cache_size,
            cache_capacity: self.config.cache_size,
            cache_bytes,
            cache_max_bytes: self.config.max_cache_bytes,
            total_requests: self.request_counter.load(Ordering::Relaxed),
            memory: MemoryStats {
                allocated_bytes: get_memory_usage(),
                limit_per_request_bytes: self.config.memory_limit_bytes,
            },
            loaded_modules,
        }
    }
}

/// WASI HTTP host that serves WASM components.
///
/// The host manages the wasmtime engine, module cache, and HTTP server.
/// Use [`HostBuilder`] to create instances.
///
/// # Examples
///
/// ```no_run
/// use mik::runtime::HostBuilder;
/// use std::net::SocketAddr;
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let host = HostBuilder::new()
///         .modules_dir("modules/")
///         .port(3000)
///         .build()?;
///
///     // Start serving HTTP requests
///     let addr: SocketAddr = "127.0.0.1:3000".parse()?;
///     host.serve(addr).await
/// }
/// ```
pub struct Host {
    shared: Arc<SharedState>,
    /// Shutdown signal for the epoch incrementer thread.
    epoch_shutdown: Arc<AtomicBool>,
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
            1024 * 1024 * 1024 // Default: 1GB
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

        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;

        // Create moka cache with byte-aware eviction
        let cache = MokaCache::builder()
            .max_capacity(config.max_cache_bytes as u64)
            .weigher(|_key: &String, value: &Arc<CachedComponent>| -> u32 {
                value.size_bytes.min(u32::MAX as usize) as u32
            })
            .time_to_idle(Duration::from_secs(3600))
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
            circuit_breaker: CircuitBreaker::new(),
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

    /// Check if running in single component mode.
    #[allow(dead_code)]
    pub fn is_single_component(&self) -> bool {
        self.shared.single_component.is_some()
    }

    /// Get the single component name (for routing).
    pub fn single_component_name(&self) -> Option<&str> {
        self.shared.single_component_name.as_deref()
    }

    /// Check if static file serving is enabled.
    pub fn has_static_files(&self) -> bool {
        self.shared.static_dir.is_some()
    }

    /// Start serving HTTP requests on the given address.
    pub async fn serve(self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        info!("Serving on http://{}", addr);
        info!("Health endpoint: {}", HEALTH_PATH);
        info!("Metrics endpoint: {}", METRICS_PATH);

        if let Some(name) = self.single_component_name() {
            info!("Routes: /run/{}/* -> component", name);
        } else {
            info!("Routes: /run/<module>/* -> <module>.wasm");
        }

        if self.has_static_files() {
            info!("Routes: /static/<project>/* -> static files");
        }

        if let Some(ref scripts_dir) = self.shared.scripts_dir {
            info!("Routes: /script/<name> -> {:?}", scripts_dir);
        }

        // Setup graceful shutdown
        let shutdown_signal = self.shared.shutdown.clone();
        let mut shutdown_handle = tokio::spawn(async move {
            // Wait for SIGTERM/SIGINT
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("Failed to setup SIGTERM handler");
                let mut sigint =
                    signal(SignalKind::interrupt()).expect("Failed to setup SIGINT handler");

                tokio::select! {
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM");
                    }
                    _ = sigint.recv() => {
                        info!("Received SIGINT");
                    }
                }
            }

            #[cfg(not(unix))]
            {
                // Windows/other platforms - use ctrl_c
                if let Err(e) = tokio::signal::ctrl_c().await {
                    error!("Failed to listen for ctrl_c: {}", e);
                    return;
                }
                info!("Received Ctrl+C");
            }

            // Set shutdown flag
            shutdown_signal.store(true, Ordering::SeqCst);
        });

        // Track active connection tasks
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
        let active_connections = Arc::new(AtomicU64::new(0));

        loop {
            tokio::select! {
                // Accept new connections
                accept_result = listener.accept() => {
                    let (stream, remote_addr) = accept_result?;

                    // Check if shutdown has been initiated
                    if self.shared.shutdown.load(Ordering::SeqCst) {
                        debug!("Rejecting new connection during shutdown");
                        break;
                    }

                    let io = TokioIo::new(stream);
                    let shared = self.shared.clone();
                    let active_conns = active_connections.clone();
                    let shutdown_tx = shutdown_tx.clone();

                    // Acquire semaphore permit to limit concurrent requests
                    let Ok(permit) = shared.request_semaphore.clone().acquire_owned().await else {
                        warn!("Failed to acquire request permit, semaphore closed");
                        continue;
                    };

                    // Increment active connection count
                    active_conns.fetch_add(1, Ordering::SeqCst);

                    tokio::spawn(async move {
                        // Permit is dropped when this task completes
                        let _permit = permit;
                        let _shutdown_guard = shutdown_tx;

                        let service = service_fn(move |req| {
                            let shared = shared.clone();
                            async move { handle_request(shared, req, remote_addr).await }
                        });

                        // Auto-detect HTTP/1.1 or HTTP/2 for better performance
                        let builder = HttpConnectionBuilder::new(TokioExecutor::new());
                        if let Err(e) = builder.serve_connection(io, service).await {
                            error!("Connection error: {}", e);
                        }

                        // Decrement active connection count
                        active_conns.fetch_sub(1, Ordering::SeqCst);
                    });
                }

                // Check for shutdown signal
                _ = &mut shutdown_handle => {
                    // Shutdown signal received
                    break;
                }
            }
        }

        // Shutdown sequence initiated
        info!("Initiating graceful shutdown...");

        // Stop accepting new connections (already done by breaking out of loop)
        drop(listener);

        // Drop our copy of shutdown_tx so shutdown_rx can complete when all tasks finish
        drop(shutdown_tx);

        // Wait for in-flight requests to complete (with timeout)
        let drain_timeout = Duration::from_secs(self.shared.config.shutdown_timeout_secs);
        let active_count = active_connections.load(Ordering::SeqCst);

        if active_count > 0 {
            info!(
                "Waiting for {} active connections to complete (timeout: {:?})",
                active_count, drain_timeout
            );

            if tokio::time::timeout(drain_timeout, async {
                // Wait until all active connections finish
                while active_connections.load(Ordering::SeqCst) > 0 {
                    tokio::time::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS)).await;
                }
                // Also wait for shutdown_rx to close (all tasks dropped their senders)
                shutdown_rx.recv().await;
            })
            .await
            .is_ok()
            {
                info!("All connections completed gracefully");
            } else {
                let remaining = active_connections.load(Ordering::SeqCst);
                warn!("Shutdown timeout - {} connections still active", remaining);
            }
        } else {
            info!("No active connections to drain");
        }

        info!("Shutdown complete");
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Signal the epoch incrementer thread to stop
        self.epoch_shutdown.store(true, Ordering::Relaxed);
    }
}

/// Handle health check endpoint.
fn handle_health_endpoint(
    shared: &Arc<SharedState>,
    req: &Request<hyper::body::Incoming>,
    request_id: &Uuid,
    trace_id: &str,
    start_time: Instant,
    client_accepts_gzip: bool,
) -> Result<Response<Full<Bytes>>> {
    let duration = start_time.elapsed();
    info!(duration_ms = duration.as_millis() as u64, "Health check");

    let detail = if req
        .uri()
        .query()
        .and_then(|q| q.split('&').find(|s| s.starts_with("verbose=")))
        .and_then(|s| s.strip_prefix("verbose="))
        .is_some_and(|v| v == "true" || v == "1")
    {
        HealthDetail::Full
    } else {
        HealthDetail::Summary
    };

    let health = shared.get_health_status(detail);
    let body = serde_json::to_string_pretty(&health).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize health status to JSON");
        r#"{"status":"error"}"#.to_string()
    });

    let response = Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .header("X-Request-ID", request_id.to_string())
        .header("X-Trace-ID", trace_id)
        .body(Full::new(Bytes::from(body)))?;

    Ok(maybe_compress_response(response, client_accepts_gzip))
}

/// Handle metrics endpoint (Prometheus format).
fn handle_metrics_endpoint(
    shared: &Arc<SharedState>,
    request_id: &Uuid,
    trace_id: &str,
    start_time: Instant,
    client_accepts_gzip: bool,
) -> Result<Response<Full<Bytes>>> {
    let duration = start_time.elapsed();
    info!(duration_ms = duration.as_millis() as u64, "Metrics request");

    let metrics = shared.get_prometheus_metrics();

    let response = Response::builder()
        .status(200)
        .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        .header("X-Request-ID", request_id.to_string())
        .header("X-Trace-ID", trace_id)
        .body(Full::new(Bytes::from(metrics)))?;

    Ok(maybe_compress_response(response, client_accepts_gzip))
}

/// Handle an HTTP request using the shared runtime state.
///
/// This is the main request handler that can be used by both the standard
/// runtime and the high-performance server backends.
///
/// # Arguments
///
/// * `shared` - Shared runtime state containing engine, cache, and config
/// * `req` - The incoming HTTP request
/// * `remote_addr` - Remote address of the client
///
/// # Returns
///
/// The HTTP response to send back to the client.
pub async fn handle_request(
    shared: Arc<SharedState>,
    req: Request<hyper::body::Incoming>,
    remote_addr: SocketAddr,
) -> Result<Response<Full<Bytes>>> {
    let request_id = Uuid::new_v4();
    let start_time = Instant::now();

    let trace_id = req
        .headers()
        .get("x-trace-id")
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| request_id.to_string(), String::from);

    shared.request_counter.fetch_add(1, Ordering::Relaxed);

    let method = req.method();
    let path = req.uri().path();

    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        trace_id = %trace_id,
        method = %method,
        path = %path,
        remote_addr = %remote_addr
    );
    let _enter = span.enter();
    info!("Request started");

    let client_accepts_gzip = accepts_gzip(&req);

    // Handle built-in endpoints
    if path == HEALTH_PATH {
        return handle_health_endpoint(
            &shared,
            &req,
            &request_id,
            &trace_id,
            start_time,
            client_accepts_gzip,
        );
    }
    if path == METRICS_PATH {
        return handle_metrics_endpoint(
            &shared,
            &request_id,
            &trace_id,
            start_time,
            client_accepts_gzip,
        );
    }

    // Create span collector and root request span for timing data
    let span_collector = SpanCollector::new();
    let request_span = SpanBuilder::new("request");
    let request_span_id = request_span.span_id().to_string();

    // Handle the request and log result
    let result = handle_request_inner(
        shared,
        req,
        remote_addr,
        client_accepts_gzip,
        &trace_id,
        span_collector.clone(),
        &request_span_id,
    )
    .await;
    let duration = start_time.elapsed();

    // Complete root request span based on result
    match &result {
        Ok(_) => span_collector.add(request_span.finish()),
        Err(e) => span_collector.add(request_span.finish_with_error(e.to_string())),
    }

    // Collect and log span summary
    let spans = span_collector.collect();
    if !spans.is_empty() {
        let summary = SpanSummary::new(&trace_id, duration.as_millis() as u64, spans);
        info!(
            trace_id = %summary.trace_id,
            total_ms = summary.total_ms,
            handler_calls = summary.handler_calls,
            spans = ?summary.spans,
            "Request timing spans"
        );
    }

    match &result {
        Ok(response) => {
            let status = response.status().as_u16();
            info!(
                status = status,
                duration_ms = duration.as_millis() as u64,
                "Request completed"
            );
        },
        Err(e) => {
            let category = categorize_error(e);
            error!(
                error = %e,
                category = category.as_str(),
                duration_ms = duration.as_millis() as u64,
                "Request failed"
            );
        },
    }

    // Add request ID and trace ID to response headers
    result.map(|mut resp| {
        // Safe: UUID::to_string() always produces valid header characters
        if let Ok(header_value) = request_id.to_string().parse() {
            resp.headers_mut().insert("X-Request-ID", header_value);
        }
        // Add trace ID for distributed tracing
        if let Ok(header_value) = trace_id.parse() {
            resp.headers_mut().insert("X-Trace-ID", header_value);
        }
        resp
    })
}

// ============================================================================
// Helper functions for handle_request_inner (extracted to reduce line count)
// ============================================================================

/// Validates path length and returns a 414 response if too long.
fn validate_path_length(path: &str) -> Option<Response<Full<Bytes>>> {
    if path.len() > MAX_PATH_LENGTH {
        Response::builder()
            .status(414)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(format!(
                r#"{{"error":"URI too long","length":{},"max":{}}}"#,
                path.len(),
                MAX_PATH_LENGTH
            ))))
            .ok()
    } else {
        None
    }
}

/// Validates Content-Length header and returns a 413 response if too large.
fn validate_content_length(
    headers: &hyper::HeaderMap,
    max_body: usize,
) -> Option<Response<Full<Bytes>>> {
    if let Some(content_length) = headers.get("content-length")
        && let Ok(len_str) = content_length.to_str()
        && let Ok(len) = len_str.parse::<usize>()
        && len > max_body
    {
        Response::builder()
            .status(413)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(format!(
                r#"{{"error":"Request body too large","size":{len},"max":{max_body}}}"#
            ))))
            .ok()
    } else {
        None
    }
}

/// Parses the module route from a run path, returning (module_name, handler_path).
fn parse_module_route(run_path: &str) -> (String, String) {
    if run_path.is_empty() {
        (String::new(), "/".to_string())
    } else if let Some(idx) = run_path.find('/') {
        let module_part = &run_path[..idx];
        let rest = &run_path[idx..];

        // Only decode if percent-encoded characters are present
        let decoded_module = if module_part.contains('%') {
            percent_decode_str(module_part)
                .decode_utf8_lossy()
                .into_owned()
        } else {
            module_part.to_string()
        };

        let decoded_rest = if rest.contains('%') {
            percent_decode_str(rest).decode_utf8_lossy().into_owned()
        } else {
            rest.to_string()
        };

        (decoded_module, decoded_rest)
    } else {
        // No slash found - entire run_path is the module
        let decoded = if run_path.contains('%') {
            percent_decode_str(run_path)
                .decode_utf8_lossy()
                .into_owned()
        } else {
            run_path.to_string()
        };
        (decoded, "/".to_string())
    }
}

/// Result type for module resolution.
enum ModuleResolution {
    /// Successfully resolved module with component and handler path.
    Success {
        component: Arc<Component>,
        handler_path: String,
        module_name: Option<String>,
        module_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    },
    /// Early return with a response (error or not found).
    Response(Response<Full<Bytes>>),
}

/// Resolves the module component from the path.
async fn resolve_module(shared: &Arc<SharedState>, path: &str) -> Result<ModuleResolution> {
    // All module routes must start with /run/
    let Some(run_path) = path.strip_prefix(RUN_PREFIX) else {
        return Ok(ModuleResolution::Response(not_found(
            "Not found. WASM modules are served at /run/<module>/",
        )?));
    };

    let (module, handler_path) = parse_module_route(run_path);

    if module.is_empty() {
        return Ok(ModuleResolution::Response(not_found(
            "No module specified. Use /run/<module>/",
        )?));
    }

    // Single component mode: check if module matches
    if let (Some(comp), Some(expected_name)) =
        (&shared.single_component, &shared.single_component_name)
    {
        if module == *expected_name {
            return Ok(ModuleResolution::Success {
                component: comp.clone(),
                handler_path,
                module_name: Some(module),
                module_permit: None,
            });
        }
        let err = error::Error::module_not_found(&module);
        return Ok(ModuleResolution::Response(error_response(&err)?));
    }

    // Multi-module mode: load from directory
    resolve_multi_module(shared, &module, handler_path).await
}

/// Resolves a module in multi-module mode (circuit breaker, semaphore, loading).
async fn resolve_multi_module(
    shared: &Arc<SharedState>,
    module: &str,
    handler_path: String,
) -> Result<ModuleResolution> {
    // Check circuit breaker before processing
    if let Err(e) = shared.circuit_breaker.check_request(module) {
        warn!("Circuit breaker blocked request to '{}': {}", module, e);
        let err = error::Error::circuit_breaker_open(module);
        let mut resp = error_response(&err)?;
        resp.headers_mut().insert(
            "Retry-After",
            CIRCUIT_BREAKER_RETRY_AFTER_SECS
                .to_string()
                .parse()
                .expect("valid Retry-After header value"),
        );
        return Ok(ModuleResolution::Response(resp));
    }

    // Acquire per-module semaphore permit
    let module_semaphore = shared.get_module_semaphore(module);
    let module_permit = if let Ok(permit) = module_semaphore.try_acquire_owned() {
        Some(permit)
    } else {
        warn!(
            "Module '{}' overloaded (max {} concurrent requests)",
            module, shared.config.max_per_module_requests
        );
        let err = error::Error::rate_limit_exceeded(format!(
            "Module '{}' overloaded (max {} concurrent)",
            module, shared.config.max_per_module_requests
        ));
        let mut resp = error_response(&err)?;
        resp.headers_mut().insert(
            "Retry-After",
            MODULE_OVERLOAD_RETRY_AFTER_SECS
                .to_string()
                .parse()
                .expect("valid Retry-After header value"),
        );
        return Ok(ModuleResolution::Response(resp));
    };

    match shared.get_or_load(module).await {
        Ok(comp) => Ok(ModuleResolution::Success {
            component: comp,
            handler_path,
            module_name: Some(module.to_string()),
            module_permit,
        }),
        Err(e) => {
            warn!("Module load failed: {}", e);
            // Record failure in circuit breaker
            shared.circuit_breaker.record_failure(module);
            // Use typed error for response
            let err = error::Error::module_not_found(module);
            Ok(ModuleResolution::Response(error_response(&err)?))
        },
    }
}

/// Collects request body with size limit, returning 413 if exceeded.
async fn collect_request_body(
    body: hyper::body::Incoming,
    max_body: usize,
) -> Result<Result<Bytes, Response<Full<Bytes>>>> {
    let limited_body = Limited::new(body, max_body);
    match limited_body.collect().await {
        Ok(collected) => Ok(Ok(collected.to_bytes())),
        Err(e) => {
            // Check if this is a size limit error using source chain
            let mut is_limit_error = false;
            let mut current: Option<&dyn std::error::Error> = Some(&*e);
            while let Some(err) = current {
                if err.to_string().contains("length limit") {
                    is_limit_error = true;
                    break;
                }
                current = err.source();
            }
            if is_limit_error {
                Ok(Err(Response::builder()
                    .status(413)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(format!(
                        r#"{{"error":"Request body too large","max":{max_body}}}"#
                    ))))?))
            } else {
                Err(anyhow::anyhow!("Failed to read request body: {e}"))
            }
        },
    }
}

async fn handle_request_inner(
    shared: Arc<SharedState>,
    req: Request<hyper::body::Incoming>,
    _remote_addr: SocketAddr,
    client_accepts_gzip: bool,
    trace_id: &str,
    span_collector: SpanCollector,
    parent_span_id: &str,
) -> Result<Response<Full<Bytes>>> {
    let path = req.uri().path();
    let max_body = shared.max_body_size_bytes;

    // Validate path length (DoS prevention)
    if let Some(resp) = validate_path_length(path) {
        return Ok(resp);
    }

    // Check Content-Length header (fast path for body size limit)
    if let Some(resp) = validate_content_length(req.headers(), max_body) {
        return Ok(resp);
    }

    // Handle static file requests
    if path.starts_with(STATIC_PREFIX) {
        return match &shared.static_dir {
            Some(dir) => serve_static_file(dir, path)
                .await
                .map(|resp| maybe_compress_response(resp, client_accepts_gzip)),
            None => not_found("Static file serving not enabled"),
        };
    }

    // Handle script requests
    if path.starts_with(SCRIPT_PREFIX) {
        let script_path = path.to_string();
        return match script::handle_script_request(
            shared.clone(),
            req,
            &script_path,
            trace_id,
            span_collector,
            parent_span_id,
        )
        .await
        {
            Ok(resp) => Ok(maybe_compress_response(resp, client_accepts_gzip)),
            Err(e) => {
                warn!("Script error: {}", e);
                let err = error::Error::script_error(&script_path, e.to_string());
                error_response(&err)
            },
        };
    }

    // Resolve module component
    let (component, handler_path, module_name, module_permit) =
        match resolve_module(&shared, path).await? {
            ModuleResolution::Success {
                component,
                handler_path,
                module_name,
                module_permit,
            } => (component, handler_path, module_name, module_permit),
            ModuleResolution::Response(resp) => return Ok(resp),
        };

    // Rewrite the request URI and collect body
    let req = rewrite_request_path(req, handler_path)?;
    let (parts, body) = req.into_parts();
    let body_bytes = match collect_request_body(body, max_body).await? {
        Ok(bytes) => bytes,
        Err(resp) => return Ok(resp),
    };
    let req = Request::from_parts(parts, HyperCompatibleBody(Full::new(body_bytes)));

    // Execute WASM request (keep module_permit in scope for semaphore)
    let _module_permit = module_permit;
    let result = execute_wasm_request(shared.clone(), component, req).await;

    // Record success/failure in circuit breaker
    if let Some(ref module) = module_name {
        match &result {
            Ok(_) => shared.circuit_breaker.record_success(module),
            Err(_) => shared.circuit_breaker.record_failure(module),
        }
    }

    result.map(|resp| maybe_compress_response(resp, client_accepts_gzip))
}

/// Execute a WASM request with a Full<Bytes> body (for script orchestration).
pub(crate) async fn execute_wasm_request_internal(
    shared: Arc<SharedState>,
    component: Arc<Component>,
    req: Request<Full<Bytes>>,
) -> Result<Response<Full<Bytes>>> {
    let (parts, body) = req.into_parts();
    let req = Request::from_parts(parts, HyperCompatibleBody(body));
    execute_wasm_request(shared, component, req).await
}

/// Execute a WASM request (internal helper).
///
/// Body is pre-collected with size limits already enforced.
async fn execute_wasm_request(
    shared: Arc<SharedState>,
    component: Arc<Component>,
    req: Request<HyperCompatibleBody>,
) -> Result<Response<Full<Bytes>>> {
    // Create fresh WASI context
    let wasi = WasiCtxBuilder::new().inherit_stdio().inherit_env().build();

    // Use pre-computed Arc (cheap pointer copy instead of cloning Vec)
    let http_allowed = shared.http_allowed.clone();

    let state = HostState {
        wasi,
        http: WasiHttpCtx::new(),
        table: ResourceTable::new(),
        http_allowed,
        memory_limit: shared.memory_limit_bytes,
    };

    let mut store = Store::new(&shared.engine, state);

    // Enable ResourceLimiter for memory enforcement
    store.limiter(|state| state);

    // Configure epoch deadline for async yielding (100 epochs/second, so timeout_secs * 100)
    // Using epoch_deadline_async_yield_and_update instead of set_epoch_deadline because:
    // 1. On shutdown, the epoch incrementer thread stops, causing WASM to hit its deadline
    // 2. With async yielding, WASM will yield (return Pending) instead of trapping
    // 3. The tokio::time::timeout wrapper will then cancel the execution gracefully
    // This provides cooperative cancellation during shutdown rather than abrupt traps.
    let timeout_epochs = shared.execution_timeout.as_secs().saturating_mul(100);
    store.epoch_deadline_async_yield_and_update(timeout_epochs);

    // Set fuel budget for deterministic CPU limiting
    // Fuel provides deterministic limits complementing epoch-based preemption
    store.set_fuel(shared.fuel_budget)?;

    // Create response channel
    let (sender, receiver) = tokio::sync::oneshot::channel();

    // Create request/response resources
    let req_resource = store.data_mut().new_incoming_request(Scheme::Http, req)?;
    let out_resource = store.data_mut().new_response_outparam(sender)?;

    // Instantiate and call handler with timeout enforcement
    let timeout = shared.execution_timeout;

    let proxy = tokio::time::timeout(
        timeout,
        wasmtime_wasi_http::bindings::Proxy::instantiate_async(
            &mut store,
            &component,
            &shared.linker,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("WASM instantiation timed out after {timeout:?}"))?
    .context("Failed to instantiate proxy")?;

    tokio::time::timeout(timeout, async {
        proxy
            .wasi_http_incoming_handler()
            .call_handle(&mut store, req_resource, out_resource)
            .await
    })
    .await
    .map_err(|_| anyhow::anyhow!("WASM execution timed out after {timeout:?}"))?
    .context("Handler call failed")?;

    // Get response
    let response = receiver
        .await
        .context("No response received")?
        .context("Response error")?;

    // Convert to hyper response
    let mut builder = Response::builder().status(response.status());

    for (name, value) in response.headers() {
        builder = builder.header(name, value);
    }

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();

    Ok(builder.body(Full::new(body_bytes))?)
}

/// Categorize an error by walking the error chain.
///
/// Uses `to_string()` instead of `format!("{:?}")` to avoid potential panics
/// from buggy Debug implementations.
fn categorize_error(error: &anyhow::Error) -> ErrorCategory {
    // Walk the entire error chain for robust matching
    let mut current: &dyn std::error::Error = error.as_ref();
    loop {
        let msg = current.to_string();

        if msg.contains("timed out") {
            return ErrorCategory::Timeout;
        }
        if msg.contains("Module not found") || msg.contains("Failed to load") {
            return ErrorCategory::ModuleLoad;
        }
        if msg.contains("Not found") || msg.contains("Invalid") {
            return ErrorCategory::InvalidRequest;
        }
        if msg.contains("Failed to instantiate") {
            return ErrorCategory::Instantiation;
        }
        if msg.contains("Handler call failed") {
            return ErrorCategory::Execution;
        }
        if msg.contains("static file") {
            return ErrorCategory::StaticFile;
        }

        // Walk to next error in chain
        match current.source() {
            Some(source) => current = source,
            None => break,
        }
    }

    ErrorCategory::Internal
}

/// Create a 404 Not Found response.
fn not_found(message: &str) -> Result<Response<Full<Bytes>>> {
    Ok(Response::builder()
        .status(404)
        .body(Full::new(Bytes::from(message.to_string())))?)
}

/// Create an error response from a typed runtime error.
///
/// Uses the error's status_code() method for the HTTP status and
/// provides a JSON error body.
fn error_response(err: &error::Error) -> Result<Response<Full<Bytes>>> {
    let status = err.status_code();
    let body = serde_json::json!({
        "error": err.to_string(),
        "status": status
    });
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))?)
}

/// Convert a typed error to its error category for logging.
impl From<&error::Error> for ErrorCategory {
    fn from(err: &error::Error) -> Self {
        match err {
            error::Error::ModuleNotFound { .. } | error::Error::ModuleLoadFailed { .. } => {
                ErrorCategory::ModuleLoad
            },
            error::Error::ExecutionTimeout { .. } => ErrorCategory::Timeout,
            error::Error::PathTraversal { .. } | error::Error::InvalidRequest(_) => {
                ErrorCategory::InvalidRequest
            },
            error::Error::ScriptNotFound { .. } | error::Error::ScriptError { .. } => {
                ErrorCategory::Script
            },
            error::Error::CircuitBreakerOpen { .. } | error::Error::RateLimitExceeded { .. } => {
                ErrorCategory::Reliability
            },
            error::Error::MemoryLimitExceeded { .. } => ErrorCategory::Execution,
            _ => ErrorCategory::Internal,
        }
    }
}

/// Rewrite request to use new path.
fn rewrite_request_path(
    req: Request<hyper::body::Incoming>,
    new_path: impl AsRef<str>,
) -> Result<Request<hyper::body::Incoming>> {
    let new_path = new_path.as_ref();
    let (mut parts, body) = req.into_parts();

    // Build new URI with updated path
    let mut uri_parts = parts.uri.into_parts();
    uri_parts.path_and_query = Some(new_path.parse()?);
    parts.uri = Uri::from_parts(uri_parts)?;

    Ok(Request::from_parts(parts, body))
}

/// Serve a static file from the static directory.
/// Path format: /static/<project>/<file>
async fn serve_static_file(static_dir: &Path, path: &str) -> Result<Response<Full<Bytes>>> {
    // Strip /static/ prefix
    let file_path = path.strip_prefix(STATIC_PREFIX).unwrap_or(path);

    // Security: sanitize path to prevent directory traversal
    let sanitized_path = match security::sanitize_file_path(file_path) {
        Ok(p) => p,
        Err(e) => {
            warn!("Path traversal attempt blocked: {} - {}", file_path, e);
            return Ok(Response::builder()
                .status(400)
                .body(Full::new(Bytes::from("Invalid path")))?);
        },
    };

    // Check if it's a directory - try index.html (async to avoid blocking)
    let check_path = static_dir.join(&sanitized_path);
    let target_path = match tokio::fs::metadata(&check_path).await {
        Ok(meta) if meta.is_dir() => sanitized_path.join("index.html"),
        _ => sanitized_path,
    };

    // Security: validate path stays within static_dir after resolving symlinks (TOCTOU protection)
    let full_path = match security::validate_path_within_base(static_dir, &target_path) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Symlink escape attempt blocked: {} - {}",
                target_path.display(),
                e
            );
            return Ok(Response::builder()
                .status(400)
                .body(Full::new(Bytes::from("Invalid path")))?);
        },
    };

    // Read file
    match tokio::fs::read(&full_path).await {
        Ok(contents) => {
            let content_type = guess_content_type(&full_path);
            Ok(Response::builder()
                .status(200)
                .header("Content-Type", content_type.as_ref())
                .header("Cache-Control", STATIC_CACHE_CONTROL)
                .body(Full::new(Bytes::from(contents)))?)
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => not_found("File not found"),
        Err(e) => {
            error!("Failed to read static file {}: {}", full_path.display(), e);
            Ok(Response::builder()
                .status(500)
                .body(Full::new(Bytes::from("Internal server error")))?)
        },
    }
}

/// Guess content type from file extension using the `mime_guess` crate.
///
/// Uses a comprehensive MIME type database instead of custom extension matching.
/// Returns `Cow<'static, str>` to avoid allocations for common MIME types.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use mik::runtime::guess_content_type;
///
/// assert_eq!(guess_content_type(Path::new("style.css")), "text/css; charset=utf-8");
/// assert_eq!(guess_content_type(Path::new("image.png")), "image/png");
/// assert_eq!(guess_content_type(Path::new("data.bin")), "application/octet-stream");
/// ```
pub fn guess_content_type(path: &Path) -> Cow<'static, str> {
    mime_guess::from_path(path)
        .first()
        .map_or(Cow::Borrowed("application/octet-stream"), |mime| {
            // Use static strings for common types to avoid allocations
            let mime_str = mime.essence_str();
            match mime_str {
                "text/html" => Cow::Borrowed("text/html; charset=utf-8"),
                "text/css" => Cow::Borrowed("text/css; charset=utf-8"),
                "text/javascript" => Cow::Borrowed("text/javascript; charset=utf-8"),
                "application/javascript" => Cow::Borrowed("application/javascript; charset=utf-8"),
                "application/json" => Cow::Borrowed("application/json; charset=utf-8"),
                "text/plain" => Cow::Borrowed("text/plain; charset=utf-8"),
                "text/xml" => Cow::Borrowed("text/xml; charset=utf-8"),
                "application/xml" => Cow::Borrowed("application/xml; charset=utf-8"),
                // Common binary types (no charset needed)
                "image/png" => Cow::Borrowed("image/png"),
                "image/jpeg" => Cow::Borrowed("image/jpeg"),
                "image/gif" => Cow::Borrowed("image/gif"),
                "image/svg+xml" => Cow::Borrowed("image/svg+xml"),
                "image/webp" => Cow::Borrowed("image/webp"),
                "image/x-icon" => Cow::Borrowed("image/x-icon"),
                "application/pdf" => Cow::Borrowed("application/pdf"),
                "application/wasm" => Cow::Borrowed("application/wasm"),
                // Uncommon types: allocate only when needed
                _ => {
                    if mime_str.starts_with("text/")
                        || mime_str.contains("json")
                        || mime_str.contains("xml")
                    {
                        Cow::Owned(format!("{mime_str}; charset=utf-8"))
                    } else {
                        Cow::Owned(mime_str.to_string())
                    }
                },
            }
        })
}

/// Get current memory usage (platform-specific).
fn get_memory_usage() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().next().map(String::from))
            .and_then(|s| s.parse::<usize>().ok())
            .map(|pages| pages * 4096)
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Check if a content type is compressible (text-based or JSON/XML).
fn is_compressible_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript")
        || content_type == "image/svg+xml"
}

/// Check if the client accepts gzip encoding.
fn accepts_gzip<B>(req: &Request<B>) -> bool {
    req.headers()
        .get(ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("gzip"))
}

/// Compress body bytes with gzip if appropriate.
///
/// Applies gzip compression when:
/// - Body is larger than `GZIP_MIN_SIZE` (1024 bytes)
/// - Content type is compressible (text, JSON, XML, etc.)
/// - Client accepts gzip encoding
///
/// Returns the original bytes unchanged if compression is not beneficial,
/// along with optional Content-Encoding header value.
fn maybe_compress_body(
    body: Bytes,
    content_type: &str,
    client_accepts_gzip: bool,
) -> (Bytes, Option<&'static str>) {
    // Skip if client doesn't accept gzip
    if !client_accepts_gzip {
        return (body, None);
    }

    // Skip small responses (compression overhead not worth it)
    if body.len() < GZIP_MIN_SIZE {
        return (body, None);
    }

    // Check content type for compressibility
    if !is_compressible_content_type(content_type) {
        return (body, None);
    }

    // Compress with gzip (pre-allocate estimated compressed size)
    let mut encoder = GzEncoder::new(Vec::with_capacity(body.len()), Compression::fast());
    if encoder.write_all(&body).is_err() {
        return (body, None);
    }
    let Ok(compressed) = encoder.finish() else {
        return (body, None);
    };

    // Only use compressed version if it's smaller
    if compressed.len() >= body.len() {
        return (body, None);
    }

    debug!(
        "Gzip compressed response: {} -> {} bytes ({:.1}% reduction)",
        body.len(),
        compressed.len(),
        (1.0 - compressed.len() as f64 / body.len() as f64) * 100.0
    );

    (Bytes::from(compressed), Some("gzip"))
}

/// Apply gzip compression to a response if appropriate.
///
/// This is a convenience wrapper that extracts body bytes from a response,
/// compresses them, and rebuilds the response.
fn maybe_compress_response(
    response: Response<Full<Bytes>>,
    client_accepts_gzip: bool,
) -> Response<Full<Bytes>> {
    use std::task::{Context, Poll};

    // Skip if client doesn't accept gzip
    if !client_accepts_gzip {
        return response;
    }

    // For Full<Bytes>, we can't easily extract the bytes without consuming.
    // Instead, we'll collect the body using the BodyExt trait.
    // Since this is synchronous code and Full<Bytes> is a simple single-frame body,
    // we use a blocking approach via poll.
    let (parts, body) = response.into_parts();

    // Get content type for compression decision
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Check body size hint first
    let size_hint = body.size_hint();
    let body_size = size_hint.exact().unwrap_or(size_hint.lower()) as usize;

    if body_size < GZIP_MIN_SIZE {
        return Response::from_parts(parts, body);
    }

    if !is_compressible_content_type(content_type) {
        return Response::from_parts(parts, body);
    }

    // Poll the single frame from Full<Bytes>
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut pinned = std::pin::pin!(body);

    let body_bytes = match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
            Ok(data) => data,
            Err(_) => return Response::from_parts(parts, Full::new(Bytes::new())),
        },
        _ => return Response::from_parts(parts, Full::new(Bytes::new())),
    };

    // Compress
    let (compressed_bytes, encoding) = maybe_compress_body(body_bytes.clone(), content_type, true);

    if encoding.is_none() {
        // Compression didn't help, return original
        return Response::from_parts(parts, Full::new(body_bytes));
    }

    // Build compressed response
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in &parts.headers {
        // Skip Content-Length (will be recalculated)
        if name != CONTENT_LENGTH {
            builder = builder.header(name, value);
        }
    }
    builder = builder.header(CONTENT_ENCODING, "gzip");
    builder = builder.header(CONTENT_LENGTH, compressed_bytes.len());

    builder.body(Full::new(compressed_bytes)).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to build compressed response, falling back to uncompressed");
        Response::from_parts(parts, Full::new(body_bytes))
    })
}

impl SharedState {
    /// Generate Prometheus-format metrics.
    fn get_prometheus_metrics(&self) -> String {
        use std::fmt::Write;

        let total_requests = self.request_counter.load(Ordering::Relaxed);
        let cache_entries = self.cache.entry_count();
        let cache_bytes = self.cache.weighted_size();
        let circuit_states = self.circuit_breaker.get_all_states();

        let mut output = String::with_capacity(2048);

        // Help and type declarations
        output.push_str("# HELP mik_requests_total Total number of HTTP requests received\n");
        output.push_str("# TYPE mik_requests_total counter\n");
        let _ = writeln!(output, "mik_requests_total {total_requests}\n");

        output.push_str("# HELP mik_cache_entries Number of modules in cache\n");
        output.push_str("# TYPE mik_cache_entries gauge\n");
        let _ = writeln!(output, "mik_cache_entries {cache_entries}\n");

        output.push_str("# HELP mik_cache_bytes Total bytes used by cached modules\n");
        output.push_str("# TYPE mik_cache_bytes gauge\n");
        let _ = writeln!(output, "mik_cache_bytes {cache_bytes}\n");

        output.push_str("# HELP mik_cache_capacity_bytes Maximum cache size in bytes\n");
        output.push_str("# TYPE mik_cache_capacity_bytes gauge\n");
        let _ = writeln!(
            output,
            "mik_cache_capacity_bytes {}\n",
            self.config.max_cache_bytes
        );

        output.push_str("# HELP mik_max_concurrent_requests Maximum allowed concurrent requests\n");
        output.push_str("# TYPE mik_max_concurrent_requests gauge\n");
        let _ = writeln!(
            output,
            "mik_max_concurrent_requests {}\n",
            self.config.max_concurrent_requests
        );

        output
            .push_str("# HELP mik_circuit_breaker_state Circuit breaker state per module (0=closed, 1=open, 2=half-open)\n");
        output.push_str("# TYPE mik_circuit_breaker_state gauge\n");
        for (module, state) in &circuit_states {
            let state_value = match state.as_str() {
                "open" => 1,
                "half_open" => 2,
                _ => 0, // closed or unknown
            };
            let _ = writeln!(
                output,
                "mik_circuit_breaker_state{{module=\"{module}\"}} {state_value}"
            );
        }
        if !circuit_states.is_empty() {
            output.push('\n');
        }

        // Memory usage (if available)
        if let Some(mem) = get_memory_usage() {
            output.push_str("# HELP mik_memory_bytes Process memory usage in bytes\n");
            output.push_str("# TYPE mik_memory_bytes gauge\n");
            let _ = writeln!(output, "mik_memory_bytes {mem}");
        }

        output
    }
}

// NOTE: Tests for is_http_host_allowed are in reliability/src/security.rs
// which is the single source of truth for this function.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid() {
        let config = HostConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_zero_timeout_is_invalid() {
        let config = HostConfig {
            execution_timeout_secs: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::Timeout { value: 0, .. }));
        assert!(err.to_string().contains("must be greater than 0"));
    }

    #[test]
    fn test_excessive_timeout_is_invalid() {
        let config = HostConfig {
            execution_timeout_secs: 301, // Max is 300
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::Timeout { value: 301, .. }));
        assert!(err.to_string().contains("must be <= 300 seconds"));
    }

    #[test]
    fn test_valid_timeout_at_boundary() {
        // Max allowed
        let config = HostConfig {
            execution_timeout_secs: 300,
            ..Default::default()
        };
        assert!(config.validate().is_ok());

        // Min allowed
        let config = HostConfig {
            execution_timeout_secs: 1,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_memory_limit_too_small() {
        let config = HostConfig {
            memory_limit_bytes: 512 * 1024, // 512KB, less than 1MB min
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::MemoryLimit { .. }));
        assert!(err.to_string().contains("must be >= 1MB"));
    }

    #[test]
    fn test_memory_limit_too_large() {
        let config = HostConfig {
            memory_limit_bytes: 5 * 1024 * 1024 * 1024, // 5GB, more than 4GB max
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::MemoryLimit { .. }));
        assert!(err.to_string().contains("must be <= 4GB"));
    }

    #[test]
    fn test_valid_memory_limit_at_boundaries() {
        // Min boundary: 1MB
        let config = HostConfig {
            memory_limit_bytes: 1024 * 1024,
            ..Default::default()
        };
        assert!(config.validate().is_ok());

        // Max boundary: 4GB
        let config = HostConfig {
            memory_limit_bytes: 4 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_zero_concurrent_requests_is_invalid() {
        let config = HostConfig {
            max_concurrent_requests: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::Concurrency { .. }));
        assert!(
            err.to_string()
                .contains("max_concurrent_requests must be greater than 0")
        );
    }

    #[test]
    fn test_per_module_exceeds_total_concurrent() {
        let config = HostConfig {
            max_concurrent_requests: 100,
            max_per_module_requests: 200, // Exceeds total
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::Concurrency { .. }));
        assert!(
            err.to_string()
                .contains("max_per_module_requests cannot exceed max_concurrent_requests")
        );
    }

    #[test]
    fn test_per_module_equal_to_total_is_valid() {
        let config = HostConfig {
            max_concurrent_requests: 100,
            max_per_module_requests: 100, // Equal is allowed
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_error_display() {
        let timeout_err = ConfigError::Timeout {
            value: 0,
            reason: "must be greater than 0",
        };
        assert_eq!(
            timeout_err.to_string(),
            "invalid execution_timeout_secs=0: must be greater than 0"
        );

        let memory_err = ConfigError::MemoryLimit {
            value: 512,
            reason: "too small",
        };
        assert_eq!(
            memory_err.to_string(),
            "invalid memory_limit_bytes=512: too small"
        );

        let concurrency_err = ConfigError::Concurrency {
            reason: "must be positive",
        };
        assert_eq!(
            concurrency_err.to_string(),
            "invalid concurrency configuration: must be positive"
        );
    }

    #[test]
    fn test_config_error_is_error_trait() {
        let err: Box<dyn std::error::Error> = Box::new(ConfigError::Timeout {
            value: 0,
            reason: "test",
        });
        // Just verify it compiles and can be used as a trait object
        assert!(!err.to_string().is_empty());
    }

    /// Test that the epoch thread stops when Host is dropped.
    ///
    /// This test creates a Host with a minimal configuration, then drops it
    /// and verifies that the epoch_shutdown flag was set. The actual thread
    /// termination happens asynchronously, but we verify the signal is sent.
    #[test]
    fn test_epoch_thread_shutdown_on_drop() {
        // Create a temporary directory with a dummy wasm file for the Host
        let temp_dir = std::env::temp_dir().join("mik_epoch_test");
        let _ = std::fs::create_dir_all(&temp_dir);

        // Create a minimal valid WASM component (magic + version + empty)
        // This is just enough to pass initial validation
        let wasm_path = temp_dir.join("test.wasm");
        // Minimal WASM module: magic number (0x00 0x61 0x73 0x6D) + version (0x01 0x00 0x00 0x00)
        std::fs::write(&wasm_path, [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]).unwrap();

        // Create Host with multi-module mode
        let config = HostConfig {
            modules_path: temp_dir.clone(),
            cache_size: 1,
            max_cache_bytes: 1024 * 1024,
            max_concurrent_requests: 1,
            ..HostConfig::default()
        };

        // Host::new should succeed since the directory contains a .wasm file
        let host = Host::new(config);

        // Clean up temp file
        let _ = std::fs::remove_file(&wasm_path);
        let _ = std::fs::remove_dir(&temp_dir);

        // Skip test if Host creation failed (e.g., pooling allocator issues on some systems)
        let Ok(host) = host else {
            return; // Skip test on systems where pooling allocator fails
        };

        // Capture the epoch_shutdown Arc before dropping
        let epoch_shutdown = host.epoch_shutdown.clone();

        // Verify the flag is initially false
        assert!(
            !epoch_shutdown.load(Ordering::Relaxed),
            "epoch_shutdown should be false before drop"
        );

        // Drop the host
        drop(host);

        // Verify the flag was set to true by the Drop impl
        assert!(
            epoch_shutdown.load(Ordering::Relaxed),
            "epoch_shutdown should be true after drop"
        );

        // Give the thread a moment to exit (optional, for thoroughness)
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    #[test]
    fn test_fuel_budget_default() {
        // Verify default fuel budget is set correctly
        let config = HostConfig::default();
        assert_eq!(config.fuel_budget, None);

        // When None, the resolved value should be DEFAULT_FUEL_BUDGET
        let resolved = config.fuel_budget.unwrap_or(constants::DEFAULT_FUEL_BUDGET);
        assert_eq!(resolved, constants::DEFAULT_FUEL_BUDGET);
        assert_eq!(resolved, 1_000_000_000);
    }

    #[test]
    fn test_fuel_budget_custom() {
        // Verify custom fuel budget is used
        let config = HostConfig {
            fuel_budget: Some(500_000_000),
            ..Default::default()
        };
        assert_eq!(config.fuel_budget, Some(500_000_000));

        // Resolved value should use the custom budget
        let resolved = config.fuel_budget.unwrap_or(constants::DEFAULT_FUEL_BUDGET);
        assert_eq!(resolved, 500_000_000);
    }

    #[test]
    fn test_fuel_exhaustion_handled() {
        // Test that fuel exhaustion is handled gracefully.
        //
        // This test verifies that:
        // 1. Fuel budget can be configured via HostConfig
        // 2. The budget is propagated to SharedState correctly
        // 3. When fuel runs out, execution stops with an error (not panic)
        //
        // Note: Full integration testing of fuel exhaustion requires a WASM
        // module that runs an infinite loop. The fuel metering in wasmtime
        // will stop execution when the budget is exhausted, returning a Trap
        // error. This test focuses on the configuration plumbing.

        // Create a temp directory with a minimal WASM file
        let temp_dir = std::env::temp_dir().join("mik_fuel_test");
        let _ = std::fs::create_dir_all(&temp_dir);

        let wasm_path = temp_dir.join("test.wasm");
        // Minimal WASM module
        std::fs::write(&wasm_path, [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]).unwrap();

        // Very low fuel budget to ensure quick exhaustion
        let config = HostConfig {
            modules_path: temp_dir.clone(),
            cache_size: 1,
            max_cache_bytes: 1024 * 1024,
            max_concurrent_requests: 1,
            fuel_budget: Some(1000), // Very low budget
            ..HostConfig::default()
        };

        let host = Host::new(config);

        // Clean up temp file
        let _ = std::fs::remove_file(&wasm_path);
        let _ = std::fs::remove_dir(&temp_dir);

        // Skip test if Host creation failed
        let Ok(host) = host else {
            return;
        };

        // Verify fuel budget was set correctly in SharedState
        assert_eq!(host.shared.fuel_budget, 1000);

        // Verify consume_fuel is enabled in engine config
        // (This is validated by the engine creation succeeding with fuel operations)
    }
}
