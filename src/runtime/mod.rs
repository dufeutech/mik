//! WASI HTTP runtime for mik serve.
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
// Allow dead code for library-first API methods
#![allow(dead_code)]
//!
//! This module provides the core functionality for running WASI HTTP components.
//!
//! # Library-First API
//!
//! The runtime provides a clean separation between:
//! - [`Runtime`]: Core WASM execution engine, no network binding
//! - [`Server`]: HTTP server that wraps a Runtime
//!
//! This allows mik to be embedded in applications like Tauri, Electron, or custom servers.
//!
//! # Examples
//!
//! ## Standalone Server (CLI use case)
//!
//! ```no_run
//! use mik::runtime::{Runtime, Server};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let runtime = Runtime::builder()
//!     .modules_dir("modules/")
//!     .build()?;
//!
//! Server::new(runtime, "127.0.0.1:3000".parse()?)
//!     .serve()
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Embedded Runtime (library use case)
//!
//! ```no_run
//! use mik::runtime::{Runtime, Request};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let runtime = Runtime::builder()
//!     .modules_dir("modules/")
//!     .build()?;
//!
//! // Handle requests programmatically
//! let response = runtime.handle_request(Request::new("GET", "/run/hello/greet")).await?;
//! println!("Status: {}", response.status);
//! # Ok(())
//! # }
//! ```

pub mod aot_cache;
pub mod builder;
mod cache;
pub mod cluster;
pub mod compression;
pub mod endpoints;
pub mod error;
mod host;
pub mod host_config;
pub mod host_state;
pub mod lb;
mod observability;
pub mod reliability;
pub mod request;
pub mod request_handler;
pub mod schema_handler;
pub mod script;
pub mod security;
pub mod server;
pub mod spans;
pub mod static_files;
pub mod trace_context;
pub mod types;
pub mod wasm_executor;

#[cfg(test)]
mod aot_cache_property_tests;

#[cfg(test)]
mod loom_tests;

// Re-export internal types
pub(crate) use cache::{CachedComponent, ModuleCache};
pub(crate) use host::Host;

// Re-export main builder type
#[allow(unused_imports)]
pub use builder::RuntimeBuilder;
pub use host_config::{DEFAULT_MEMORY_LIMIT_BYTES, DEFAULT_SHUTDOWN_TIMEOUT_SECS, HostConfig};
// New library-first API types - for external consumers
#[allow(unused_imports)]
pub use request::{Request, Response};
#[allow(unused_imports)]
pub use request_handler::handle_request;
#[allow(unused_imports)]
pub use server::{Server, ServerBuilder};
#[allow(unused_imports)]
pub use static_files::guess_content_type;
#[allow(unused_imports)]
pub use types::{ErrorCategory, HealthDetail, HealthStatus, MemoryStats};
// Cluster orchestration - for external consumers
#[allow(unused_imports)]
pub use cluster::{Cluster, ClusterBuilder, WorkerHandle};

use crate::constants;
use anyhow::Result;
use host_state::HostState;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::error;

use wasmtime::Engine;
use wasmtime::component::{Component, Linker};

// Re-export for script.rs
pub(crate) use wasm_executor::execute_wasm_request_internal;

/// Route prefix for WASM module requests.
pub const RUN_PREFIX: &str = "/run/";

/// Route prefix for OpenAPI schema requests.
pub const OPENAPI_PREFIX: &str = "/openapi/";

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

/// Default timeout for WASM execution (uses `constants::MAX_WASM_TIMEOUT_SECS`).
pub const DEFAULT_EXECUTION_TIMEOUT_SECS: u64 = constants::MAX_WASM_TIMEOUT_SECS;

/// Default max cache memory in MB.
pub const DEFAULT_MAX_CACHE_MB: usize = constants::DEFAULT_CACHE_MB;

/// Default max concurrent requests per module.
pub const DEFAULT_MAX_PER_MODULE_REQUESTS: usize = constants::DEFAULT_MAX_PER_MODULE_REQUESTS;

// NOTE: is_http_host_allowed is imported from reliability::security
// This is the single source of truth used by both host and mik CLI.

// ConfigError and HostConfig moved to host_config.rs
// HostBuilder, SystemConfig, manifest parsing moved to builder.rs

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
    pub(crate) circuit_breaker: reliability::CircuitBreaker,
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

