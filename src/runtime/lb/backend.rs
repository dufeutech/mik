//! Backend abstraction for load balancer.
//!
//! This module provides the [`Backend`] enum which abstracts over different backend types:
//!
//! - [`Backend::Http`] - Network backend for HTTP requests (current behavior)
//! - [`Backend::Runtime`] - Embedded runtime for in-process request handling (new)
//!
//! This abstraction enables:
//! - CLI use: Load balance across network backends
//! - Embedding use: Load balance across in-process runtimes
//! - Hybrid use: Mix network and embedded backends
//!
//! # Example
//!
//! ```ignore
//! use mik::runtime::lb::{Backend, Proxy};
//!
//! // Network backend
//! let http_backend = Backend::http("127.0.0.1:3001");
//!
//! // Embedded runtime backend
//! let runtime = Arc::new(Runtime::builder().build()?);
//! let runtime_backend = Backend::runtime(runtime);
//!
//! // Mix in a single proxy
//! let proxy = Proxy::builder()
//!     .backend(http_backend)
//!     .backend(runtime_backend)
//!     .build()?;
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::RwLock;
use tracing::debug;

use super::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::runtime::request::{Request, Response};

// Forward declare Runtime type - will be imported from runtime module when Agent A completes
// For now, we use a trait object approach
/// Trait for runtime backends that can handle requests.
#[async_trait::async_trait]
pub trait RuntimeHandler: Send + Sync {
    /// Handle a request and return a response.
    async fn handle_request(&self, req: &Request) -> Result<Response>;

    /// Get the runtime identifier.
    fn id(&self) -> &str;

    /// Perform a health check.
    async fn health_check(&self) -> bool;
}

/// Backend for load balancing.
///
/// Represents a single backend that can receive proxied requests.
/// Supports both network (HTTP) and embedded (Runtime) backends.
#[derive(Clone)]
pub enum Backend {
    /// Network backend - forwards requests over HTTP.
    Http(HttpBackend),
    /// Embedded runtime backend - handles requests in-process.
    Runtime(RuntimeBackend),
}

impl Backend {
    /// Create an HTTP backend from an address string.
    pub fn http(address: impl Into<String>) -> Self {
        Self::Http(HttpBackend::new(address.into()))
    }

    /// Create an HTTP backend with custom options.
    pub fn http_with_options(
        address: impl Into<String>,
        weight: u32,
        max_connections: u32,
    ) -> Self {
        Self::Http(HttpBackend::with_options(
            address.into(),
            weight,
            max_connections,
        ))
    }

    /// Create an HTTP backend with a circuit breaker.
    pub fn http_with_circuit_breaker(
        address: impl Into<String>,
        config: CircuitBreakerConfig,
    ) -> Self {
        Self::Http(HttpBackend::with_circuit_breaker(address.into(), config))
    }

    /// Create an embedded runtime backend.
    pub fn runtime(handler: Arc<dyn RuntimeHandler>) -> Self {
        Self::Runtime(RuntimeBackend::new(handler))
    }

    /// Create an embedded runtime backend with custom options.
    pub fn runtime_with_options(
        handler: Arc<dyn RuntimeHandler>,
        weight: u32,
        max_connections: u32,
    ) -> Self {
        Self::Runtime(RuntimeBackend::with_options(
            handler,
            weight,
            max_connections,
        ))
    }

    /// Get the backend identifier.
    pub fn id(&self) -> &str {
        match self {
            Self::Http(b) => b.address(),
            Self::Runtime(b) => b.id(),
        }
    }

    /// Check if the backend is healthy.
    pub fn is_healthy(&self) -> bool {
        match self {
            Self::Http(b) => b.is_healthy(),
            Self::Runtime(b) => b.is_healthy(),
        }
    }

    /// Check if the backend is available for requests.
    ///
    /// Returns true if the backend is healthy AND the circuit breaker allows requests.
    pub fn is_available(&self) -> bool {
        match self {
            Self::Http(b) => b.is_available(),
            Self::Runtime(b) => b.is_available(),
        }
    }

    /// Check if the backend has capacity for more connections.
    pub fn has_capacity(&self) -> bool {
        match self {
            Self::Http(b) => b.has_capacity(),
            Self::Runtime(b) => b.has_capacity(),
        }
    }

