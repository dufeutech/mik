//! Health check implementation for backend servers.
//!
//! Provides periodic health monitoring of backend servers to ensure requests
//! are only routed to healthy instances. Supports both HTTP and TCP health checks.
//!
//! # Key Types
//!
//! - [`HealthCheckConfig`] - Configuration for health check behavior (interval, timeout, thresholds)
//! - [`HealthCheckType`] - Enum for HTTP (path-based) or TCP (connection-based) checks
//!
//! Health checks run in a background task and automatically update backend health status.

use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::Backend;
use super::metrics::LbMetrics;

/// Type of health check to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthCheckType {
    /// HTTP health check - sends GET request to specified path.
    Http {
        /// Path to check (e.g., "/health").
        path: String,
    },
    /// TCP health check - just verifies the port is accepting connections.
    Tcp,
}

impl Default for HealthCheckType {
    fn default() -> Self {
        Self::Http {
            path: "/health".to_string(),
        }
    }
}

/// Configuration for health checks.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Interval between health checks.
    pub interval: Duration,
    /// Timeout for each health check request.
    pub timeout: Duration,
    /// Type of health check to perform.
    pub check_type: HealthCheckType,
    /// Number of consecutive failures before marking unhealthy.
    pub unhealthy_threshold: u32,
    /// Number of consecutive successes before marking healthy.
    pub healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_millis(2000),
            check_type: HealthCheckType::default(),
            unhealthy_threshold: 3,
            healthy_threshold: 2,
        }
    }
}

impl HealthCheckConfig {
    /// Create a new HTTP health check configuration.
    #[allow(dead_code)]
    pub fn http(path: impl Into<String>) -> Self {
        Self {
            check_type: HealthCheckType::Http { path: path.into() },
            ..Default::default()
        }
    }

    /// Create a new TCP health check configuration.
    #[allow(dead_code)]
    pub fn tcp() -> Self {
        Self {
            check_type: HealthCheckType::Tcp,
            ..Default::default()
        }
    }

    /// Get the path for HTTP health checks (for backwards compatibility).
    #[allow(dead_code)]
    pub fn path(&self) -> &str {
        match &self.check_type {
            HealthCheckType::Http { path } => path,
            HealthCheckType::Tcp => "",
        }
    }
}

/// Health check service for monitoring backend health.
pub(super) struct HealthCheck {
    config: HealthCheckConfig,
    /// HTTP client for HTTP health checks (only created when needed).
    client: Option<reqwest::Client>,
}

impl HealthCheck {
    /// Create a new health check service.
    pub(super) fn new(config: HealthCheckConfig) -> Self {
        // Only create HTTP client if we're doing HTTP health checks
        let client = match &config.check_type {
            HealthCheckType::Http { .. } => Some(
                reqwest::Client::builder()
                    .timeout(config.timeout)
                    .pool_max_idle_per_host(1)
                    .build()
                    .expect("failed to create HTTP client - check TLS configuration"),
            ),
            HealthCheckType::Tcp => None,
        };

        Self { config, client }
    }

    /// Check the health of a single backend.
    pub(super) async fn check(&self, backend: &Backend) -> bool {
        match &self.config.check_type {
            HealthCheckType::Http { path } => self.check_http(backend, path).await,
            HealthCheckType::Tcp => self.check_tcp(backend).await,
        }
    }

    /// Perform an HTTP health check.
    async fn check_http(&self, backend: &Backend, path: &str) -> bool {
        let client = self
            .client
            .as_ref()
            .expect("HTTP client should exist for HTTP health checks");
        let url = backend.url(path);

        match client.get(&url).send().await {
            Ok(response) => {
                let healthy = response.status().is_success();
                if healthy {
                    debug!(backend = %backend.address(), "HTTP health check passed");
                } else {
                    debug!(
                        backend = %backend.address(),
                        status = %response.status(),
                        "HTTP health check failed with status"
                    );
                }
                healthy
            },
            Err(e) => {
                debug!(
                    backend = %backend.address(),
                    error = %e,
                    "HTTP health check failed"
                );
                false
            },
        }
    }

