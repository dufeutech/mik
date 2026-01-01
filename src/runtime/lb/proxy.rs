//! Core proxy logic for request forwarding without network binding.
//!
//! This module provides the [`Proxy`] struct which handles the core load balancing
//! logic without any network binding. It can be used:
//!
//! - Wrapped by [`LoadBalancer`](super::server::LoadBalancer) for CLI/network use
//! - Directly in embedded applications (Tauri, Electron, custom servers)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        Proxy (this module)                      │
//! │  - Pure request forwarding logic                                │
//! │  - Backend selection (round-robin, weighted, etc.)              │
//! │  - Health checking coordination                                 │
//! │  - NO network binding                                           │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                  LoadBalancer (server module)                   │
//! │  - Wraps Proxy                                                  │
//! │  - Binds to network address                                     │
//! │  - Handles TCP/HTTP connections                                 │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use mik::runtime::lb::{Proxy, ProxyBuilder, Backend, BackendHealth};
//!
//! // Option A: For CLI/network use
//! let lb = LoadBalancer::builder()
//!     .listen("0.0.0.0:8080".parse()?)
//!     .backend("127.0.0.1:3001")
//!     .backend("127.0.0.1:3002")
//!     .build()?;
//! lb.serve().await?;
//!
//! // Option B: For embedding use (no network binding)
//! let proxy = Proxy::builder()
//!     .backend(Backend::http("127.0.0.1:3001")?)
//!     .backend(Backend::runtime(my_runtime))
//!     .strategy(LoadBalanceStrategy::RoundRobin)
//!     .build()?;
//!
//! // Forward requests programmatically
//! let response = proxy.forward(request).await?;
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing::{debug, error, warn};

use super::backend::{Backend, BackendHealth, HttpBackend};
use super::health::{HealthCheckConfig, HealthChecker};
use super::metrics::LbMetrics;
use super::selection::{LoadBalanceStrategy, Selection};

// Import Request/Response types from runtime
pub use crate::runtime::request::{Request, Response};

/// Core proxy for request forwarding.
///
/// The `Proxy` handles the core load balancing logic:
/// - Maintains a list of backends (HTTP or embedded Runtime)
/// - Selects backends using configurable strategies
/// - Coordinates health checking
/// - Forwards requests to selected backends
///
/// It does NOT bind to any network address - use [`LoadBalancer`](super::server::LoadBalancer)
/// for network serving, or call [`forward`](Self::forward) directly for embedded use.
pub struct Proxy {
    /// List of backends to load balance across.
    backends: Arc<RwLock<Vec<Backend>>>,
    /// Health checker for backend monitoring.
    health_checker: Option<Arc<HealthChecker>>,
    /// Load balancing strategy.
    strategy: Arc<RwLock<Box<dyn Selection>>>,
    /// Request timeout for forwarding.
    request_timeout: Duration,
    /// HTTP client for HTTP backends (shared across all backends).
    http_client: reqwest::Client,
    /// Metrics collector.
    metrics: LbMetrics,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy")
            .field("request_timeout", &self.request_timeout)
            .field("has_health_checker", &self.health_checker.is_some())
            .finish_non_exhaustive()
    }
}

impl Proxy {
    /// Create a new `ProxyBuilder`.
    pub fn builder() -> ProxyBuilder {
        ProxyBuilder::new()
    }

    /// Forward a request to a selected backend.
    ///
    /// This method:
    /// 1. Selects an available backend using the configured strategy
    /// 2. Forwards the request to the selected backend
    /// 3. Records metrics and updates backend health
    /// 4. Returns the response or an error
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No healthy backends are available
    /// - All backends are at connection capacity
    /// - The backend request fails
    pub async fn forward(&self, req: Request) -> Result<Response> {
        let start = Instant::now();

        // Select an available backend
        let backend = match self.select_backend().await {
            SelectResult::Selected(backend) => backend,
            SelectResult::NoHealthyBackends => {
                warn!("No healthy backends available");
                return Ok(Response {
                    status: 503,
                    headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
                    body: b"No healthy backends available".to_vec(),
                });
            },
            SelectResult::AllAtCapacity => {
                warn!("All backends at connection capacity");
                return Ok(Response {
                    status: 503,
                    headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
                    body: b"All backends at connection capacity".to_vec(),
                });
            },
        };

        let backend_id = backend.id().to_string();

        // Track request start
        backend.start_request();
        self.metrics
            .set_active_connections(&backend_id, backend.active_requests());

        // Forward the request
        let result = backend
            .forward(&req, &self.http_client, self.request_timeout)
            .await;

        // Track request end
        backend.end_request();
        self.metrics
            .set_active_connections(&backend_id, backend.active_requests());

        let duration = start.elapsed().as_secs_f64();

        match result {
            Ok(response) => {
                backend.record_success();
                self.metrics.record_success(&backend_id, duration);
                debug!(
                    backend = %backend_id,
                    status = response.status,
                    duration_ms = %(duration * 1000.0),
                    "Request completed"
                );
                Ok(response)
            },
            Err(e) => {
                backend.record_failure();
                self.metrics.record_failure(&backend_id, duration);
                error!(
                    backend = %backend_id,
                    error = %e,
                    duration_ms = %(duration * 1000.0),
                    "Request failed"
                );
                Ok(Response {
                    status: 502,
                    headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
                    body: format!("Backend error: {e}").into_bytes(),
                })
            },
        }
    }

