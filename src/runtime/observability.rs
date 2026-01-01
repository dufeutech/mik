//! Observability module for health checks, metrics, and memory usage.
//!
//! This module provides runtime observability features:
//! - Health status with cache and memory information
//! - Prometheus-format metrics export
//! - Platform-specific memory usage tracking

use super::SharedState;
use super::types::{HealthDetail, HealthStatus, MemoryStats};
use crate::constants;
use std::sync::atomic::Ordering;

impl SharedState {
    /// Get health status with configurable detail level.
    pub(crate) fn get_health_status(&self, detail: HealthDetail) -> HealthStatus {
        // Get cache stats (no lock needed - moka is thread-safe)
        let loaded_modules = if detail == HealthDetail::Full {
            // Collect module names from cache with pre-allocated capacity
            self.cache.run_pending_tasks();
            let cache_size = self.cache.entry_count() as usize;
            let mut modules = Vec::with_capacity(cache_size);
            for (key, _) in &self.cache {
                modules.push((*key).clone());
            }
            Some(modules)
        } else {
            None
        };

        let cache_size = self.cache.entry_count() as usize;
        let cache_bytes = self.cache.weighted_size() as usize;

        HealthStatus {
            status: constants::HEALTH_STATUS_READY.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            cache_size,
            cache_capacity: self.config.cache_size,
            cache_bytes,
            cache_max_bytes: self.config.max_cache_bytes,
            total_requests: self.request_counter.load(Ordering::Relaxed),
            memory: MemoryStats {
                allocated_bytes: get_memory_usage(),
                limit_per_request_bytes: self.config.memory_limit_bytes,
            },
            loaded_modules,
        }
    }

    /// Generate Prometheus-format metrics.
    pub(crate) fn get_prometheus_metrics(&self) -> String {
        use std::fmt::Write;

        let total_requests = self.request_counter.load(Ordering::Relaxed);
        let cache_entries = self.cache.entry_count();
        let cache_bytes = self.cache.weighted_size();
        let circuit_states = self.circuit_breaker.get_all_states();

        let mut output = String::with_capacity(2048);

        // Help and type declarations
        output.push_str("# HELP mik_requests_total Total number of HTTP requests received\n");
        output.push_str("# TYPE mik_requests_total counter\n");
        let _ = writeln!(output, "mik_requests_total {total_requests}\n");

        output.push_str("# HELP mik_cache_entries Number of modules in cache\n");
        output.push_str("# TYPE mik_cache_entries gauge\n");
        let _ = writeln!(output, "mik_cache_entries {cache_entries}\n");

        output.push_str("# HELP mik_cache_bytes Total bytes used by cached modules\n");
        output.push_str("# TYPE mik_cache_bytes gauge\n");
        let _ = writeln!(output, "mik_cache_bytes {cache_bytes}\n");

        output.push_str("# HELP mik_cache_capacity_bytes Maximum cache size in bytes\n");
        output.push_str("# TYPE mik_cache_capacity_bytes gauge\n");
        let _ = writeln!(
            output,
            "mik_cache_capacity_bytes {}\n",
            self.config.max_cache_bytes
        );

        output.push_str("# HELP mik_max_concurrent_requests Maximum allowed concurrent requests\n");
        output.push_str("# TYPE mik_max_concurrent_requests gauge\n");
        let _ = writeln!(
            output,
            "mik_max_concurrent_requests {}\n",
            self.config.max_concurrent_requests
        );

        output
            .push_str("# HELP mik_circuit_breaker_state Circuit breaker state per module (0=closed, 1=open, 2=half-open)\n");
        output.push_str("# TYPE mik_circuit_breaker_state gauge\n");
        for (module, state) in &circuit_states {
            let state_value = match state.as_str() {
                "open" => 1,
                "half_open" => 2,
                _ => 0, // closed or unknown
            };
            let _ = writeln!(
                output,
                "mik_circuit_breaker_state{{module=\"{module}\"}} {state_value}"
            );
        }
        if !circuit_states.is_empty() {
            output.push('\n');
        }

        // Memory usage (if available)
        if let Some(mem) = get_memory_usage() {
            output.push_str("# HELP mik_memory_bytes Process memory usage in bytes\n");
            output.push_str("# TYPE mik_memory_bytes gauge\n");
            let _ = writeln!(output, "mik_memory_bytes {mem}");
        }

        output
    }
}

/// Get current memory usage (platform-specific).
pub(crate) fn get_memory_usage() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().next().map(String::from))
            .and_then(|s| s.parse::<usize>().ok())
            .map(|pages| pages * 4096)
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