    /// Perform a TCP health check by attempting to connect to the backend.
    async fn check_tcp(&self, backend: &Backend) -> bool {
        let address = backend.address();

        // Resolve the address to a SocketAddr
        let socket_addr = match address.to_socket_addrs() {
            Ok(mut addrs) => {
                if let Some(addr) = addrs.next() {
                    addr
                } else {
                    debug!(
                        backend = %address,
                        "TCP health check failed: no addresses resolved"
                    );
                    return false;
                }
            },
            Err(e) => {
                debug!(
                    backend = %address,
                    error = %e,
                    "TCP health check failed: address resolution error"
                );
                return false;
            },
        };

        // Attempt TCP connection with timeout
        match timeout(self.config.timeout, TcpStream::connect(socket_addr)).await {
            Ok(Ok(_stream)) => {
                debug!(backend = %address, "TCP health check passed");
                true
            },
            Ok(Err(e)) => {
                debug!(
                    backend = %address,
                    error = %e,
                    "TCP health check failed: connection error"
                );
                false
            },
            Err(_) => {
                debug!(
                    backend = %address,
                    timeout = ?self.config.timeout,
                    "TCP health check failed: connection timeout"
                );
                false
            },
        }
    }
}

/// Run continuous health checks for all backends.
pub(super) async fn run_health_checks(
    backends: Arc<RwLock<Vec<Backend>>>,
    config: HealthCheckConfig,
) {
    let health_check = HealthCheck::new(config.clone());
    let metrics = LbMetrics::new();
    let mut interval = tokio::time::interval(config.interval);

    match &config.check_type {
        HealthCheckType::Http { path } => {
            info!(
                interval = ?config.interval,
                path = %path,
                check_type = "HTTP",
                "Starting health check loop"
            );
        },
        HealthCheckType::Tcp => {
            info!(
                interval = ?config.interval,
                check_type = "TCP",
                "Starting health check loop"
            );
        },
    }

    loop {
        interval.tick().await;

        let backends_snapshot = {
            let backends = backends.read().await;
            backends.clone()
        };

        for (i, backend) in backends_snapshot.iter().enumerate() {
            let is_healthy = health_check.check(backend).await;
            let was_healthy = backend.is_healthy();

            // Apply threshold logic
            if is_healthy {
                backend.mark_healthy();
                if backend.success_count() >= u64::from(config.healthy_threshold) && !was_healthy {
                    info!(
                        backend = %backend.address(),
                        index = i,
                        "Backend recovered and marked healthy"
                    );
                }
            } else {
                backend.mark_unhealthy();
                if backend.failure_count() >= u64::from(config.unhealthy_threshold) && was_healthy {
                    warn!(
                        backend = %backend.address(),
                        index = i,
                        failures = backend.failure_count(),
                        "Backend marked unhealthy after consecutive failures"
                    );
                }
            }
        }

        // Update the shared backends with new health state
        {
            let backends_write = backends.write().await;
            for (i, backend) in backends_snapshot.iter().enumerate() {
                if i < backends_write.len() {
                    // Copy health state to shared backends
                    if backend.is_healthy() {
                        backends_write[i].mark_healthy();
                    } else {
                        backends_write[i].mark_unhealthy();
                    }
                }
            }
        }

        // Update backend health metrics after each health check cycle
        metrics.update_backend_metrics(
            backends_snapshot
                .iter()
                .map(|b| (b.address(), b.is_healthy(), b.active_requests())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn test_health_check_config_default() {
        let config = HealthCheckConfig::default();
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.timeout, Duration::from_millis(2000));
        assert_eq!(
            config.check_type,
            HealthCheckType::Http {
                path: "/health".to_string()
            }
        );
        assert_eq!(config.path(), "/health");
        assert_eq!(config.unhealthy_threshold, 3);
        assert_eq!(config.healthy_threshold, 2);
    }

    #[test]
    fn test_health_check_config_http() {
        let config = HealthCheckConfig::http("/healthz");
        assert_eq!(
            config.check_type,
            HealthCheckType::Http {
                path: "/healthz".to_string()
            }
        );
        assert_eq!(config.path(), "/healthz");
    }

    #[test]
    fn test_health_check_config_tcp() {
        let config = HealthCheckConfig::tcp();
        assert_eq!(config.check_type, HealthCheckType::Tcp);
        assert_eq!(config.path(), "");
    }

    #[test]
    fn test_health_check_config_custom() {
        let config = HealthCheckConfig {
            interval: Duration::from_secs(1),
            timeout: Duration::from_millis(500),
            check_type: HealthCheckType::Http {
                path: "/healthz".to_string(),
            },
            unhealthy_threshold: 5,
            healthy_threshold: 3,
        };

        assert_eq!(config.interval, Duration::from_secs(1));
        assert_eq!(config.path(), "/healthz");
    }

    #[test]
    fn test_health_check_type_default() {
        let check_type = HealthCheckType::default();
        assert_eq!(
            check_type,
            HealthCheckType::Http {
                path: "/health".to_string()
            }
        );
    }

    #[test]
    fn test_health_check_creates_client_for_http() {
        let config = HealthCheckConfig::http("/health");
        let health_check = HealthCheck::new(config);
        assert!(health_check.client.is_some());
    }

    #[test]
    fn test_health_check_no_client_for_tcp() {
        let config = HealthCheckConfig::tcp();
        let health_check = HealthCheck::new(config);
        assert!(health_check.client.is_none());
    }

    #[tokio::test]
    async fn test_tcp_health_check_passes_when_port_open() {
        // Start a TCP listener on a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Create backend and health check
        let backend = Backend::new(addr.to_string());
        let config = HealthCheckConfig {
            timeout: Duration::from_millis(100),
            ..HealthCheckConfig::tcp()
        };
        let health_check = HealthCheck::new(config);

        // Health check should pass
        let result = health_check.check(&backend).await;
        assert!(
            result,
            "TCP health check should pass when port is accepting connections"
        );
    }

    #[tokio::test]
    async fn test_tcp_health_check_fails_when_port_closed() {
        // Use a port that's likely not in use
        // We bind to a port and then drop it to get a "free" port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // Close the listener so the port is closed

        // Create backend and health check
        let backend = Backend::new(addr.to_string());
        let config = HealthCheckConfig {
            timeout: Duration::from_millis(100),
            ..HealthCheckConfig::tcp()
        };
        let health_check = HealthCheck::new(config);

        // Health check should fail
        let result = health_check.check(&backend).await;
        assert!(!result, "TCP health check should fail when port is closed");
    }

    #[tokio::test]
    async fn test_tcp_health_check_fails_on_invalid_address() {
        // Use an invalid address
        let backend = Backend::new("invalid-host:12345".to_string());
        let config = HealthCheckConfig {
            timeout: Duration::from_millis(100),
            ..HealthCheckConfig::tcp()
        };
        let health_check = HealthCheck::new(config);

        // Health check should fail
        let result = health_check.check(&backend).await;
        assert!(!result, "TCP health check should fail on invalid address");
    }

    #[tokio::test]
    async fn test_tcp_health_check_respects_timeout() {
        // Use a non-routable address to trigger timeout
        // 10.255.255.1 is typically non-routable and will cause a timeout
        let backend = Backend::new("10.255.255.1:12345".to_string());
        let config = HealthCheckConfig {
            timeout: Duration::from_millis(50), // Very short timeout
            ..HealthCheckConfig::tcp()
        };
        let health_check = HealthCheck::new(config);

        let start = std::time::Instant::now();
        let result = health_check.check(&backend).await;
        let elapsed = start.elapsed();

        assert!(!result, "TCP health check should fail on timeout");
        // Allow some margin for the timeout check
        assert!(
            elapsed < Duration::from_millis(200),
            "TCP health check should respect timeout (elapsed: {elapsed:?})"
        );
    }
}