    /// Mark the backend as healthy.
    pub fn mark_healthy(&self) {
        match self {
            Self::Http(b) => b.mark_healthy(),
            Self::Runtime(b) => b.mark_healthy(),
        }
    }

    /// Mark the backend as unhealthy.
    pub fn mark_unhealthy(&self) {
        match self {
            Self::Http(b) => b.mark_unhealthy(),
            Self::Runtime(b) => b.mark_unhealthy(),
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        match self {
            Self::Http(b) => b.record_success(),
            Self::Runtime(b) => b.record_success(),
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        match self {
            Self::Http(b) => b.record_failure(),
            Self::Runtime(b) => b.record_failure(),
        }
    }

    /// Increment active request count.
    pub fn start_request(&self) {
        match self {
            Self::Http(b) => b.start_request(),
            Self::Runtime(b) => b.start_request(),
        }
    }

    /// Decrement active request count.
    pub fn end_request(&self) {
        match self {
            Self::Http(b) => b.end_request(),
            Self::Runtime(b) => b.end_request(),
        }
    }

    /// Get the number of active requests.
    pub fn active_requests(&self) -> u64 {
        match self {
            Self::Http(b) => b.active_requests(),
            Self::Runtime(b) => b.active_requests(),
        }
    }

    /// Get the consecutive success count.
    pub fn success_count(&self) -> u64 {
        match self {
            Self::Http(b) => b.success_count(),
            Self::Runtime(b) => b.success_count(),
        }
    }

    /// Get the consecutive failure count.
    pub fn failure_count(&self) -> u64 {
        match self {
            Self::Http(b) => b.failure_count(),
            Self::Runtime(b) => b.failure_count(),
        }
    }

    /// Get health status for this backend.
    pub fn health(&self) -> BackendHealth {
        match self {
            Self::Http(b) => b.health(),
            Self::Runtime(b) => b.health(),
        }
    }

    /// Forward a request to this backend.
    pub async fn forward(
        &self,
        req: &Request,
        http_client: &reqwest::Client,
        timeout: Duration,
    ) -> Result<Response> {
        match self {
            Self::Http(b) => b.forward(req, http_client, timeout).await,
            Self::Runtime(b) => b.forward(req).await,
        }
    }

    /// Perform a health check on this backend.
    pub async fn health_check(&self, http_client: &reqwest::Client, path: &str) -> bool {
        match self {
            Self::Http(b) => b.health_check(http_client, path).await,
            Self::Runtime(b) => b.health_check().await,
        }
    }
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(b) => f.debug_tuple("Http").field(&b.address()).finish(),
            Self::Runtime(b) => f.debug_tuple("Runtime").field(&b.id()).finish(),
        }
    }
}

/// Health status for a backend.
#[derive(Debug, Clone)]
pub struct BackendHealth {
    /// Backend identifier.
    pub backend_id: String,
    /// Whether the backend is healthy.
    pub healthy: bool,
    /// Last observed latency in milliseconds.
    pub latency_ms: Option<u64>,
    /// When the last health check was performed.
    pub last_check: Option<Instant>,
    /// Number of active requests.
    pub active_requests: u64,
    /// Total requests handled.
    pub total_requests: u64,
    /// Backend type.
    pub backend_type: BackendType,
}

/// Type of backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// HTTP network backend.
    Http,
    /// Embedded runtime backend.
    Runtime,
}

impl fmt::Display for BackendType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http => write!(f, "http"),
            Self::Runtime => write!(f, "runtime"),
        }
    }
}

// ============================================================================
// HTTP Backend Implementation
// ============================================================================

/// Default maximum concurrent connections per backend.
pub(super) const DEFAULT_MAX_CONNECTIONS: u32 = 100;

/// State of a backend server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BackendState {
    /// Backend is healthy and accepting requests.
    Healthy,
    /// Backend is unhealthy and should not receive requests.
    Unhealthy,
    /// Backend health is unknown (initial state).
    Unknown,
    /// Backend is being drained (no new requests, completing existing ones).
    Draining,
}

impl fmt::Display for BackendState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
            Self::Draining => "draining",
        };
        write!(f, "{s}")
    }
}

