//! L7 Load Balancer for mik runtime.
//!
//! This module provides a high-performance HTTP load balancer that distributes
//! requests across multiple backend workers. It supports:
//!
//! - Round-robin load balancing
//! - Weighted round-robin load balancing (proportional traffic distribution)
//! - Consistent hashing for sticky sessions (path, header, or client IP based)
//! - Health checks with automatic failover
//! - Connection pooling via reqwest
//! - Graceful shutdown with request draining
//!
//! # Architecture
//!
//! ```text
//! [Client] -> [L7 LB :3000] -> [Worker :3001]
//!                           -> [Worker :3002]
//!                           -> [Worker :3003]
//! ```
//!
//! # Example
//!
//! ```ignore
//! use mik::runtime::lb::{LoadBalancer, Backend};
//!
//! let backends = vec![
//!     Backend::new("127.0.0.1:3001"),
//!     Backend::new("127.0.0.1:3002"),
//! ];
//!
//! let lb = LoadBalancer::new(backends);
//! lb.serve("0.0.0.0:3000").await?;
//! ```

mod backend;
mod circuit_breaker;
mod health;
pub mod metrics;
mod proxy;
mod reload;
mod selection;

#[allow(unused_imports)]
pub use backend::{Backend, BackendState};
#[allow(unused_imports)]
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerState};
pub use health::{HealthCheckConfig, HealthCheckType};
#[allow(unused_imports)]
pub use metrics::LbMetrics;
pub use proxy::ProxyService;
#[allow(unused_imports)]
pub use reload::{ReloadConfig, ReloadHandle, ReloadManager, ReloadResult, ReloadSignal};
pub use selection::RoundRobin;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing::info;

use crate::manifest::LbConfig;

/// Default address for the load balancer to listen on.
/// This is a valid socket address constant - parsing cannot fail.
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3000";

/// Configuration for the load balancer.
#[derive(Debug, Clone)]
pub struct LoadBalancerConfig {
    /// Address to listen on.
    pub listen_addr: SocketAddr,
    /// Backend addresses.
    pub backends: Vec<String>,
    /// Health check configuration.
    pub health_check: HealthCheckConfig,
    /// Request timeout.
    pub request_timeout: Duration,
    /// Maximum concurrent requests per backend.
    pub max_connections_per_backend: usize,
    /// Pool idle timeout in seconds (connections idle longer than this are closed).
    pub pool_idle_timeout_secs: u64,
    /// TCP keepalive interval in seconds.
    pub tcp_keepalive_secs: u64,
    /// Use HTTP/2 only (with prior knowledge) for backend connections.
    /// Enable this when all backends support HTTP/2 for better performance.
    pub http2_only: bool,
}

impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            // SAFETY: DEFAULT_LISTEN_ADDR is a compile-time constant with a known-valid format.
            // Parsing "0.0.0.0:3000" cannot fail as it's a valid IPv4 address with port.
            listen_addr: DEFAULT_LISTEN_ADDR
                .parse()
                .expect("DEFAULT_LISTEN_ADDR is a valid socket address"),
            backends: vec![],
            health_check: HealthCheckConfig::default(),
            request_timeout: Duration::from_secs(30),
            max_connections_per_backend: 100,
            pool_idle_timeout_secs: 90,
            tcp_keepalive_secs: 60,
            http2_only: false,
        }
    }
}

