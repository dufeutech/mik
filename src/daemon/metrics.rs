//! Prometheus metrics for the mik daemon.
#![allow(dead_code)] // Metrics infrastructure for future observability
#![allow(clippy::cast_precision_loss)]
//!
//! Provides observability through Prometheus-compatible metrics.
//! Metrics are exposed at `GET /metrics` in Prometheus text format.
//!
//! # Metrics Exposed
//!
//! ## Request Metrics
//! - `mik_http_requests_total` - Total HTTP requests (labels: method, path, status)
//! - `mik_http_request_duration_seconds` - Request duration histogram
//!
//! ## Instance Metrics
//! - `mik_instances_total` - Total instances by status (labels: status)
//! - `mik_instance_uptime_seconds` - Instance uptime gauge
//!
//! ## Service Metrics
//! - `mik_kv_operations_total` - KV operations (labels: operation)
//! - `mik_sql_queries_total` - SQL queries (labels: type)
//! - `mik_storage_operations_total` - Storage operations (labels: operation)
//! - `mik_queue_operations_total` - Queue operations (labels: operation, queue)
//!
//! ## Scheduler Metrics
//! - `mik_cron_executions_total` - Cron job executions (labels: job, success)
//! - `mik_cron_execution_duration_seconds` - Cron job duration histogram

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;

/// Global Prometheus handle for rendering metrics.
static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Initializes the metrics system.
///
/// Must be called once at startup before recording any metrics.
/// Returns the Prometheus handle for rendering metrics.
pub fn init_metrics() -> PrometheusHandle {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");

    // Register metric descriptions
    register_metrics();

    // Store handle for later access
    let _ = PROMETHEUS_HANDLE.set(handle.clone());

    handle
}

/// Gets the global Prometheus handle.
pub fn get_handle() -> Option<&'static PrometheusHandle> {
    PROMETHEUS_HANDLE.get()
}

/// Registers all metric descriptions.
fn register_metrics() {
    // HTTP metrics
    describe_counter!("mik_http_requests_total", "Total number of HTTP requests");
    describe_histogram!(
        "mik_http_request_duration_seconds",
        "HTTP request duration in seconds"
    );

    // Instance metrics
    describe_gauge!("mik_instances_total", "Total number of instances by status");
    describe_gauge!("mik_instance_uptime_seconds", "Instance uptime in seconds");

    // KV metrics
    describe_counter!("mik_kv_operations_total", "Total KV store operations");

    // SQL metrics
    describe_counter!("mik_sql_queries_total", "Total SQL queries executed");
    describe_histogram!(
        "mik_sql_query_duration_seconds",
        "SQL query duration in seconds"
    );

    // Storage metrics
    describe_counter!("mik_storage_operations_total", "Total storage operations");
    describe_counter!(
        "mik_storage_bytes_total",
        "Total bytes transferred (read/write)"
    );

    // Queue metrics
    describe_counter!("mik_queue_operations_total", "Total queue operations");
    describe_gauge!("mik_queue_length", "Current queue length");

    // Cron metrics
    describe_counter!("mik_cron_executions_total", "Total cron job executions");
    describe_histogram!(
        "mik_cron_execution_duration_seconds",
        "Cron job execution duration in seconds"
    );
}

// =============================================================================
// HTTP Metrics
// =============================================================================

/// Records an HTTP request.
pub fn record_http_request(method: &str, path: &str, status: u16, duration_secs: f64) {
    let status_str = status.to_string();

    counter!(
        "mik_http_requests_total",
        "method" => method.to_string(),
        "path" => normalize_path(path),
        "status" => status_str
    )
    .increment(1);

    histogram!(
        "mik_http_request_duration_seconds",
        "method" => method.to_string(),
        "path" => normalize_path(path)
    )
    .record(duration_secs);
}