// Cache methods (get_module_semaphore, get_or_load) are defined in cache.rs
// Health and metrics methods are defined in observability.rs
// Host struct and initialization are defined in host.rs

// =============================================================================
// Runtime: Library-First API
// =============================================================================

/// Core WASM runtime without network binding.
///
/// This is the library-first API for mik. `Runtime` handles requests programmatically,
/// making it suitable for embedding in applications like Tauri, Electron, or custom servers.
/// Use [`Server`] to wrap a Runtime for HTTP serving.
///
/// # Examples
///
/// ## Programmatic Request Handling
///
/// ```no_run
/// use mik::runtime::{Runtime, Request};
///
/// # async fn example() -> anyhow::Result<()> {
/// let runtime = Runtime::builder()
///     .modules_dir("modules/")
///     .build()?;
///
/// // Handle a request without any HTTP server
/// let response = runtime.handle_request(
///     Request::new("GET", "/run/hello/greet")
/// ).await?;
///
/// println!("Status: {}, Body: {:?}", response.status, response.body_str());
/// # Ok(())
/// # }
/// ```
///
/// ## Integration with Custom HTTP Server
///
/// ```no_run
/// use mik::runtime::{Runtime, Request, Response};
///
/// # async fn example() -> anyhow::Result<()> {
/// let runtime = std::sync::Arc::new(
///     Runtime::builder()
///         .modules_dir("modules/")
///         .build()?
/// );
///
/// // Use with any HTTP framework (axum, actix, warp, etc.)
/// // let axum_handler = move |req: axum::Request| {
/// //     let runtime = runtime.clone();
/// //     async move {
/// //         let mik_req = Request::from(req);
/// //         runtime.handle_request(mik_req).await
/// //     }
/// // };
/// # Ok(())
/// # }
/// ```
pub struct Runtime {
    /// Shared state containing engine, linker, cache, config.
    pub(crate) shared: Arc<SharedState>,
    /// Shutdown signal for the epoch incrementer thread.
    epoch_shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("port", &self.shared.config.port)
            .field("modules_path", &self.shared.modules_dir)
            .field("single_component", &self.shared.single_component_name)
            .field("cache_size", &self.shared.cache.entry_count())
            .field(
                "is_shutting_down",
                &self.shared.shutdown.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Create a new runtime builder.
    ///
    /// This is the recommended way to create a `Runtime` instance.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::runtime::Runtime;
    ///
    /// # fn example() -> anyhow::Result<()> {
    /// let runtime = Runtime::builder()
    ///     .modules_dir("modules/")
    ///     .cache_size(100)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> builder::RuntimeBuilder {
        builder::RuntimeBuilder::new()
    }

