//! Type definitions for mik.toml manifest.
//!
//! This module contains all struct and enum definitions for the manifest format.

use crate::reliability::is_http_host_allowed;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::defaults::{
    default_auto, default_execution_timeout, default_health_check_interval_ms,
    default_health_check_path, default_health_check_timeout_ms, default_health_check_type,
    default_healthy_threshold, default_http_handler, default_http2_only, default_lb_enabled,
    default_max_body_size_mb, default_max_connections_per_backend, default_modules_dir,
    default_pool_idle_timeout_secs, default_port, default_request_timeout_secs,
    default_service_name, default_shutdown_timeout, default_tcp_keepalive_secs,
    default_tracing_enabled, default_unhealthy_threshold, default_version,
};

// =============================================================================
// Main Manifest Structure
// =============================================================================

/// The main manifest structure (mik.toml).
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub tracing: TracingConfig,
    #[serde(default)]
    pub composition: CompositionConfig,
    #[serde(default)]
    pub lb: Option<LbConfig>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, Dependency>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            project: Project {
                name: "my-component".to_string(),
                version: default_version(),
                description: None,
                authors: vec![],
                language: None,
            },
            server: ServerConfig::default(),
            tracing: TracingConfig::default(),
            composition: CompositionConfig::default(),
            lb: None,
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
        }
    }
}

// =============================================================================
// Tracing Configuration
// =============================================================================

/// Tracing configuration for observability.
///
/// Controls distributed tracing and OTLP export to backends like Jaeger, Tempo, or Zipkin.
///
/// # Example
///
/// ```toml
/// [tracing]
/// enabled = true
/// otlp_endpoint = "http://localhost:4317"
/// service_name = "my-service"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Enable distributed tracing (default: true).
    ///
    /// When enabled, trace IDs are generated and propagated through requests.
    #[serde(default = "default_tracing_enabled")]
    pub enabled: bool,
    /// OTLP exporter endpoint (optional).
    ///
    /// When set, traces are exported via OTLP to backends like Jaeger or Tempo.
    /// Requires the `otlp` feature to be enabled.
    ///
    /// Example: `"http://localhost:4317"` (gRPC) or `"http://localhost:4318"` (HTTP)
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    /// Service name for traces (default: "mik").
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: default_tracing_enabled(),
            otlp_endpoint: None,
            service_name: default_service_name(),
        }
    }
}

// =============================================================================
// Server Configuration
// =============================================================================

/// Server configuration for the host runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Auto-configure based on system resources (default: true).
    ///
    /// When enabled, `cache_size`, `max_concurrent_requests`, and `max_per_module_requests`
    /// are automatically tuned based on available RAM and CPU cores.
    /// Set to false for predictable/reproducible deployments.
    #[serde(default = "default_auto")]
    pub auto: bool,
    /// Port to listen on (default: 3000)
    #[serde(default = "default_port")]
    pub port: u16,
    /// Directory containing WASM modules (default: "modules/")
    #[serde(default = "default_modules_dir")]
    pub modules: String,
    /// Directory containing JS/TS orchestration scripts (optional)
    #[serde(default)]
    pub scripts: Option<String>,
    /// Directory containing static files (default: "collected-static/")
    #[serde(default)]
    pub r#static: Option<String>,
    /// Maximum number of modules to cache (0 = auto-detect based on RAM).
    #[serde(default)]
    pub cache_size: usize,
    /// Maximum cache memory in MB (0 = auto-detect, ~10% of RAM).
    #[serde(default)]
    pub max_cache_mb: usize,
    /// Maximum request body size in MB (default: 10)
    #[serde(default = "default_max_body_size_mb")]
    pub max_body_size_mb: usize,
    /// WASM execution timeout in seconds (default: 30)
    #[serde(default = "default_execution_timeout")]
    pub execution_timeout_secs: u64,
    /// Maximum concurrent requests (0 = auto-detect based on CPU cores).
    #[serde(default)]
    pub max_concurrent_requests: usize,
    /// Maximum concurrent requests per module (0 = auto-detect).
    #[serde(default)]
    pub max_per_module_requests: usize,
    /// Graceful shutdown drain timeout in seconds (default: 30)
    #[serde(default = "default_shutdown_timeout")]
    pub shutdown_timeout_secs: u64,
    /// Enable wasi:logging for WASM modules (default: false).
    #[serde(default)]
    pub logging: bool,
    /// Allowed hosts for outgoing HTTP requests (default: empty = disabled).
    ///
    /// - `[]` or missing = disabled (no outgoing HTTP)
    /// - `["*"]` = allow all hosts
    /// - `["api.example.com", "*.supabase.co"]` = specific hosts only
    #[serde(default)]
    pub http_allowed: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            auto: default_auto(),
            port: default_port(),
            modules: default_modules_dir(),
            scripts: None,
            r#static: None,
            cache_size: 0,   // 0 = auto-detect
            max_cache_mb: 0, // 0 = auto-detect
            max_body_size_mb: default_max_body_size_mb(),
            execution_timeout_secs: default_execution_timeout(),
            max_concurrent_requests: 0, // 0 = auto-detect
            max_per_module_requests: 0, // 0 = auto-detect
            shutdown_timeout_secs: default_shutdown_timeout(),
            logging: false,
            http_allowed: Vec::new(),
        }
    }
}

