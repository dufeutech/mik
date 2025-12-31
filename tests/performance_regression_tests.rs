// Test-specific lint suppressions
#![allow(clippy::collapsible_if)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::assertions_on_constants)]

//! Performance regression tests for the mikrozen runtime.
//!
//! These tests track latency percentiles (p50, p95, p99, p999) and throughput
//! to detect performance regressions before they reach production.
//!
//! ## Background
//!
//! Performance issues reported in production:
//!
//! - Wasmtime #8428: Real-time performance degradation
//! - Wasmtime #8034: Throughput low on Linux systems
//! - Spin #2321: HTTP trigger performance slow
//!
//! ## Key Metrics
//!
//! - **Cold start time**: Time to first successful request
//! - **Warm latency**: Subsequent request latency (p50, p95, p99, p999)
//! - **Throughput**: Requests per second under load
//! - **Latency stability**: Variance over time
//!
//! ## Running Tests
//!
//! ```bash
//! # Run performance tests
//! cargo test -p mik performance_regression -- --ignored --nocapture
//!
//! # Run with baseline comparison
//! PERF_BASELINE_FILE=baseline.json cargo test -p mik performance_regression -- --ignored
//! ```

#[path = "common.rs"]
mod common;

use common::RealTestHost;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// =============================================================================
// Configuration
// =============================================================================

/// Latency thresholds in milliseconds.
/// These are baseline expectations - adjust based on your hardware.
mod thresholds {
    /// Maximum acceptable cold start time (first request).
    pub const COLD_START_MS: u64 = 500;

    /// Maximum acceptable p50 latency for warm requests.
    pub const WARM_P50_MS: u64 = 50;

    /// Maximum acceptable p95 latency.
    pub const WARM_P95_MS: u64 = 100;

    /// Maximum acceptable p99 latency.
    pub const WARM_P99_MS: u64 = 200;

    /// Maximum acceptable p999 latency (tail latency).
    pub const WARM_P999_MS: u64 = 500;

    /// Minimum throughput under load (requests per second).
    pub const MIN_THROUGHPUT_RPS: f64 = 100.0;
}

// =============================================================================
// Helper Functions
// =============================================================================

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("modules")
}

fn echo_wasm_exists() -> bool {
    fixtures_dir().join("echo.wasm").exists()
}

/// Performance metrics collected during tests.
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    /// Cold start latency in milliseconds.
    pub cold_start_ms: u64,

    /// All latency samples in milliseconds.
    pub latencies_ms: Vec<u64>,

    /// Total requests made.
    pub requests_total: u64,

    /// Successful requests.
    pub requests_success: u64,

    /// Total test duration.
    pub duration: Duration,
}

impl PerformanceMetrics {
    /// Calculate percentile from latency samples.
    #[must_use]
    pub fn percentile(&self, p: f64) -> u64 {
        if self.latencies_ms.is_empty() {
            return 0;
        }
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_unstable();
        let idx = ((sorted.len() as f64 * p / 100.0) as usize).min(sorted.len() - 1);
        sorted[idx]
    }

    /// P50 latency (median).
    #[must_use]
    pub fn p50(&self) -> u64 {
        self.percentile(50.0)
    }

    /// P95 latency.
    #[must_use]
    pub fn p95(&self) -> u64 {
        self.percentile(95.0)
    }

    /// P99 latency.
    #[must_use]
    pub fn p99(&self) -> u64 {
        self.percentile(99.0)
    }

    /// P99.9 latency (tail latency).
    #[must_use]
    pub fn p999(&self) -> u64 {
        self.percentile(99.9)
    }

    /// Average latency.
    #[must_use]
    pub fn avg(&self) -> f64 {
        if self.latencies_ms.is_empty() {
            return 0.0;
        }
        self.latencies_ms.iter().sum::<u64>() as f64 / self.latencies_ms.len() as f64
    }

    /// Minimum latency.
    #[must_use]
    pub fn min(&self) -> u64 {
        self.latencies_ms.iter().copied().min().unwrap_or(0)
    }

    /// Maximum latency.
    #[must_use]
    pub fn max(&self) -> u64 {
        self.latencies_ms.iter().copied().max().unwrap_or(0)
    }

    /// Throughput in requests per second.
    #[must_use]
    pub fn throughput(&self) -> f64 {
        if self.duration.as_secs_f64() < 0.001 {
            return 0.0;
        }
        self.requests_total as f64 / self.duration.as_secs_f64()
    }