    /// Get the list of backends.
    pub async fn backends(&self) -> Vec<Backend> {
        self.backends.read().await.clone()
    }

    /// Get health status for all backends.
    pub async fn health_status(&self) -> Vec<BackendHealth> {
        let backends = self.backends.read().await;
        backends.iter().map(Backend::health).collect()
    }

    /// Start background health checking.
    ///
    /// Returns a handle that can be used to stop health checking.
    pub fn start_health_checks(&self) -> Option<tokio::task::JoinHandle<()>> {
        let health_checker = self.health_checker.clone()?;
        let backends = self.backends.clone();
        let metrics = self.metrics.clone();

        Some(tokio::spawn(async move {
            health_checker.run(backends, metrics).await;
        }))
    }

    /// Select an available backend using the load balancing strategy.
    async fn select_backend(&self) -> SelectResult {
        let backends = self.backends.read().await;
        let strategy = self.strategy.read().await;

        // Get indices of available backends (healthy + circuit breaker allows)
        let available_indices: Vec<usize> = backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_available())
            .map(|(i, _)| i)
            .collect();

        if available_indices.is_empty() {
            return SelectResult::NoHealthyBackends;
        }

        // Get indices with connection capacity
        let capacity_indices: Vec<usize> = available_indices
            .iter()
            .copied()
            .filter(|&i| backends[i].has_capacity())
            .collect();

        if capacity_indices.is_empty() {
            return SelectResult::AllAtCapacity;
        }

        // Select using the strategy
        strategy
            .select(&capacity_indices)
            .map_or(SelectResult::AllAtCapacity, |idx| {
                SelectResult::Selected(backends[idx].clone())
            })
    }
}

/// Result of backend selection.
enum SelectResult {
    /// A backend was successfully selected.
    Selected(Backend),
    /// No healthy backends are available.
    NoHealthyBackends,
    /// All healthy backends are at connection capacity.
    AllAtCapacity,
}

/// Builder for [`Proxy`].
///
/// Use this to configure and create a `Proxy` instance.
///
/// # Example
///
/// ```ignore
/// let proxy = Proxy::builder()
///     .backend(Backend::http("127.0.0.1:3001")?)
///     .backend(Backend::http("127.0.0.1:3002")?)
///     .strategy(LoadBalanceStrategy::RoundRobin)
///     .health_check(HealthCheckConfig::http("/health"))
///     .request_timeout(Duration::from_secs(30))
///     .build()?;
/// ```
#[derive(Default)]
pub struct ProxyBuilder {
    backends: Vec<Backend>,
    strategy: LoadBalanceStrategy,
    health_check: Option<HealthCheckConfig>,
    request_timeout: Duration,
    max_connections_per_backend: usize,
    pool_idle_timeout: Duration,
    tcp_keepalive: Duration,
    http2_only: bool,
}