impl ServerConfig {
    /// Check if outgoing HTTP is enabled.
    #[allow(dead_code)] // Public API for host runtime
    pub const fn http_enabled(&self) -> bool {
        !self.http_allowed.is_empty()
    }

    /// Check if all hosts are allowed for outgoing HTTP.
    #[allow(dead_code)] // Public API for host runtime
    pub fn http_allows_all(&self) -> bool {
        self.http_allowed.iter().any(|h| h == "*")
    }

    /// Check if a specific host is allowed for outgoing HTTP.
    ///
    /// Delegates to `mikrozen_reliability::is_http_host_allowed` which is the
    /// single source of truth for this logic.
    #[allow(dead_code)] // Public API for host runtime
    pub fn is_host_allowed(&self, host: &str) -> bool {
        is_http_host_allowed(host, &self.http_allowed)
    }
}

// =============================================================================
// Composition Configuration
// =============================================================================

/// Composition configuration for WASM component composition.
///
/// Controls automatic composition with mik-sdk bridge component
/// to convert handlers to WASI HTTP compatible components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionConfig {
    /// Enable automatic HTTP handler composition (default: true for "app" type).
    ///
    /// When enabled, handlers exporting `mik:core/handler` are automatically
    /// composed with bridge to export `wasi:http/incoming-handler`.
    #[serde(default = "default_http_handler")]
    pub http_handler: bool,
    /// Path to bridge component (optional, downloads from registry if not set).
    #[serde(default)]
    pub bridge: Option<String>,
}

impl Default for CompositionConfig {
    fn default() -> Self {
        Self {
            http_handler: default_http_handler(),
            bridge: None,
        }
    }
}

// =============================================================================
// Load Balancer Configuration
// =============================================================================