/// HTTP backend that forwards requests over the network.
#[derive(Debug)]
pub struct HttpBackend {
    /// Address of the backend (e.g., "127.0.0.1:3001").
    address: String,
    /// Weight for weighted load balancing (higher = more traffic).
    weight: u32,
    /// Maximum concurrent connections allowed for this backend.
    max_connections: u32,
    /// Whether the backend is currently healthy.
    healthy: AtomicBool,
    /// Number of consecutive health check failures.
    failure_count: AtomicU64,
    /// Number of consecutive health check successes.
    success_count: AtomicU64,
    /// Total requests handled.
    total_requests: AtomicU64,
    /// Currently active requests.
    active_requests: AtomicU64,
    /// Last health check time.
    last_check: RwLock<Option<Instant>>,
    /// Last successful response time.
    last_success: RwLock<Option<Instant>>,
    /// Last observed latency.
    last_latency_ms: AtomicU64,
    /// Optional circuit breaker for this backend.
    circuit_breaker: Option<CircuitBreaker>,
}

impl HttpBackend {
    /// Create a new HTTP backend with the given address.
    pub fn new(address: String) -> Self {
        Self::with_options(address, 1, DEFAULT_MAX_CONNECTIONS)
    }

    /// Create a new HTTP backend with custom options.
    pub fn with_options(address: String, weight: u32, max_connections: u32) -> Self {
        Self {
            address,
            weight: weight.max(1),
            max_connections: max_connections.max(1),
            healthy: AtomicBool::new(true),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            last_check: RwLock::new(None),
            last_success: RwLock::new(None),
            last_latency_ms: AtomicU64::new(0),
            circuit_breaker: None,
        }
    }

    /// Create a new HTTP backend with a circuit breaker.
    pub const fn with_circuit_breaker(address: String, config: CircuitBreakerConfig) -> Self {
        Self {
            address,
            weight: 1,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            healthy: AtomicBool::new(true),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            last_check: RwLock::new(None),
            last_success: RwLock::new(None),
            last_latency_ms: AtomicU64::new(0),
            circuit_breaker: Some(CircuitBreaker::new(config)),
        }
    }

    /// Get the backend address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Get the full URL for a given path.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }

    /// Check if the backend is healthy.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Check if the backend is available (healthy + circuit breaker allows).
    pub fn is_available(&self) -> bool {
        if !self.is_healthy() {
            return false;
        }
        if let Some(ref cb) = self.circuit_breaker {
            return cb.is_available();
        }
        true
    }

    /// Check if the backend has capacity for more connections.
    pub fn has_capacity(&self) -> bool {
        self.active_requests.load(Ordering::Acquire) < u64::from(self.max_connections)
    }

    /// Get the current state of the backend.
    #[allow(dead_code)]
    pub fn state(&self) -> BackendState {
        if self.last_check.read().is_none() {
            BackendState::Unknown
        } else if self.is_healthy() {
            BackendState::Healthy
        } else {
            BackendState::Unhealthy
        }
    }