impl ProxyBuilder {
    /// Create a new `ProxyBuilder` with default settings.
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
            strategy: LoadBalanceStrategy::RoundRobin,
            health_check: Some(HealthCheckConfig::default()),
            request_timeout: Duration::from_secs(30),
            max_connections_per_backend: 100,
            pool_idle_timeout: Duration::from_secs(90),
            tcp_keepalive: Duration::from_secs(60),
            http2_only: false,
        }
    }

    /// Add a backend to the proxy.
    #[must_use]
    pub fn backend(mut self, backend: Backend) -> Self {
        self.backends.push(backend);
        self
    }

    /// Add an HTTP backend by address string.
    ///
    /// # Errors
    ///
    /// Returns an error if the address cannot be parsed.
    #[must_use]
    pub fn http_backend(mut self, address: impl Into<String>) -> Self {
        self.backends
            .push(Backend::Http(HttpBackend::new(address.into())));
        self
    }

    /// Set the load balancing strategy.
    #[must_use]
    pub fn strategy(mut self, strategy: LoadBalanceStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Set the health check configuration.
    ///
    /// Pass `None` to disable health checking.
    #[must_use]
    pub fn health_check(mut self, config: Option<HealthCheckConfig>) -> Self {
        self.health_check = config;
        self
    }

    /// Set the request timeout.
    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Set the maximum connections per backend for HTTP backends.
    #[must_use]
    pub fn max_connections_per_backend(mut self, max: usize) -> Self {
        self.max_connections_per_backend = max;
        self
    }

    /// Set the connection pool idle timeout.
    #[must_use]
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.pool_idle_timeout = timeout;
        self
    }

    /// Set the TCP keepalive interval.
    #[must_use]
    pub fn tcp_keepalive(mut self, interval: Duration) -> Self {
        self.tcp_keepalive = interval;
        self
    }

    /// Enable HTTP/2 only mode for backend connections.
    ///
    /// When enabled, HTTP/2 with prior knowledge is used for all backend connections.
    /// Only enable this when all backends support HTTP/2.
    #[must_use]
    pub fn http2_only(mut self, enabled: bool) -> Self {
        self.http2_only = enabled;
        self
    }

    /// Build the [`Proxy`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No backends were configured
    /// - The HTTP client cannot be created
    pub fn build(self) -> Result<Proxy> {
        if self.backends.is_empty() {
            anyhow::bail!("At least one backend must be configured");
        }

        // Create HTTP client with connection pooling
        let mut client_builder = reqwest::Client::builder()
            .timeout(self.request_timeout)
            .pool_max_idle_per_host(self.max_connections_per_backend)
            .pool_idle_timeout(self.pool_idle_timeout)
            .tcp_keepalive(self.tcp_keepalive);

        if self.http2_only {
            client_builder = client_builder.http2_prior_knowledge();
        }

        let http_client = client_builder
            .build()
            .context("Failed to create HTTP client")?;

        // Create selection strategy
        let strategy: Box<dyn Selection> = self.strategy.into_selector(self.backends.len());

        // Create health checker if configured
        let health_checker = self
            .health_check
            .map(|config| Arc::new(HealthChecker::new(config)));

        Ok(Proxy {
            backends: Arc::new(RwLock::new(self.backends)),
            health_checker,
            strategy: Arc::new(RwLock::new(strategy)),
            request_timeout: self.request_timeout,
            http_client,
            metrics: LbMetrics::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_builder_requires_backends() {
        let result = Proxy::builder().build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("backend"));
    }

    #[test]
    fn test_proxy_builder_with_http_backend() {
        let result = Proxy::builder()
            .http_backend("127.0.0.1:3001")
            .http_backend("127.0.0.1:3002")
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_proxy_builder_configuration() {
        let result = Proxy::builder()
            .http_backend("127.0.0.1:3001")
            .strategy(LoadBalanceStrategy::RoundRobin)
            .request_timeout(Duration::from_secs(60))
            .max_connections_per_backend(200)
            .pool_idle_timeout(Duration::from_secs(120))
            .tcp_keepalive(Duration::from_secs(30))
            .http2_only(true)
            .health_check(Some(HealthCheckConfig::http("/healthz")))
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_proxy_builder_no_health_check() {
        let result = Proxy::builder()
            .http_backend("127.0.0.1:3001")
            .health_check(None)
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_request_builder() {
        let req = Request::new("GET", "/api/users")
            .with_header("Accept", "application/json")
            .with_body(b"test".to_vec());

        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/api/users");
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.body, b"test");
    }

    #[test]
    fn test_response_builders() {
        let ok = Response::ok();
        assert_eq!(ok.status, 200);

        let not_found = Response::not_found("not found");
        assert_eq!(not_found.status, 404);

        let error = Response::internal_error("oops");
        assert_eq!(error.status, 500);

        let bad_gateway = Response::bad_gateway("backend failed");
        assert_eq!(bad_gateway.status, 502);

        let unavailable = Response::service_unavailable("no backends");
        assert_eq!(unavailable.status, 503);
    }
}
