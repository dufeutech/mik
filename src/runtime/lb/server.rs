//! Network serving logic for the load balancer.
//!
//! This module provides the [`LoadBalancer`] struct which wraps [`Proxy`](super::proxy::Proxy)
//! and adds network binding capabilities. This is the CLI-facing API for running a
//! load balancer as a network service.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      LoadBalancer (this module)                 │
//! │  - Wraps Proxy                                                  │
//! │  - Binds to network address                                     │
//! │  - Handles TCP/HTTP connections                                 │
//! │  - Manages health check background task                         │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                      Proxy (proxy module)                       │
//! │  - Pure request forwarding logic                                │
//! │  - Backend selection                                            │
//! │  - Health checking coordination                                 │
//! │  - NO network binding                                           │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use mik::runtime::lb::{LoadBalancer, LoadBalancerBuilder};
//!
//! // Build and run a load balancer
//! let lb = LoadBalancer::builder()
//!     .listen("0.0.0.0:8080".parse()?)
//!     .backend("127.0.0.1:3001")
//!     .backend("127.0.0.1:3002")
//!     .health_check_path("/health")
//!     .build()?;
//!
//! // Serve until shutdown
//! lb.serve().await?;
//! ```

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tracing::{debug, error, info};

use super::backend::{Backend, HttpBackend};
use super::health::HealthCheckConfig;
use super::proxy::{Proxy, ProxyBuilder, Request, Response};
use super::selection::LoadBalanceStrategy;

/// Default address for the load balancer to listen on.
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3000";

/// L7 Load Balancer with network serving capabilities.
///
/// Wraps a [`Proxy`] and adds network binding. Use [`LoadBalancer::builder()`]
/// to create instances, or wrap an existing `Proxy` with [`LoadBalancer::new()`].
#[derive(Debug)]
pub struct LoadBalancer {
    /// The underlying proxy for request forwarding.
    proxy: Arc<Proxy>,
    /// Address to listen on.
    addr: SocketAddr,
    /// Whether to use HTTP/2 for incoming connections.
    http2_only: bool,
}

impl LoadBalancer {
    /// Create a new load balancer by wrapping an existing proxy.
    ///
    /// This is useful when you've already configured a [`Proxy`] and want to
    /// serve it over the network.
    pub fn new(proxy: Proxy, addr: SocketAddr) -> Self {
        Self {
            proxy: Arc::new(proxy),
            addr,
            http2_only: false,
        }
    }

    /// Create a new load balancer with HTTP/2 mode.
    pub fn with_http2(proxy: Proxy, addr: SocketAddr, http2_only: bool) -> Self {
        Self {
            proxy: Arc::new(proxy),
            addr,
            http2_only,
        }
    }

    /// Create a new [`LoadBalancerBuilder`].
    pub fn builder() -> LoadBalancerBuilder {
        LoadBalancerBuilder::new()
    }

    /// Get a reference to the underlying proxy.
    pub fn proxy(&self) -> &Proxy {
        &self.proxy
    }

    /// Get the listen address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Start serving incoming connections.
    ///
    /// This will:
    /// 1. Start background health checks for all backends
    /// 2. Listen for incoming HTTP connections
    /// 3. Forward requests to healthy backends via the proxy
    ///
    /// This method runs until an error occurs or the process is terminated.
    pub async fn serve(self) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        let protocol = if self.http2_only { "h2" } else { "http/1.1" };
        info!(
            "L7 Load Balancer listening on http://{} ({})",
            self.addr, protocol
        );

        // Log backends
        let backends = self.proxy.backends().await;
        for (i, backend) in backends.iter().enumerate() {
            info!("  Backend {}: {}", i + 1, backend.id());
        }

        // Start health check background task
        let _health_handle = self.proxy.start_health_checks();

        let proxy = self.proxy.clone();
        let http2_only = self.http2_only;

        loop {
            let (stream, remote_addr) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let proxy = proxy.clone();

            tokio::spawn(async move {
                let service = service_fn(move |req: HyperRequest<Incoming>| {
                    let proxy = proxy.clone();
                    async move { handle_request(proxy, req, remote_addr).await }
                });

                let result = if http2_only {
                    http2::Builder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await
                } else {
                    http1::Builder::new().serve_connection(io, service).await
                };

                if let Err(e) = result
                    && !e.to_string().contains("connection closed")
                {
                    error!("Connection error: {}", e);
                }
            });
        }
    }
}

/// Handle a single HTTP request by forwarding it via the proxy.
async fn handle_request(
    proxy: Arc<Proxy>,
    req: HyperRequest<Incoming>,
    remote_addr: SocketAddr,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    use http_body_util::BodyExt;

    let method = req.method().to_string();
    let uri = req.uri().clone();
    let path = uri
        .path_and_query()
        .map_or("/", hyper::http::uri::PathAndQuery::as_str)
        .to_string();

    debug!(
        method = %method,
        path = %path,
        remote = %remote_addr,
        "Received request"
    );

    // Convert hyper Request to our Request type
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    // Collect body
    let body = match req.collect().await {
        Ok(collected) => collected.to_bytes().to_vec(),
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return Ok(HyperResponse::builder()
                .status(400)
                .body(Full::new(Bytes::from(format!("Failed to read body: {e}"))))
                .expect("valid error response"));
        },
    };

    let request = Request {
        method,
        path,
        headers,
        body,
    };

    // Forward via proxy
    let response = match proxy.forward(request).await {
        Ok(resp) => resp,
        Err(e) => {
            error!("Proxy error: {}", e);
            Response {
                status: 502,
                headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
                body: format!("Proxy error: {e}").into_bytes(),
            }
        },
    };

    // Convert our Response to hyper Response
    let mut builder = HyperResponse::builder().status(response.status);

    for (name, value) in &response.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    Ok(builder
        .body(Full::new(Bytes::from(response.body)))
        .expect("valid response"))
}