/// Normalizes a path for metrics (removes variable segments).
fn normalize_path(path: &str) -> String {
    // Replace UUIDs and numeric IDs with placeholders
    let path = path.trim_start_matches('/');

    let segments: Vec<&str> = path.split('/').collect();
    let normalized: Vec<String> = segments
        .iter()
        .map(|seg| {
            // Replace UUIDs
            if seg.len() == 36 && seg.contains('-') {
                return ":id".to_string();
            }
            // Replace numeric IDs
            if seg.parse::<u64>().is_ok() {
                return ":id".to_string();
            }
            (*seg).to_string()
        })
        .collect();

    format!("/{}", normalized.join("/"))
}

// =============================================================================
// Instance Metrics
// =============================================================================

/// Records instance count by status.
pub fn set_instance_count(status: &str, count: u64) {
    gauge!(
        "mik_instances_total",
        "status" => status.to_string()
    )
    .set(count as f64);
}

/// Records instance uptime.
pub fn set_instance_uptime(name: &str, uptime_secs: f64) {
    gauge!(
        "mik_instance_uptime_seconds",
        "instance" => name.to_string()
    )
    .set(uptime_secs);
}

// =============================================================================
// KV Metrics
// =============================================================================

/// Records a KV operation.
pub fn record_kv_operation(operation: &str) {
    counter!(
        "mik_kv_operations_total",
        "operation" => operation.to_string()
    )
    .increment(1);
}

// =============================================================================
// SQL Metrics
// =============================================================================

/// Records a SQL query.
pub fn record_sql_query(query_type: &str, duration_secs: f64) {
    counter!(
        "mik_sql_queries_total",
        "type" => query_type.to_string()
    )
    .increment(1);

    histogram!(
        "mik_sql_query_duration_seconds",
        "type" => query_type.to_string()
    )
    .record(duration_secs);
}

// =============================================================================
// Storage Metrics
// =============================================================================

/// Records a storage operation.
pub fn record_storage_operation(operation: &str, bytes: Option<u64>) {
    counter!(
        "mik_storage_operations_total",
        "operation" => operation.to_string()
    )
    .increment(1);

    if let Some(bytes) = bytes {
        counter!(
            "mik_storage_bytes_total",
            "operation" => operation.to_string()
        )
        .increment(bytes);
    }
}

// =============================================================================
// Queue Metrics
// =============================================================================

/// Records a queue operation.
pub fn record_queue_operation(operation: &str, queue: &str) {
    counter!(
        "mik_queue_operations_total",
        "operation" => operation.to_string(),
        "queue" => queue.to_string()
    )
    .increment(1);
}

/// Sets the current queue length.
pub fn set_queue_length(queue: &str, length: usize) {
    gauge!(
        "mik_queue_length",
        "queue" => queue.to_string()
    )
    .set(length as f64);
}

// =============================================================================
// Cron Metrics
// =============================================================================

/// Records a cron job execution.
pub fn record_cron_execution(job: &str, success: bool, duration_secs: f64) {
    let success_str = if success { "true" } else { "false" };

    counter!(
        "mik_cron_executions_total",
        "job" => job.to_string(),
        "success" => success_str.to_string()
    )
    .increment(1);

    histogram!(
        "mik_cron_execution_duration_seconds",
        "job" => job.to_string()
    )
    .record(duration_secs);
}

// =============================================================================
// Metrics Rendering
// =============================================================================

/// Renders all metrics in Prometheus text format.
pub fn render_metrics() -> String {
    match get_handle() {
        Some(handle) => handle.render(),
        None => "# Metrics not initialized\n".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("/api/users"), "/api/users");
        assert_eq!(normalize_path("/api/users/123"), "/api/users/:id");
        assert_eq!(
            normalize_path("/api/users/550e8400-e29b-41d4-a716-446655440000"),
            "/api/users/:id"
        );
        assert_eq!(normalize_path("/kv/mykey"), "/kv/mykey");
        assert_eq!(normalize_path("/queues/jobs/push"), "/queues/jobs/push");
    }

    #[test]
    fn test_normalize_path_preserves_names() {
        assert_eq!(normalize_path("/instances/default"), "/instances/default");
        assert_eq!(
            normalize_path("/storage/images/logo.png"),
            "/storage/images/logo.png"
        );
    }
}
