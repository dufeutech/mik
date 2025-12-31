//! Prometheus metrics for the L7 Load Balancer.
//!
//! Provides observability for the load balancer through Prometheus-compatible metrics.
//! These metrics integrate with the existing mik metrics infrastructure.
//!
//! # Metrics Exposed
//!
//! ## Request Metrics
//! - `mik_lb_requests_total` - Total LB requests (labels: backend, status)
//! - `mik_lb_request_duration_seconds` - Request duration histogram (labels: backend)
//!
//! ## Backend Metrics
//! - `mik_lb_backends_healthy` - Number of healthy backends
//! - `mik_lb_backends_total` - Total number of backends
//! - `mik_lb_active_connections` - Active connections per backend (labels: backend)

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use std::sync::OnceLock;

/// Global flag to track if LB metrics have been registered.
static LB_METRICS_REGISTERED: OnceLock<()> = OnceLock::new();

/// Registers all LB metric descriptions.
///
/// This function is idempotent - calling it multiple times has no effect
/// after the first successful registration.
pub fn register_lb_metrics() {
    LB_METRICS_REGISTERED.get_or_init(|| {
        // Request metrics
        describe_counter!(
            "mik_lb_requests_total",
            "Total number of load balancer requests"
        );
        describe_histogram!(
            "mik_lb_request_duration_seconds",
            "Load balancer request duration in seconds"
        );

        // Backend metrics
        describe_gauge!("mik_lb_backends_healthy", "Number of healthy backends");
        describe_gauge!("mik_lb_backends_total", "Total number of backends");
        describe_gauge!(
            "mik_lb_active_connections",
            "Active connections per backend"
        );
    });
}

/// Load balancer metrics collector.
///
/// Provides methods to record metrics for the L7 load balancer.
/// Uses the global metrics registry, so metrics are automatically
/// available at the `/metrics` endpoint.
///
/// # Example
///
/// ```ignore
/// use mik::runtime::lb::metrics::LbMetrics;
/// use std::time::Instant;
///
/// let metrics = LbMetrics::new();
///
/// // Record a successful request
/// let start = Instant::now();
/// // ... handle request ...
/// let duration = start.elapsed();
/// metrics.record_request("127.0.0.1:3001", "success", duration.as_secs_f64());
///
/// // Update backend health
/// metrics.set_backends_healthy(2);
/// metrics.set_backends_total(3);
/// ```
#[derive(Debug, Clone, Default)]
pub struct LbMetrics;

impl LbMetrics {
    /// Create a new `LbMetrics` instance.
    ///
    /// This also ensures that metric descriptions are registered with `register_lb_metrics()`.
    pub fn new() -> Self {
        register_lb_metrics();
        Self
    }

    /// Records a load balancer request.
    ///
    /// # Arguments
    ///
    /// * `backend` - The backend address (e.g., "127.0.0.1:3001")
    /// * `status` - The request status ("success" or "failure")
    /// * `duration_secs` - The request duration in seconds
    pub fn record_request(&self, backend: &str, status: &str, duration_secs: f64) {
        counter!(
            "mik_lb_requests_total",
            "backend" => backend.to_string(),
            "status" => status.to_string()
        )
        .increment(1);

        histogram!(
            "mik_lb_request_duration_seconds",
            "backend" => backend.to_string()
        )
        .record(duration_secs);
    }

    /// Records a successful request.
    ///
    /// Convenience method that calls `record_request` with status="success".
    pub fn record_success(&self, backend: &str, duration_secs: f64) {
        self.record_request(backend, "success", duration_secs);
    }

    /// Records a failed request.
    ///
    /// Convenience method that calls `record_request` with status="failure".
    pub fn record_failure(&self, backend: &str, duration_secs: f64) {
        self.record_request(backend, "failure", duration_secs);
    }

    /// Sets the number of healthy backends.
    ///
    /// # Arguments
    ///
    /// * `count` - Number of healthy backends
    pub fn set_backends_healthy(&self, count: usize) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("mik_lb_backends_healthy").set(count as f64);
    }

    /// Sets the total number of backends.
    ///
    /// # Arguments
    ///
    /// * `count` - Total number of backends
    pub fn set_backends_total(&self, count: usize) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("mik_lb_backends_total").set(count as f64);
    }

    /// Sets the active connections for a specific backend.
    ///
    /// # Arguments
    ///
    /// * `backend` - The backend address (e.g., "127.0.0.1:3001")
    /// * `connections` - Number of active connections
    pub fn set_active_connections(&self, backend: &str, connections: u64) {
        #[allow(clippy::cast_precision_loss)]
        gauge!(
            "mik_lb_active_connections",
            "backend" => backend.to_string()
        )
        .set(connections as f64);
    }

    /// Updates backend health metrics from a list of backends.
    ///
    /// This is a convenience method that updates both healthy count,
    /// total count, and active connections for all backends.
    ///
    /// # Arguments
    ///
    /// * `backends` - Iterator of (`address`, `is_healthy`, `active_connections`) tuples
    pub fn update_backend_metrics<'a>(&self, backends: impl Iterator<Item = (&'a str, bool, u64)>) {
        let mut total = 0usize;
        let mut healthy = 0usize;

        for (address, is_healthy, active_connections) in backends {
            total += 1;
            if is_healthy {
                healthy += 1;
            }
            self.set_active_connections(address, active_connections);
        }

        self.set_backends_total(total);
        self.set_backends_healthy(healthy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lb_metrics_new() {
        // Should not panic and should be idempotent
        let _metrics1 = LbMetrics::new();
        let _metrics2 = LbMetrics::new();
    }

    #[test]
    fn test_lb_metrics_default() {
        let metrics = LbMetrics;
        // Just verify it doesn't panic
        metrics.set_backends_total(3);
        metrics.set_backends_healthy(2);
    }

    #[test]
    fn test_record_request() {
        let metrics = LbMetrics::new();

        // Record various requests - should not panic
        metrics.record_request("127.0.0.1:3001", "success", 0.1);
        metrics.record_request("127.0.0.1:3001", "failure", 0.5);
        metrics.record_request("127.0.0.1:3002", "success", 0.2);
    }

    #[test]
    fn test_record_success_failure() {
        let metrics = LbMetrics::new();

        metrics.record_success("127.0.0.1:3001", 0.1);
        metrics.record_failure("127.0.0.1:3001", 0.5);
    }

    #[test]
    fn test_set_backends_gauges() {
        let metrics = LbMetrics::new();

        metrics.set_backends_total(5);
        metrics.set_backends_healthy(3);
    }

    #[test]
    fn test_set_active_connections() {
        let metrics = LbMetrics::new();

        metrics.set_active_connections("127.0.0.1:3001", 10);
        metrics.set_active_connections("127.0.0.1:3002", 5);
    }

    #[test]
    fn test_update_backend_metrics() {
        let metrics = LbMetrics::new();

        let backends = [
            ("127.0.0.1:3001", true, 10u64),
            ("127.0.0.1:3002", false, 0u64),
            ("127.0.0.1:3003", true, 5u64),
        ];

        metrics.update_backend_metrics(
            backends
                .iter()
                .map(|(addr, healthy, conn)| (*addr, *healthy, *conn)),
        );
    }

    #[test]
    fn test_register_lb_metrics_idempotent() {
        // Multiple calls should not panic
        register_lb_metrics();
        register_lb_metrics();
        register_lb_metrics();
    }
}