    /// Mark the backend as healthy.
    pub fn mark_healthy(&self) {
        self.healthy.store(true, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
        self.success_count.fetch_add(1, Ordering::AcqRel);
        *self.last_check.write() = Some(Instant::now());
        *self.last_success.write() = Some(Instant::now());
    }

    /// Mark the backend as unhealthy.
    pub fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Release);
        self.success_count.store(0, Ordering::Release);
        self.failure_count.fetch_add(1, Ordering::AcqRel);
        *self.last_check.write() = Some(Instant::now());
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        *self.last_success.write() = Some(Instant::now());
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_success();
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_failure();
        }
    }

    /// Increment active request count.
    pub fn start_request(&self) {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement active request count.
    pub fn end_request(&self) {
        self.active_requests.fetch_sub(1, Ordering::AcqRel);
    }

    /// Get the number of active requests.
    pub fn active_requests(&self) -> u64 {
        self.active_requests.load(Ordering::Acquire)
    }

    /// Get the total number of requests handled.
    #[allow(dead_code)]
    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Get the consecutive failure count.
    pub fn failure_count(&self) -> u64 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Get the consecutive success count.
    pub fn success_count(&self) -> u64 {
        self.success_count.load(Ordering::Relaxed)
    }

    /// Get health status for this backend.
    pub fn health(&self) -> BackendHealth {
        let last_latency = self.last_latency_ms.load(Ordering::Relaxed);
        BackendHealth {
            backend_id: self.address.clone(),
            healthy: self.is_healthy(),
            latency_ms: if last_latency > 0 {
                Some(last_latency)
            } else {
                None
            },
            last_check: *self.last_check.read(),
            active_requests: self.active_requests(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            backend_type: BackendType::Http,
        }
    }

    /// Forward a request to this backend.
    pub async fn forward(
        &self,
        req: &Request,
        client: &reqwest::Client,
        timeout: Duration,
    ) -> Result<Response> {
        let start = Instant::now();
        let url = self.url(&req.path);

        // Build the request
        let method = req.method.parse().unwrap_or(reqwest::Method::GET);
        let mut builder = client.request(method, &url).timeout(timeout);

        // Add headers (skip hop-by-hop)
        for (name, value) in &req.headers {
            if !is_hop_by_hop_header(name) {
                builder = builder.header(name.as_str(), value.as_str());
            }
        }

        // Add body if not empty
        if !req.body.is_empty() {
            builder = builder.body(req.body.clone());
        }

        // Send request
        let response = builder.send().await?;

        // Record latency
        let latency_ms = start.elapsed().as_millis() as u64;
        self.last_latency_ms.store(latency_ms, Ordering::Relaxed);

        // Build response
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter(|(name, _)| !is_hop_by_hop_header(name.as_str()))
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = response.bytes().await?.to_vec();

        Ok(Response {
            status,
            headers,
            body,
        })
    }

    /// Perform an HTTP health check.
    pub async fn health_check(&self, client: &reqwest::Client, path: &str) -> bool {
        let url = self.url(path);
        let start = Instant::now();

        match client.get(&url).send().await {
            Ok(response) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                self.last_latency_ms.store(latency_ms, Ordering::Relaxed);

                let healthy = response.status().is_success();
                if healthy {
                    debug!(backend = %self.address, latency_ms, "HTTP health check passed");
                } else {
                    debug!(
                        backend = %self.address,
                        status = %response.status(),
                        "HTTP health check failed with status"
                    );
                }
                healthy
            },
            Err(e) => {
                debug!(
                    backend = %self.address,
                    error = %e,
                    "HTTP health check failed"
                );
                false
            },
        }
    }
}

impl Clone for HttpBackend {
    fn clone(&self) -> Self {
        Self {
            address: self.address.clone(),
            weight: self.weight,
            max_connections: self.max_connections,
            healthy: AtomicBool::new(self.healthy.load(Ordering::Acquire)),
            failure_count: AtomicU64::new(self.failure_count.load(Ordering::Relaxed)),
            success_count: AtomicU64::new(self.success_count.load(Ordering::Relaxed)),
            total_requests: AtomicU64::new(self.total_requests.load(Ordering::Relaxed)),
            active_requests: AtomicU64::new(self.active_requests.load(Ordering::Relaxed)),
            last_check: RwLock::new(*self.last_check.read()),
            last_success: RwLock::new(*self.last_success.read()),
            last_latency_ms: AtomicU64::new(self.last_latency_ms.load(Ordering::Relaxed)),
            circuit_breaker: self.circuit_breaker.clone(),
        }
    }
}

// ============================================================================
// Runtime Backend Implementation
// ============================================================================

/// Embedded runtime backend that handles requests in-process.
pub struct RuntimeBackend {
    /// The runtime handler.
    handler: Arc<dyn RuntimeHandler>,
    /// Weight for weighted load balancing.
    weight: u32,
    /// Maximum concurrent requests.
    max_connections: u32,
    /// Whether the backend is currently healthy.
    healthy: AtomicBool,
    /// Number of consecutive health check failures.
    failure_count: AtomicU64,
    /// Number of consecutive health check successes.
    success_count: AtomicU64,
    /// Total requests handled.
    total_requests: AtomicU64,
    /// Currently active requests.
    active_requests: AtomicU64,
    /// Last health check time.
    last_check: RwLock<Option<Instant>>,
    /// Last observed latency.
    last_latency_ms: AtomicU64,
    /// Optional circuit breaker.
    circuit_breaker: Option<CircuitBreaker>,
}