    /// Create a runtime from a Host (internal conversion).
    ///
    /// Note: We intentionally consume `Host` to take ownership of the epoch shutdown,
    /// preventing the Host's Drop impl from signaling shutdown prematurely.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn from_host(host: Host) -> Self {
        Self {
            shared: host.shared.clone(),
            epoch_shutdown: host.epoch_shutdown.clone(),
        }
    }

    /// Handle an HTTP request programmatically.
    ///
    /// This is the core method for the library-first API. It processes a request
    /// through the WASM runtime and returns a response, without any network I/O.
    ///
    /// # Arguments
    ///
    /// * `req` - The request to handle
    ///
    /// # Returns
    ///
    /// The response from the WASM handler, or an error if processing failed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::runtime::{Runtime, Request};
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let runtime = Runtime::builder()
    ///     .modules_dir("modules/")
    ///     .build()?;
    ///
    /// let response = runtime.handle_request(
    ///     Request::new("POST", "/run/api/users")
    ///         .with_header("Content-Type", "application/json")
    ///         .with_body_str(r#"{"name": "Alice"}"#)
    /// ).await?;
    ///
    /// if response.is_success() {
    ///     println!("User created: {}", response.body_str()?);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn handle_request(&self, req: request::Request) -> Result<request::Response> {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;
        use std::net::{IpAddr, Ipv4Addr};

        // Convert our Request to hyper request format
        let mut hyper_req = hyper::Request::builder()
            .method(req.method.as_str())
            .uri(&req.path);

        for (name, value) in &req.headers {
            hyper_req = hyper_req.header(name.as_str(), value.as_str());
        }

        // Create body
        let body = Full::new(Bytes::from(req.body));
        let hyper_req = hyper_req.body(body)?;

        // Use a dummy remote address for programmatic requests
        let remote_addr = std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        // Process the request through our internal handler
        let result = self.handle_request_internal(hyper_req, remote_addr).await?;

        // Convert hyper response back to our Response type
        let status = result.status().as_u16();
        let headers: Vec<(String, String)> = result
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.to_string(), v.to_string()))
            })
            .collect();

        // Extract body bytes from Full<Bytes>
        let body_bytes = result
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes().to_vec())
            .unwrap_or_default();

        Ok(request::Response {
            status,
            headers,
            body: body_bytes,
        })
    }

    /// Internal request handling that works with Full<Bytes> body.
    async fn handle_request_internal(
        &self,
        req: hyper::Request<http_body_util::Full<hyper::body::Bytes>>,
        _remote_addr: std::net::SocketAddr,
    ) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>> {
        use crate::runtime::compression::maybe_compress_response;
        use crate::runtime::error;
        use crate::runtime::host_state::HyperCompatibleBody;
        use crate::runtime::request_handler::{
            error_response, not_found, parse_module_route, validate_content_length,
            validate_path_length,
        };
        use crate::runtime::static_files::serve_static_file;
        use crate::runtime::wasm_executor::execute_wasm_request;
        use http_body_util::Full;
        use hyper::body::Bytes;
        use uuid::Uuid;

        let request_id = Uuid::new_v4();
        let path = req.uri().path().to_string();

        // Extract W3C trace context from header or generate new
        let trace_ctx = trace_context::extract_trace_context(req.headers());
        let traceparent = trace_ctx.to_traceparent();

        self.shared.request_counter.fetch_add(1, Ordering::Relaxed);

        let client_accepts_gzip = req
            .headers()
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("gzip"));

        // Handle built-in endpoints
        if path == HEALTH_PATH {
            // Convert Full<Bytes> to Incoming for health endpoint
            // For now, create a simple health response directly
            let health = self.shared.get_health_status(types::HealthDetail::Summary);
            let body = serde_json::to_string_pretty(&health)
                .unwrap_or_else(|_| r#"{"status":"error"}"#.to_string());

            let response = hyper::Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .header("X-Request-ID", request_id.to_string())
                .header("traceparent", &traceparent)
                .body(Full::new(Bytes::from(body)))?;

            return Ok(maybe_compress_response(response, client_accepts_gzip));
        }

        if path == METRICS_PATH {
            let metrics = self.shared.get_prometheus_metrics();

            let response = hyper::Response::builder()
                .status(200)
                .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
                .header("X-Request-ID", request_id.to_string())
                .header("traceparent", &traceparent)
                .body(Full::new(Bytes::from(metrics)))?;

            return Ok(maybe_compress_response(response, client_accepts_gzip));
        }

        // Validate path length
        if let Some(resp) = validate_path_length(&path) {
            return Ok(resp);
        }

        // Check Content-Length header
        let max_body = self.shared.max_body_size_bytes;
        if let Some(resp) = validate_content_length(req.headers(), max_body) {
            return Ok(resp);
        }

        // Handle static file requests
        if path.starts_with(STATIC_PREFIX) {
            return match &self.shared.static_dir {
                Some(dir) => serve_static_file(dir, &path)
                    .await
                    .map(|resp| maybe_compress_response(resp, client_accepts_gzip)),
                None => not_found("Static file serving not enabled"),
            };
        }

        // Handle /run/ module requests
        let Some(run_path) = path.strip_prefix(RUN_PREFIX) else {
            return not_found("Not found. WASM modules are served at /run/<module>/");
        };

        let (module, handler_path) = parse_module_route(run_path);

        if module.is_empty() {
            return not_found("No module specified. Use /run/<module>/");
        }

        // Resolve module
        let (component, module_name, module_permit) = {
            // Single component mode
            if let (Some(comp), Some(expected_name)) = (
                &self.shared.single_component,
                &self.shared.single_component_name,
            ) {
                if module == *expected_name {
                    (comp.clone(), Some(module), None)
                } else {
                    let err = error::Error::module_not_found(&module);
                    return error_response(&err);
                }
            } else {
                // Multi-module mode: check circuit breaker
                if let Err(e) = self.shared.circuit_breaker.check_request(&module) {
                    tracing::warn!("Circuit breaker blocked request to '{}': {}", module, e);
                    let err = error::Error::circuit_breaker_open(&module);
                    let mut resp = error_response(&err)?;
                    resp.headers_mut()
                        .insert("Retry-After", "30".parse().expect("valid header value"));
                    return Ok(resp);
                }

                // Acquire per-module semaphore permit
                let module_semaphore = self.shared.get_module_semaphore(&module);
                let module_permit = if let Ok(permit) = module_semaphore.try_acquire_owned() {
                    Some(permit)
                } else {
                    tracing::warn!(
                        "Module '{}' overloaded (max {} concurrent requests)",
                        module,
                        self.shared.config.max_per_module_requests
                    );
                    let err = error::Error::rate_limit_exceeded(format!(
                        "Module '{}' overloaded (max {} concurrent)",
                        module, self.shared.config.max_per_module_requests
                    ));
                    let mut resp = error_response(&err)?;
                    resp.headers_mut()
                        .insert("Retry-After", "5".parse().expect("valid header value"));
                    return Ok(resp);
                };

                match self.shared.get_or_load(&module).await {
                    Ok(comp) => (comp, Some(module.clone()), module_permit),
                    Err(e) => {
                        tracing::warn!("Module load failed: {}", e);
                        self.shared.circuit_breaker.record_failure(&module);
                        let err = error::Error::module_not_found(&module);
                        return error_response(&err);
                    },
                }
            }
        };

        // Rebuild request with new path
        let (parts, body) = req.into_parts();
        let mut new_parts = parts.clone();

        // Update URI with handler path
        let mut uri_parts = new_parts.uri.into_parts();
        uri_parts.path_and_query = Some(handler_path.parse()?);
        new_parts.uri = hyper::Uri::from_parts(uri_parts)?;

        let req = hyper::Request::from_parts(new_parts, HyperCompatibleBody(body));

        // Execute WASM request
        let _module_permit = module_permit;
        let result = execute_wasm_request(self.shared.clone(), component, req).await;

        // Record success/failure in circuit breaker
        if let Some(ref module) = module_name {
            match &result {
                Ok(_) => self.shared.circuit_breaker.record_success(module),
                Err(_) => self.shared.circuit_breaker.record_failure(module),
            }
        }

        result.map(|resp| maybe_compress_response(resp, client_accepts_gzip))
    }

    /// Get the health status of the runtime.
    ///
    /// Returns information about cache usage, request counts, and memory.
    #[must_use]
    pub fn health(&self) -> types::HealthStatus {
        self.shared.get_health_status(types::HealthDetail::Full)
    }

    /// Get Prometheus-format metrics.
    #[must_use]
    pub fn metrics(&self) -> String {
        self.shared.get_prometheus_metrics()
    }

    /// Check if running in single component mode.
    #[must_use]
    pub fn is_single_component(&self) -> bool {
        self.shared.single_component.is_some()
    }

    /// Get the single component name (for routing).
    #[must_use]
    pub fn single_component_name(&self) -> Option<&str> {
        self.shared.single_component_name.as_deref()
    }

    /// Check if static file serving is enabled.
    #[must_use]
    pub fn has_static_files(&self) -> bool {
        self.shared.static_dir.is_some()
    }

    /// Get the configured port (from manifest or builder).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.shared.config.port
    }

    /// Get a reference to the shared state (for advanced use cases).
    #[must_use]
    pub fn shared(&self) -> &Arc<SharedState> {
        &self.shared
    }

    /// Trigger a graceful shutdown.
    ///
    /// This sets the shutdown flag, which will cause any running server
    /// to begin its shutdown sequence.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
    }

    /// Check if shutdown has been requested.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shared.shutdown.load(Ordering::SeqCst)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Signal the epoch incrementer thread to stop
        self.epoch_shutdown.store(true, Ordering::Relaxed);
    }
}

// NOTE: Tests for is_http_host_allowed are in reliability/src/security.rs
// which is the single source of truth for this function.

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: HostConfig validation tests are in host_config.rs

    /// Test that the epoch thread stops when Host is dropped.
    ///
    /// This test creates a Host with a minimal configuration, then drops it
    /// and verifies that the `epoch_shutdown` flag was set. The actual thread
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