/// Load balancer configuration.
///
/// Controls the L7 load balancer that distributes requests across multiple backend workers.
///
/// # Example
///
/// ```toml
/// [lb]
/// enabled = true
/// health_check_type = "http"  # or "tcp"
/// health_check_interval_ms = 5000
/// health_check_timeout_ms = 2000
/// health_check_path = "/health"  # only used when health_check_type = "http"
/// unhealthy_threshold = 3
/// healthy_threshold = 2
/// request_timeout_secs = 30
/// max_connections_per_backend = 100
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LbConfig {
    /// Enable load balancing (default: false).
    #[serde(default = "default_lb_enabled")]
    pub enabled: bool,
    /// Type of health check to perform (default: `http`).
    ///
    /// Options:
    /// - `http` - HTTP GET request to `health_check_path`, expects 2xx response
    /// - `tcp` - TCP connection check, just verifies port is accepting connections
    #[serde(default = "default_health_check_type")]
    pub health_check_type: String,
    /// Interval between health checks in milliseconds (default: 5000).
    #[serde(default = "default_health_check_interval_ms")]
    pub health_check_interval_ms: u64,
    /// Timeout for each health check request in milliseconds (default: 2000).
    #[serde(default = "default_health_check_timeout_ms")]
    pub health_check_timeout_ms: u64,
    /// Path to check for HTTP health checks (default: `/health`).
    /// Only used when `health_check_type` = `http`.
    #[serde(default = "default_health_check_path")]
    pub health_check_path: String,
    /// Number of consecutive failures before marking backend unhealthy (default: 3).
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,
    /// Number of consecutive successes before marking backend healthy (default: 2).
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
    /// Request timeout in seconds (default: 30).
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Maximum concurrent connections per backend (default: 100).
    #[serde(default = "default_max_connections_per_backend")]
    pub max_connections_per_backend: usize,
    /// Pool idle timeout in seconds (default: 90).
    /// Connections idle longer than this are closed.
    #[serde(default = "default_pool_idle_timeout_secs")]
    pub pool_idle_timeout_secs: u64,
    /// TCP keepalive interval in seconds (default: 60).
    #[serde(default = "default_tcp_keepalive_secs")]
    pub tcp_keepalive_secs: u64,
    /// Use HTTP/2 only (with prior knowledge) for backend connections (default: false).
    /// Enable this when all backends support HTTP/2 for better performance.
    #[serde(default = "default_http2_only")]
    pub http2_only: bool,
}

impl Default for LbConfig {
    fn default() -> Self {
        Self {
            enabled: default_lb_enabled(),
            health_check_type: default_health_check_type(),
            health_check_interval_ms: default_health_check_interval_ms(),
            health_check_timeout_ms: default_health_check_timeout_ms(),
            health_check_path: default_health_check_path(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
            request_timeout_secs: default_request_timeout_secs(),
            max_connections_per_backend: default_max_connections_per_backend(),
            pool_idle_timeout_secs: default_pool_idle_timeout_secs(),
            tcp_keepalive_secs: default_tcp_keepalive_secs(),
            http2_only: default_http2_only(),
        }
    }
}

// =============================================================================
// Project Metadata
// =============================================================================

/// Project metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<Author>,
    /// Project language: rust (default), typescript, python, c
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Author information.
#[derive(Debug, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

// =============================================================================
// Dependency Types
// =============================================================================

/// Dependency specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    /// Simple version string: "1.0"
    Simple(String),
    /// Detailed specification
    Detailed(DependencyDetail),
}

/// Detailed dependency specification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyDetail {
    /// Version requirement
    #[serde(default)]
    pub version: Option<String>,
    /// Git repository URL
    #[serde(default)]
    pub git: Option<String>,
    /// Git branch
    #[serde(default)]
    pub branch: Option<String>,
    /// Git tag
    #[serde(default)]
    pub tag: Option<String>,
    /// Git revision
    #[serde(default)]
    pub rev: Option<String>,
    /// Local path
    #[serde(default)]
    pub path: Option<String>,
    /// Registry URL (ghcr.io, custom registry)
    #[serde(default)]
    pub registry: Option<String>,
}

#[allow(dead_code)]
impl Dependency {
    /// Create a simple version dependency.
    pub fn version(version: &str) -> Self {
        Self::Simple(version.to_string())
    }

    /// Create a git dependency.
    pub fn git(url: &str) -> Self {
        Self::Detailed(DependencyDetail {
            git: Some(url.to_string()),
            ..Default::default()
        })
    }

    /// Create a registry dependency (ghcr.io, etc).
    pub fn registry(registry: &str, version: &str) -> Self {
        Self::Detailed(DependencyDetail {
            registry: Some(registry.to_string()),
            version: Some(version.to_string()),
            ..Default::default()
        })
    }

    /// Create a path dependency.
    pub fn path(path: &str) -> Self {
        Self::Detailed(DependencyDetail {
            path: Some(path.to_string()),
            ..Default::default()
        })
    }
}