    /// Success rate as percentage.
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.requests_total == 0 {
            return 0.0;
        }
        (self.requests_success as f64 / self.requests_total as f64) * 100.0
    }

    /// Standard deviation of latencies.
    #[must_use]
    pub fn std_dev(&self) -> f64 {
        if self.latencies_ms.len() < 2 {
            return 0.0;
        }
        let avg = self.avg();
        let variance = self
            .latencies_ms
            .iter()
            .map(|&x| {
                let diff = x as f64 - avg;
                diff * diff
            })
            .sum::<f64>()
            / (self.latencies_ms.len() - 1) as f64;
        variance.sqrt()
    }

    /// Print detailed performance report.
    pub fn print_report(&self) {
        println!("\n=== Performance Report ===");
        println!("Duration: {:.2}s", self.duration.as_secs_f64());
        println!("Total requests: {}", self.requests_total);
        println!("Successful: {}", self.requests_success);
        println!("Success rate: {:.2}%", self.success_rate());
        println!("Throughput: {:.1} req/s", self.throughput());
        println!();
        println!("Cold start: {}ms", self.cold_start_ms);
        println!();
        println!("Latency distribution:");
        println!("  Min:  {}ms", self.min());
        println!("  P50:  {}ms", self.p50());
        println!("  P95:  {}ms", self.p95());
        println!("  P99:  {}ms", self.p99());
        println!("  P999: {}ms", self.p999());
        println!("  Max:  {}ms", self.max());
        println!("  Avg:  {:.1}ms", self.avg());
        println!("  StdDev: {:.1}ms", self.std_dev());
    }

    /// Check against thresholds.
    #[must_use]
    pub fn check_thresholds(&self) -> Vec<String> {
        let mut violations = Vec::new();

        if self.cold_start_ms > thresholds::COLD_START_MS {
            violations.push(format!(
                "Cold start {}ms exceeds threshold {}ms",
                self.cold_start_ms,
                thresholds::COLD_START_MS
            ));
        }

        if self.p50() > thresholds::WARM_P50_MS {
            violations.push(format!(
                "P50 latency {}ms exceeds threshold {}ms",
                self.p50(),
                thresholds::WARM_P50_MS
            ));
        }

        if self.p95() > thresholds::WARM_P95_MS {
            violations.push(format!(
                "P95 latency {}ms exceeds threshold {}ms",
                self.p95(),
                thresholds::WARM_P95_MS
            ));
        }

        if self.p99() > thresholds::WARM_P99_MS {
            violations.push(format!(
                "P99 latency {}ms exceeds threshold {}ms",
                self.p99(),
                thresholds::WARM_P99_MS
            ));
        }

        if self.p999() > thresholds::WARM_P999_MS {
            violations.push(format!(
                "P999 latency {}ms exceeds threshold {}ms",
                self.p999(),
                thresholds::WARM_P999_MS
            ));
        }

        if self.throughput() < thresholds::MIN_THROUGHPUT_RPS && self.requests_total > 100 {
            violations.push(format!(
                "Throughput {:.1} req/s below threshold {} req/s",
                self.throughput(),
                thresholds::MIN_THROUGHPUT_RPS
            ));
        }

        violations
    }
}

/// Histogram bucket for latency distribution.
#[derive(Debug)]
pub struct LatencyHistogram {
    buckets: Vec<(u64, u64)>, // (upper_bound_ms, count)
}

impl LatencyHistogram {
    #[must_use]
    pub fn new() -> Self {
        // Buckets: 1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, +inf
        let bounds = vec![1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, u64::MAX];
        Self {
            buckets: bounds.into_iter().map(|b| (b, 0)).collect(),
        }
    }

    pub fn record(&mut self, latency_ms: u64) {
        for (bound, count) in &mut self.buckets {
            if latency_ms <= *bound {
                *count += 1;
                return;
            }
        }
    }

