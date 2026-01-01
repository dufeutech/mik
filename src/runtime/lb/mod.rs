// Allow dead code and unused imports for library-first API
// LB types are for external consumers, not yet integrated with CLI
#![allow(dead_code, unused_imports)]

//! L7 Load Balancer for mik runtime.
//!
//! This module provides an HTTP load balancer that distributes requests across
//! multiple backend workers using various selection strategies with health checks.
//!
//! # Architecture
//!
//! The load balancer is designed for both CLI use and embedding:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      LoadBalancer (server.rs)                   │
//! │  - Network binding and HTTP serving                             │
//! │  - CLI-facing API                                               │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                         Proxy (proxy.rs)                        │
//! │  - Core request forwarding logic                                │
//! │  - Backend selection (round-robin, weighted, etc.)              │
//! │  - Health checking coordination                                 │
//! │  - NO network binding (embeddable)                              │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                      Backend (backend.rs)                       │
//! │  - Backend::Http - network backends                             │
//! │  - Backend::Runtime - embedded runtime backends                 │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ## CLI Use (with network binding)
//!
//! ```ignore
//! use mik::runtime::lb::LoadBalancer;
//!
//! let lb = LoadBalancer::builder()
//!     .listen("0.0.0.0:8080".parse()?)
//!     .backend("127.0.0.1:3001")
//!     .backend("127.0.0.1:3002")
//!     .build()?;
//!
//! lb.serve().await?;
//! ```
//!
//! ## Embedded Use (no network binding)
//!
//! ```ignore
//! use mik::runtime::lb::{Proxy, Backend};
//!
//! let proxy = Proxy::builder()
//!     .backend(Backend::http("127.0.0.1:3001"))
//!     .backend(Backend::runtime(my_runtime))
//!     .build()?;
//!
//! // Forward requests programmatically
//! let response = proxy.forward(request).await?;
//! ```

pub mod backend;
mod circuit_breaker;
pub mod health;
pub mod metrics;
pub mod proxy;
pub mod selection;
pub mod server;

// Re-export main types for convenience
pub use backend::{
    Backend, BackendHealth, BackendType, HttpBackend, RuntimeBackend, RuntimeHandler,
};
pub use health::{HealthCheckConfig, HealthCheckType, HealthChecker};
pub use proxy::{Proxy, ProxyBuilder, Request, Response};
pub use selection::{LoadBalanceStrategy, RoundRobin, Selection};
pub use server::{LoadBalancer, LoadBalancerBuilder};

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;

use crate::manifest::LbConfig;

/// Default address for the load balancer to listen on.
/// This is a valid socket address constant - parsing cannot fail.
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3000";

/// Configuration for the load balancer.
///
/// This is the legacy configuration struct for backward compatibility.
/// For new code, prefer using `LoadBalancerBuilder` directly.
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

    /// Convert this config into a `LoadBalancerBuilder`.
    pub fn into_builder(self) -> LoadBalancerBuilder {
        let mut builder = LoadBalancer::builder()
            .listen(self.listen_addr)
            .health_check(Some(self.health_check))
            .request_timeout(self.request_timeout)
            .max_connections_per_backend(self.max_connections_per_backend)
            .pool_idle_timeout(Duration::from_secs(self.pool_idle_timeout_secs))
            .tcp_keepalive(Duration::from_secs(self.tcp_keepalive_secs))
            .http2_only(self.http2_only);

        for backend in self.backends {
            builder = builder.backend(backend);
        }

        builder
    }

    /// Build and serve the load balancer.
    ///
    /// This is a convenience method that creates a `LoadBalancer` from this config
    /// and immediately starts serving.
    ///
    /// # Errors
    ///
    /// Returns an error if the load balancer cannot be created or if serving fails.
    pub async fn serve(self) -> Result<()> {
        let lb = self.into_builder().build()?;
        lb.serve().await
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
    fn test_load_balancer_config_from_manifest() {
        let lb_config = LbConfig {
            enabled: true,
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
            health_check_type: "tcp".to_string(),
            ..LbConfig::default()
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

    #[test]
    fn test_config_into_builder() {
        let config = LoadBalancerConfig {
            listen_addr: "127.0.0.1:8080".parse().unwrap(),
            backends: vec!["127.0.0.1:3001".to_string(), "127.0.0.1:3002".to_string()],
            ..Default::default()
        };

        let lb = config.into_builder().build();
        assert!(lb.is_ok());
    }
}