impl LoadBalancerConfig {
    /// Create a `LoadBalancerConfig` from a manifest `LbConfig`.
    ///
    /// This converts the manifest configuration (which uses simpler types like
    /// milliseconds as u64) into the runtime configuration (which uses Duration).
    ///
    /// # Arguments
    ///
    /// * `lb_config` - The load balancer configuration from mik.toml
    /// * `listen_addr` - The address to listen on (typically from server config)
    /// * `backends` - List of backend addresses to load balance across
    ///
    /// # Example
    ///
    /// ```ignore
    /// use mik::manifest::LbConfig;
    /// use mik::runtime::lb::LoadBalancerConfig;
    ///
    /// let lb_config = LbConfig::default();
    /// let config = LoadBalancerConfig::from_manifest(
    ///     &lb_config,
    ///     "0.0.0.0:3000".parse().unwrap(),
    ///     vec!["127.0.0.1:3001".to_string()],
    /// );
    /// ```
    #[allow(dead_code)]
    pub fn from_manifest(
        lb_config: &LbConfig,
        listen_addr: SocketAddr,
        backends: Vec<String>,
    ) -> Self {
        // Determine health check type from manifest configuration
        let check_type = match lb_config.health_check_type.to_lowercase().as_str() {
            "tcp" => HealthCheckType::Tcp,
            _ => HealthCheckType::Http {
                path: lb_config.health_check_path.clone(),
            },
        };

        let health_check = HealthCheckConfig {
            interval: Duration::from_millis(lb_config.health_check_interval_ms),
            timeout: Duration::from_millis(lb_config.health_check_timeout_ms),
            check_type,
            unhealthy_threshold: lb_config.unhealthy_threshold,
            healthy_threshold: lb_config.healthy_threshold,
        };

        Self {
            listen_addr,
            backends,
            health_check,
            request_timeout: Duration::from_secs(lb_config.request_timeout_secs),
            max_connections_per_backend: lb_config.max_connections_per_backend,
            pool_idle_timeout_secs: lb_config.pool_idle_timeout_secs,
            tcp_keepalive_secs: lb_config.tcp_keepalive_secs,
            http2_only: lb_config.http2_only,
        }
    }
}

/// L7 Load Balancer.
///
/// Distributes HTTP requests across multiple backend workers using
/// round-robin selection with health-check-based failover.
pub struct LoadBalancer {
    config: LoadBalancerConfig,
    backends: Arc<RwLock<Vec<Backend>>>,
    selection: Arc<RwLock<RoundRobin>>,
    client: reqwest::Client,
}

impl LoadBalancer {
    /// Create a new load balancer with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created (e.g., TLS configuration issues).
    pub fn new(config: LoadBalancerConfig) -> Result<Self> {
        let backends: Vec<Backend> = config
            .backends
            .iter()
            .map(|addr| Backend::new(addr.clone()))
            .collect();

        let selection = RoundRobin::new(backends.len());

        // Create HTTP client with connection pooling and HTTP/2 support
        let mut client_builder = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .pool_max_idle_per_host(config.max_connections_per_backend)
            .pool_idle_timeout(Duration::from_secs(config.pool_idle_timeout_secs))
            .tcp_keepalive(Duration::from_secs(config.tcp_keepalive_secs));

        // Enable HTTP/2 with prior knowledge for local backends
        // This provides better performance through multiplexing when backends support HTTP/2
        if config.http2_only {
            client_builder = client_builder.http2_prior_knowledge();
        }

        let client = client_builder
            .build()
            .context("failed to create HTTP client - check TLS configuration")?;

        Ok(Self {
            config,
            backends: Arc::new(RwLock::new(backends)),
            selection: Arc::new(RwLock::new(selection)),
            client,
        })
    }

    /// Create a load balancer from a list of backend addresses.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created.
    #[allow(dead_code)]
    pub fn from_backends(listen_addr: SocketAddr, backends: Vec<String>) -> Result<Self> {
        let config = LoadBalancerConfig {
            listen_addr,
            backends,
            ..Default::default()
        };
        Self::new(config)
    }

    /// Start the load balancer.
    ///
    /// This will:
    /// 1. Start background health checks
    /// 2. Listen for incoming HTTP requests
    /// 3. Proxy requests to healthy backends
    pub async fn serve(self) -> Result<()> {
        let addr = self.config.listen_addr;
        let backends = self.backends.clone();
        let health_config = self.config.health_check.clone();

        // Start health check background task
        let health_backends = backends.clone();
        tokio::spawn(async move {
            health::run_health_checks(health_backends, health_config).await;
        });

        info!("L7 Load Balancer listening on http://{}", addr);

        // Log backends
        {
            let backends = backends.read().await;
            for (i, backend) in backends.iter().enumerate() {
                info!("  Backend {}: {}", i + 1, backend.address());
            }
        }

        // Create and run the proxy service
        let proxy = ProxyService::with_http2(
            backends,
            self.selection,
            self.client,
            self.config.request_timeout,
            self.config.http2_only,
        );

        proxy.serve(addr).await
    }