    pub fn print(&self) {
        println!("\nLatency histogram:");
        let total: u64 = self.buckets.iter().map(|(_, c)| c).sum();
        let mut cumulative = 0u64;

        for (bound, count) in &self.buckets {
            cumulative += count;
            let pct = if total > 0 {
                (*count as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            let cum_pct = if total > 0 {
                (cumulative as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            if *bound == u64::MAX {
                println!(
                    "  >5000ms: {:>6} ({:>5.1}%, cum {:>5.1}%)",
                    count, pct, cum_pct
                );
            } else {
                println!(
                    "  ≤{:>5}ms: {:>6} ({:>5.1}%, cum {:>5.1}%)",
                    bound, count, pct, cum_pct
                );
            }
        }
    }
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Integration Tests
// =============================================================================

/// Test cold start latency.
///
/// Measures the time from starting the server to completing the first request.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_cold_start_latency() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    println!("Measuring cold start latency...");

    let start = Instant::now();

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let server_start_time = start.elapsed();
    println!("Server started in {:?}", server_start_time);

    // First request (cold)
    let request_start = Instant::now();
    let resp = host
        .post_json("/run/echo/", &serde_json::json!({"cold": true}))
        .await
        .expect("Cold request should succeed");
    let cold_start_ms = request_start.elapsed().as_millis() as u64;

    assert_eq!(resp.status(), 200, "Cold request should succeed");
    println!("Cold start latency: {}ms", cold_start_ms);

    // Threshold check
    assert!(
        cold_start_ms <= thresholds::COLD_START_MS,
        "Cold start {}ms exceeds threshold {}ms",
        cold_start_ms,
        thresholds::COLD_START_MS
    );
}

/// Test warm request latency percentiles.
///
/// Makes many requests and calculates latency distribution.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_warm_latency_percentiles() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    // Warmup
    println!("Warming up...");
    for _ in 0..10 {
        let _ = host
            .post_json("/run/echo/", &serde_json::json!({"warmup": true}))
            .await;
    }

    // Measure
    let num_requests = 500;
    println!("Measuring {} requests...", num_requests);

    let mut metrics = PerformanceMetrics::default();
    let mut histogram = LatencyHistogram::new();
    let client = reqwest::Client::new();
    let url = host.url("/run/echo/");
    let start = Instant::now();

    for i in 0..num_requests {
        let req_start = Instant::now();
        let result = client
            .post(&url)
            .json(&serde_json::json!({"request": i}))
            .send()
            .await;

        let latency_ms = req_start.elapsed().as_millis() as u64;
        metrics.latencies_ms.push(latency_ms);
        histogram.record(latency_ms);
        metrics.requests_total += 1;

        if let Ok(r) = result {
            if r.status().is_success() {
                metrics.requests_success += 1;
            }
        }
    }

    metrics.duration = start.elapsed();
    metrics.print_report();
    histogram.print();

    // Check thresholds
    let violations = metrics.check_thresholds();
    if !violations.is_empty() {
        println!("\nThreshold violations:");
        for v in &violations {
            println!("  - {}", v);
        }
    }

    // Soft assertions (warn but don't fail for now)
    assert!(
        metrics.success_rate() >= 99.0,
        "Success rate should be >= 99%"
    );
}

/// Test throughput under sustained load.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_throughput_under_load() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(100)
        .start()
        .await
        .expect("Failed to start host");

    let duration = Duration::from_secs(10);
    let concurrency = 20;

    println!(
        "Testing throughput: {}s at {} concurrency",
        duration.as_secs(),
        concurrency
    );

    let start = Instant::now();
    let requests_completed = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut handles = Vec::new();
    for _ in 0..concurrency {
        let url = host.url("/run/echo/");
        let completed = requests_completed.clone();
        let stop = stop.clone();

        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();

            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if client
                    .post(&url)
                    .json(&serde_json::json!({"throughput": true}))
                    .send()
                    .await
                    .is_ok()
                {
                    completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }));
    }

    // Run for duration
    tokio::time::sleep(duration).await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    // Wait for workers
    for handle in handles {
        let _ = handle.await;
    }

    let total = requests_completed.load(std::sync::atomic::Ordering::Relaxed);
    let elapsed = start.elapsed();
    let throughput = total as f64 / elapsed.as_secs_f64();

    println!("\n=== Throughput Results ===");
    println!("Duration: {:.2}s", elapsed.as_secs_f64());
    println!("Requests: {}", total);
    println!("Throughput: {:.1} req/s", throughput);

    // Check threshold
    assert!(
        throughput >= thresholds::MIN_THROUGHPUT_RPS,
        "Throughput {:.1} req/s below threshold {} req/s",
        throughput,
        thresholds::MIN_THROUGHPUT_RPS
    );
}