/// Builder for [`LoadBalancer`].
///
/// Provides a convenient fluent API for configuring and creating a load balancer.
/// This is the primary way to create load balancers for CLI use.
///
/// # Example
///
/// ```ignore
/// let lb = LoadBalancer::builder()
///     .listen("0.0.0.0:8080".parse()?)
///     .backend("127.0.0.1:3001")
///     .backend("127.0.0.1:3002")
///     .strategy(LoadBalanceStrategy::RoundRobin)
///     .health_check_path("/health")
///     .request_timeout(Duration::from_secs(60))
///     .build()?;
/// ```
pub struct LoadBalancerBuilder {
    /// Listen address.
    addr: SocketAddr,
    /// Backend addresses.
    backends: Vec<String>,
    /// Load balancing strategy.
    strategy: LoadBalanceStrategy,
    /// Health check configuration.
    health_check: Option<HealthCheckConfig>,
    /// Request timeout.
    request_timeout: Duration,
    /// Maximum connections per backend.
    max_connections_per_backend: usize,
    /// Connection pool idle timeout.
    pool_idle_timeout: Duration,
    /// TCP keepalive interval.
    tcp_keepalive: Duration,
    /// Use HTTP/2 only for backend connections.
    http2_only: bool,
}

impl Default for LoadBalancerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancerBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            addr: DEFAULT_LISTEN_ADDR
                .parse()
                .expect("DEFAULT_LISTEN_ADDR is valid"),
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

    /// Set the address to listen on.
    #[must_use]
    pub fn listen(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }

    /// Add a backend by address string (e.g., "127.0.0.1:3001").
    #[must_use]
    pub fn backend(mut self, address: impl Into<String>) -> Self {
        self.backends.push(address.into());
        self
    }

    /// Add multiple backends.
    #[must_use]
    pub fn backends(mut self, addresses: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.backends.extend(addresses.into_iter().map(Into::into));
        self
    }

    /// Set the load balancing strategy.
    #[must_use]
    pub fn strategy(mut self, strategy: LoadBalanceStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Set the health check path for HTTP health checks.
    #[must_use]
    pub fn health_check_path(mut self, path: impl Into<String>) -> Self {
        self.health_check = Some(HealthCheckConfig::http(path));
        self
    }

    /// Set the full health check configuration.
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

    /// Set the maximum connections per backend.
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
    #[must_use]
    pub fn http2_only(mut self, enabled: bool) -> Self {
        self.http2_only = enabled;
        self
    }

    /// Build the [`LoadBalancer`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No backends were configured
    /// - The HTTP client cannot be created
    pub fn build(self) -> Result<LoadBalancer> {
        if self.backends.is_empty() {
            anyhow::bail!("At least one backend must be configured");
        }

        // Convert string addresses to Backend enums
        let backends: Vec<Backend> = self
            .backends
            .into_iter()
            .map(|addr| Backend::Http(HttpBackend::new(addr)))
            .collect();

        // Build the underlying proxy
        let mut proxy_builder = ProxyBuilder::new()
            .strategy(self.strategy)
            .health_check(self.health_check)
            .request_timeout(self.request_timeout)
            .max_connections_per_backend(self.max_connections_per_backend)
            .pool_idle_timeout(self.pool_idle_timeout)
            .tcp_keepalive(self.tcp_keepalive)
            .http2_only(self.http2_only);

        for backend in backends {
            proxy_builder = proxy_builder.backend(backend);
        }

        let proxy = proxy_builder.build().context("Failed to build proxy")?;

        Ok(LoadBalancer::with_http2(proxy, self.addr, self.http2_only))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_balancer_builder_requires_backends() {
        let result = LoadBalancer::builder().build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("backend"));
    }

    #[test]
    fn test_load_balancer_builder_with_backends() {
        let result = LoadBalancer::builder()
            .backend("127.0.0.1:3001")
            .backend("127.0.0.1:3002")
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_load_balancer_builder_configuration() {
        let result = LoadBalancer::builder()
            .listen("127.0.0.1:8080".parse().unwrap())
            .backend("127.0.0.1:3001")
            .strategy(LoadBalanceStrategy::RoundRobin)
            .health_check_path("/healthz")
            .request_timeout(Duration::from_secs(60))
            .max_connections_per_backend(200)
            .pool_idle_timeout(Duration::from_secs(120))
            .tcp_keepalive(Duration::from_secs(30))
            .http2_only(true)
            .build();
        assert!(result.is_ok());

        let lb = result.unwrap();
        assert_eq!(lb.addr(), "127.0.0.1:8080".parse().unwrap());
    }

    #[test]
    fn test_load_balancer_builder_multiple_backends() {
        let result = LoadBalancer::builder()
            .backends(["127.0.0.1:3001", "127.0.0.1:3002", "127.0.0.1:3003"])
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_load_balancer_new() {
        let proxy = Proxy::builder()
            .http_backend("127.0.0.1:3001")
            .build()
            .unwrap();

        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let lb = LoadBalancer::new(proxy, addr);

        assert_eq!(lb.addr(), addr);
    }

    #[test]
    fn test_load_balancer_default_address() {
        let result = LoadBalancer::builder().backend("127.0.0.1:3001").build();
        assert!(result.is_ok());

        let lb = result.unwrap();
        assert_eq!(lb.addr(), "0.0.0.0:3000".parse().unwrap());
    }
}