    /// Get the number of healthy backends.
    #[allow(dead_code)]
    pub async fn healthy_count(&self) -> usize {
        let backends = self.backends.read().await;
        backends.iter().filter(|b| b.is_healthy()).count()
    }

    /// Get the total number of backends.
    #[allow(dead_code)]
    pub async fn total_count(&self) -> usize {
        self.backends.read().await.len()
    }

    /// Get the shared backends reference for use with ReloadManager.
    ///
    /// This allows creating a ReloadManager that can dynamically update
    /// the backend list without restarting the load balancer.
    #[allow(dead_code)]
    pub fn backends(&self) -> Arc<RwLock<Vec<Backend>>> {
        self.backends.clone()
    }

    /// Get the shared selection reference for use with ReloadManager.
    #[allow(dead_code)]
    pub fn selection(&self) -> Arc<RwLock<RoundRobin>> {
        self.selection.clone()
    }

    /// Create a ReloadManager for this load balancer.
    ///
    /// The ReloadManager allows dynamically updating the backend list
    /// with graceful draining of removed backends.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use mik::runtime::lb::{LoadBalancer, ReloadConfig};
    /// use std::time::Duration;
    ///
    /// let lb = LoadBalancer::from_backends(
    ///     "0.0.0.0:3000".parse().unwrap(),
    ///     vec!["127.0.0.1:3001".to_string()],
    /// );
    ///
    /// let reload_config = ReloadConfig {
    ///     drain_timeout: Duration::from_secs(30),
    /// };
    ///
    /// let manager = lb.reload_manager(reload_config);
    ///
    /// // Add a new backend dynamically
    /// manager.add_backend("127.0.0.1:3002".to_string()).await;
    ///
    /// // Remove a backend with graceful draining
    /// manager.remove_backend("127.0.0.1:3001").await;
    /// ```
    #[allow(dead_code)]
    pub fn reload_manager(&self, config: ReloadConfig) -> ReloadManager {
        ReloadManager::new(config, self.backends.clone(), self.selection.clone())
    }

    /// Update the backend list atomically.
    ///
    /// This is a simpler alternative to using ReloadManager when you
    /// don't need graceful draining. New backends are added immediately
    /// and removed backends are removed immediately.
    ///
    /// For graceful draining, use `reload_manager()` instead.
    #[allow(dead_code)]
    pub async fn update_backends(&self, new_backends: Vec<String>) {
        let backends: Vec<Backend> = new_backends
            .iter()
            .map(|addr| Backend::new(addr.clone()))
            .collect();

        let mut backends_write = self.backends.write().await;
        let mut selection_write = self.selection.write().await;

        *selection_write = RoundRobin::new(backends.len());
        *backends_write = backends;

        info!(count = backends_write.len(), "Backend list updated");
    }

    /// Drain a specific backend by marking it unhealthy.
    ///
    /// The backend will stop receiving new requests but will continue
    /// to serve existing connections until they complete.
    ///
    /// Returns true if the backend was found and marked for draining.
    #[allow(dead_code)]
    pub async fn drain_backend(&self, address: &str) -> bool {
        let backends = self.backends.read().await;
        for backend in backends.iter() {
            if backend.address() == address {
                backend.mark_unhealthy();
                info!(address = %address, "Backend marked for draining");
                return true;
            }
        }
        false
    }