impl RuntimeBackend {
    /// Create a new runtime backend.
    pub fn new(handler: Arc<dyn RuntimeHandler>) -> Self {
        Self::with_options(handler, 1, DEFAULT_MAX_CONNECTIONS)
    }

    /// Create a new runtime backend with custom options.
    pub fn with_options(
        handler: Arc<dyn RuntimeHandler>,
        weight: u32,
        max_connections: u32,
    ) -> Self {
        Self {
            handler,
            weight: weight.max(1),
            max_connections: max_connections.max(1),
            healthy: AtomicBool::new(true),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            last_check: RwLock::new(None),
            last_latency_ms: AtomicU64::new(0),
            circuit_breaker: None,
        }
    }

    /// Get the backend identifier.
    pub fn id(&self) -> &str {
        self.handler.id()
    }

    /// Check if the backend is healthy.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Check if the backend is available.
    pub fn is_available(&self) -> bool {
        if !self.is_healthy() {
            return false;
        }
        if let Some(ref cb) = self.circuit_breaker {
            return cb.is_available();
        }
        true
    }

    /// Check if the backend has capacity.
    pub fn has_capacity(&self) -> bool {
        self.active_requests.load(Ordering::Acquire) < u64::from(self.max_connections)
    }

    /// Mark the backend as healthy.
    pub fn mark_healthy(&self) {
        self.healthy.store(true, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
        self.success_count.fetch_add(1, Ordering::AcqRel);
        *self.last_check.write() = Some(Instant::now());
    }

    /// Mark the backend as unhealthy.
    pub fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Release);
        self.success_count.store(0, Ordering::Release);
        self.failure_count.fetch_add(1, Ordering::AcqRel);
        *self.last_check.write() = Some(Instant::now());
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_success();
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_failure();
        }
    }

    /// Increment active request count.
    pub fn start_request(&self) {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement active request count.
    pub fn end_request(&self) {
        self.active_requests.fetch_sub(1, Ordering::AcqRel);
    }

    /// Get the number of active requests.
    pub fn active_requests(&self) -> u64 {
        self.active_requests.load(Ordering::Acquire)
    }

    /// Get the consecutive success count.
    pub fn success_count(&self) -> u64 {
        self.success_count.load(Ordering::Relaxed)
    }

    /// Get the consecutive failure count.
    pub fn failure_count(&self) -> u64 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Get health status for this backend.
    pub fn health(&self) -> BackendHealth {
        let last_latency = self.last_latency_ms.load(Ordering::Relaxed);
        BackendHealth {
            backend_id: self.handler.id().to_string(),
            healthy: self.is_healthy(),
            latency_ms: if last_latency > 0 {
                Some(last_latency)
            } else {
                None
            },
            last_check: *self.last_check.read(),
            active_requests: self.active_requests(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            backend_type: BackendType::Runtime,
        }
    }

    /// Forward a request to this backend.
    pub async fn forward(&self, req: &Request) -> Result<Response> {
        let start = Instant::now();
        let result = self.handler.handle_request(req).await;

        let latency_ms = start.elapsed().as_millis() as u64;
        self.last_latency_ms.store(latency_ms, Ordering::Relaxed);

        result
    }

    /// Perform a health check.
    pub async fn health_check(&self) -> bool {
        let start = Instant::now();
        let healthy = self.handler.health_check().await;

        let latency_ms = start.elapsed().as_millis() as u64;
        self.last_latency_ms.store(latency_ms, Ordering::Relaxed);

        if healthy {
            debug!(backend = %self.handler.id(), latency_ms, "Runtime health check passed");
        } else {
            debug!(backend = %self.handler.id(), "Runtime health check failed");
        }

        healthy
    }
}

impl Clone for RuntimeBackend {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            weight: self.weight,
            max_connections: self.max_connections,
            healthy: AtomicBool::new(self.healthy.load(Ordering::Acquire)),
            failure_count: AtomicU64::new(self.failure_count.load(Ordering::Relaxed)),
            success_count: AtomicU64::new(self.success_count.load(Ordering::Relaxed)),
            total_requests: AtomicU64::new(self.total_requests.load(Ordering::Relaxed)),
            active_requests: AtomicU64::new(self.active_requests.load(Ordering::Relaxed)),
            last_check: RwLock::new(*self.last_check.read()),
            last_latency_ms: AtomicU64::new(self.last_latency_ms.load(Ordering::Relaxed)),
            circuit_breaker: self.circuit_breaker.clone(),
        }
    }
}