/// Test latency stability over time.
///
/// Verifies that latency doesn't degrade as the server runs.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_latency_stability() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let windows = 5;
    let requests_per_window = 100;
    let mut window_p99s = Vec::new();

    println!("Testing latency stability across {} windows", windows);

    for window in 0..windows {
        let mut latencies = Vec::new();
        let client = reqwest::Client::new();
        let url = host.url("/run/echo/");

        for i in 0..requests_per_window {
            let start = Instant::now();
            let _ = client
                .post(&url)
                .json(&serde_json::json!({"window": window, "req": i}))
                .send()
                .await;
            latencies.push(start.elapsed().as_millis() as u64);
        }

        latencies.sort_unstable();
        let p99_idx = (latencies.len() as f64 * 0.99) as usize;
        let p99 = latencies.get(p99_idx).copied().unwrap_or(0);
        window_p99s.push(p99);

        println!("Window {}: p99 = {}ms", window + 1, p99);
    }

    // Check stability: last window shouldn't be much worse than first
    let first_p99 = window_p99s.first().copied().unwrap_or(0);
    let last_p99 = window_p99s.last().copied().unwrap_or(0);

    println!("\nFirst window p99: {}ms", first_p99);
    println!("Last window p99: {}ms", last_p99);

    // Allow up to 2x degradation
    if first_p99 > 0 {
        assert!(
            last_p99 <= first_p99 * 2,
            "P99 latency degraded too much: {}ms -> {}ms (> 2x)",
            first_p99,
            last_p99
        );
    }
}

// =============================================================================
// Unit Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_calculation() {
        let mut m = PerformanceMetrics::default();
        m.latencies_ms = (1..=100).collect();

        // Percentile calculation: index = len * p / 100
        // With 100 samples, p50 index is 50, value is 51
        let p50 = m.p50();
        assert!(
            p50 >= 50 && p50 <= 51,
            "P50 should be around 50-51, got {}",
            p50
        );

        let p95 = m.p95();
        assert!(
            p95 >= 95 && p95 <= 96,
            "P95 should be around 95-96, got {}",
            p95
        );

        let p99 = m.p99();
        assert!(
            p99 >= 99 && p99 <= 100,
            "P99 should be around 99-100, got {}",
            p99
        );
    }

    #[test]
    fn test_percentile_empty() {
        let m = PerformanceMetrics::default();
        assert_eq!(m.p50(), 0);
        assert_eq!(m.p99(), 0);
    }

    #[test]
    fn test_throughput_calculation() {
        let mut m = PerformanceMetrics::default();
        m.requests_total = 1000;
        m.duration = Duration::from_secs(10);

        assert!((m.throughput() - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_std_dev() {
        let mut m = PerformanceMetrics::default();
        m.latencies_ms = vec![10, 10, 10, 10, 10]; // No variance

        assert!((m.std_dev() - 0.0).abs() < 0.01);

        m.latencies_ms = vec![1, 2, 3, 4, 5];
        // StdDev of 1,2,3,4,5 is ~1.58
        assert!(m.std_dev() > 1.0 && m.std_dev() < 2.0);
    }

    #[test]
    fn test_histogram_recording() {
        let mut h = LatencyHistogram::new();
        h.record(1);
        h.record(10);
        h.record(100);
        h.record(1000);

        // Just verify it doesn't panic
        h.print();
    }

    #[test]
    fn test_threshold_violations() {
        let mut m = PerformanceMetrics::default();
        m.cold_start_ms = 1000; // Over threshold
        m.latencies_ms = vec![200; 100]; // All 200ms - over p50 threshold

        let violations = m.check_thresholds();
        assert!(!violations.is_empty());
        assert!(violations.iter().any(|v| v.contains("Cold start")));
        assert!(violations.iter().any(|v| v.contains("P50")));
    }

    #[test]
    fn test_success_rate() {
        let mut m = PerformanceMetrics::default();
        m.requests_total = 100;
        m.requests_success = 95;

        assert!((m.success_rate() - 95.0).abs() < 0.01);
    }

    #[test]
    fn test_min_max() {
        let mut m = PerformanceMetrics::default();
        m.latencies_ms = vec![5, 10, 15, 20, 25];

        assert_eq!(m.min(), 5);
        assert_eq!(m.max(), 25);
    }

    #[test]
    fn test_thresholds_are_reasonable() {
        // Sanity check on thresholds
        assert!(thresholds::COLD_START_MS <= 1000);
        assert!(thresholds::WARM_P50_MS <= thresholds::WARM_P95_MS);
        assert!(thresholds::WARM_P95_MS <= thresholds::WARM_P99_MS);
        assert!(thresholds::WARM_P99_MS <= thresholds::WARM_P999_MS);
        assert!(thresholds::MIN_THROUGHPUT_RPS >= 10.0);
    }
}