    /// Get the number of active requests on a specific backend.
    #[allow(dead_code)]
    pub async fn backend_active_requests(&self, address: &str) -> Option<u64> {
        let backends = self.backends.read().await;
        backends
            .iter()
            .find(|b| b.address() == address)
            .map(backend::Backend::active_requests)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_balancer_config_default() {
        let config = LoadBalancerConfig::default();
        assert_eq!(config.listen_addr, "0.0.0.0:3000".parse().unwrap());
        assert!(config.backends.is_empty());
        assert_eq!(config.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_load_balancer_from_backends() {
        let lb = LoadBalancer::from_backends(
            "127.0.0.1:8080".parse().unwrap(),
            vec!["127.0.0.1:3001".to_string(), "127.0.0.1:3002".to_string()],
        )
        .unwrap();
        assert_eq!(lb.config.listen_addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(lb.config.backends.len(), 2);
    }

    #[test]
    fn test_load_balancer_config_from_manifest() {
        let lb_config = LbConfig {
            enabled: true,
            algorithm: "weighted".to_string(),
            health_check_type: "http".to_string(),
            health_check_interval_ms: 10000,
            health_check_timeout_ms: 3000,
            health_check_path: "/healthz".to_string(),
            unhealthy_threshold: 5,
            healthy_threshold: 3,
            request_timeout_secs: 60,
            max_connections_per_backend: 200,
            pool_idle_timeout_secs: 120,
            tcp_keepalive_secs: 30,
            http2_only: true,
        };

        let config = LoadBalancerConfig::from_manifest(
            &lb_config,
            "0.0.0.0:8080".parse().unwrap(),
            vec!["127.0.0.1:3001".to_string(), "127.0.0.1:3002".to_string()],
        );

        assert_eq!(config.listen_addr, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(config.backends.len(), 2);
        assert_eq!(config.request_timeout, Duration::from_secs(60));
        assert_eq!(config.max_connections_per_backend, 200);
        assert_eq!(config.pool_idle_timeout_secs, 120);
        assert_eq!(config.tcp_keepalive_secs, 30);
        assert!(config.http2_only);

        // Check health check config
        assert_eq!(config.health_check.interval, Duration::from_millis(10000));
        assert_eq!(config.health_check.timeout, Duration::from_millis(3000));
        assert_eq!(config.health_check.path(), "/healthz");
        assert_eq!(config.health_check.unhealthy_threshold, 5);
        assert_eq!(config.health_check.healthy_threshold, 3);
    }

    #[test]
    fn test_load_balancer_config_from_manifest_defaults() {
        let lb_config = LbConfig::default();

        let config =
            LoadBalancerConfig::from_manifest(&lb_config, "0.0.0.0:3000".parse().unwrap(), vec![]);

        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert_eq!(config.max_connections_per_backend, 100);
        assert_eq!(config.pool_idle_timeout_secs, 90);
        assert_eq!(config.tcp_keepalive_secs, 60);
        assert!(!config.http2_only);
        assert_eq!(config.health_check.interval, Duration::from_millis(5000));
        assert_eq!(config.health_check.timeout, Duration::from_millis(2000));
        assert_eq!(config.health_check.path(), "/health");
        assert_eq!(config.health_check.unhealthy_threshold, 3);
        assert_eq!(config.health_check.healthy_threshold, 2);
        // Default should be HTTP health check
        assert_eq!(
            config.health_check.check_type,
            HealthCheckType::Http {
                path: "/health".to_string()
            }
        );
    }

    #[test]
    fn test_load_balancer_config_from_manifest_tcp_health_check() {
        let lb_config = LbConfig {
            enabled: true,
            algorithm: "round_robin".to_string(),
            health_check_type: "tcp".to_string(),
            health_check_interval_ms: 5000,
            health_check_timeout_ms: 2000,
            health_check_path: "/health".to_string(), // Should be ignored for TCP
            unhealthy_threshold: 3,
            healthy_threshold: 2,
            request_timeout_secs: 30,
            max_connections_per_backend: 100,
            pool_idle_timeout_secs: 90,
            tcp_keepalive_secs: 60,
            http2_only: false,
        };

        let config = LoadBalancerConfig::from_manifest(
            &lb_config,
            "0.0.0.0:3000".parse().unwrap(),
            vec!["127.0.0.1:3001".to_string()],
        );

        // TCP health check should be selected
        assert_eq!(config.health_check.check_type, HealthCheckType::Tcp);
        // path() should return empty string for TCP
        assert_eq!(config.health_check.path(), "");
    }

    #[test]
    fn test_load_balancer_config_from_manifest_tcp_case_insensitive() {
        // Test that "TCP", "Tcp", "tcp" all work
        for health_check_type in ["TCP", "Tcp", "tcp", "  TCP  "] {
            let lb_config = LbConfig {
                health_check_type: health_check_type.trim().to_string(),
                ..LbConfig::default()
            };

            let config = LoadBalancerConfig::from_manifest(
                &lb_config,
                "0.0.0.0:3000".parse().unwrap(),
                vec![],
            );

            assert_eq!(
                config.health_check.check_type,
                HealthCheckType::Tcp,
                "Failed for health_check_type: {health_check_type}"
            );
        }
    }
}