impl fmt::Debug for RuntimeBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeBackend")
            .field("id", &self.handler.id())
            .field("healthy", &self.is_healthy())
            .field("active_requests", &self.active_requests())
            .finish_non_exhaustive()
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Check if a header is a hop-by-hop header that should not be forwarded.
fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_backend_new() {
        let backend = HttpBackend::new("127.0.0.1:3001".to_string());
        assert_eq!(backend.address(), "127.0.0.1:3001");
        assert!(backend.is_healthy());
        assert!(backend.is_available());
    }

    #[test]
    fn test_http_backend_url() {
        let backend = HttpBackend::new("127.0.0.1:3001".to_string());
        assert_eq!(backend.url("/health"), "http://127.0.0.1:3001/health");
        assert_eq!(backend.url("/run/echo/"), "http://127.0.0.1:3001/run/echo/");
    }

    #[test]
    fn test_http_backend_health_transitions() {
        let backend = HttpBackend::new("127.0.0.1:3001".to_string());

        assert!(backend.is_healthy());
        assert_eq!(backend.failure_count(), 0);
        assert_eq!(backend.success_count(), 0);

        backend.mark_healthy();
        assert!(backend.is_healthy());
        assert_eq!(backend.success_count(), 1);

        backend.mark_unhealthy();
        assert!(!backend.is_healthy());
        assert_eq!(backend.failure_count(), 1);
        assert_eq!(backend.success_count(), 0);
    }

    #[test]
    fn test_http_backend_request_tracking() {
        let backend = HttpBackend::new("127.0.0.1:3001".to_string());

        assert_eq!(backend.active_requests(), 0);

        backend.start_request();
        assert_eq!(backend.active_requests(), 1);

        backend.start_request();
        assert_eq!(backend.active_requests(), 2);

        backend.end_request();
        assert_eq!(backend.active_requests(), 1);

        backend.end_request();
        assert_eq!(backend.active_requests(), 0);
    }

    #[test]
    fn test_http_backend_capacity() {
        let backend = HttpBackend::with_options("127.0.0.1:3001".to_string(), 1, 2);
        assert!(backend.has_capacity());

        backend.start_request();
        assert!(backend.has_capacity());

        backend.start_request();
        assert!(!backend.has_capacity());

        backend.end_request();
        assert!(backend.has_capacity());
    }

    #[test]
    fn test_backend_enum_http() {
        let backend = Backend::http("127.0.0.1:3001");
        assert_eq!(backend.id(), "127.0.0.1:3001");
        assert!(backend.is_healthy());
        assert!(backend.is_available());
        assert!(backend.has_capacity());

        let health = backend.health();
        assert!(health.healthy);
        assert_eq!(health.backend_type, BackendType::Http);
    }

    #[test]
    fn test_hop_by_hop_headers() {
        assert!(is_hop_by_hop_header("connection"));
        assert!(is_hop_by_hop_header("Connection"));
        assert!(is_hop_by_hop_header("keep-alive"));
        assert!(is_hop_by_hop_header("transfer-encoding"));
        assert!(!is_hop_by_hop_header("content-type"));
        assert!(!is_hop_by_hop_header("x-custom-header"));
    }

    #[test]
    fn test_backend_health_struct() {
        let health = BackendHealth {
            backend_id: "test".to_string(),
            healthy: true,
            latency_ms: Some(50),
            last_check: Some(Instant::now()),
            active_requests: 5,
            total_requests: 100,
            backend_type: BackendType::Http,
        };

        assert_eq!(health.backend_id, "test");
        assert!(health.healthy);
        assert_eq!(health.latency_ms, Some(50));
        assert_eq!(health.backend_type, BackendType::Http);
    }
}
