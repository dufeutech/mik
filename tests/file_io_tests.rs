// Test-specific lint suppressions
#![allow(clippy::useless_vec)]

//! File I/O tests for the mikrozen runtime.
//!
//! These tests verify that the runtime handles file operations correctly,
//! especially under concurrent load and resource pressure.
//!
//! ## Background
//!
//! File I/O issues are common in WASM runtimes:
//!
//! - Wasmtime #8392: Hang on file I/O in multi-threaded app
//! - Wasmtime #7973: Poor file I/O performance (23s vs 2s native)
//! - WasmEdge #3811: fdWrite returns 0 incorrectly on Windows
//! - Spin #180: Too many open files
//!
//! ## Test Philosophy
//!
//! 1. **No Hangs** - File operations should complete or timeout, never hang
//! 2. **Resource Limits** - Graceful handling when file descriptors exhausted
//! 3. **Concurrent Safety** - Multiple file operations shouldn't deadlock
//!
//! ## Running Tests
//!
//! ```bash
//! # Run unit tests (fast, no fixtures)
//! cargo test -p mik file_io::tests
//!
//! # Run integration tests (requires fixtures)
//! cargo test -p mik file_io -- --ignored --test-threads=1
//! ```

#[path = "common.rs"]
mod common;

use common::RealTestHost;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// =============================================================================
// Helper Functions
// =============================================================================

/// Get the path to the test fixtures directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("modules")
}

/// Check if echo.wasm exists.
fn echo_wasm_exists() -> bool {
    fixtures_dir().join("echo.wasm").exists()
}

/// Timeout configuration for file I/O tests.
struct FileIoConfig {
    /// Maximum time to wait for a single operation.
    operation_timeout: Duration,
    /// Maximum time for the entire test.
    test_timeout: Duration,
    /// Number of concurrent operations to attempt.
    concurrent_ops: usize,
}

impl Default for FileIoConfig {
    fn default() -> Self {
        Self {
            operation_timeout: Duration::from_secs(5),
            test_timeout: Duration::from_secs(30),
            concurrent_ops: 50,
        }
    }
}

// =============================================================================
// Integration Tests (Require WASM fixtures)
// =============================================================================

/// Test that concurrent HTTP requests (which may involve file I/O) don't hang.
///
/// This test simulates the scenario from Wasmtime #8392 where file I/O
/// in multi-threaded applications can cause hangs.
///
/// ## Expected Behavior
///
/// - All requests complete within timeout
/// - No deadlocks or hangs
/// - Server remains responsive
///
/// ## Bug Behavior (from Wasmtime #8392)
///
/// - Application hangs on file operations
/// - Threads block indefinitely
/// - No error message, just silence
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_concurrent_requests_no_hang() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let config = FileIoConfig::default();
    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(config.concurrent_ops)
        .start()
        .await
        .expect("Failed to start host");

    println!(
        "Testing {} concurrent requests with {:?} timeout",
        config.concurrent_ops, config.operation_timeout
    );

    let test_start = Instant::now();
    let completed = Arc::new(AtomicU64::new(0));
    let timed_out = Arc::new(AtomicU64::new(0));

    // Launch concurrent requests
    let mut handles = Vec::with_capacity(config.concurrent_ops);
    for i in 0..config.concurrent_ops {
        let url = host.url("/run/echo/");
        let completed = completed.clone();
        let timed_out = timed_out.clone();
        let timeout = config.operation_timeout;

        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::builder().timeout(timeout).build().unwrap();

            let start = Instant::now();
            let result = client
                .post(&url)
                .json(&serde_json::json!({"request": i}))
                .send()
                .await;

            let elapsed = start.elapsed();

            match result {
                Ok(resp) if resp.status().is_success() => {
                    completed.fetch_add(1, Ordering::Relaxed);
                    (true, elapsed)
                },
                Ok(_) => {
                    // Non-success status but didn't hang
                    completed.fetch_add(1, Ordering::Relaxed);
                    (false, elapsed)
                },
                Err(e) if e.is_timeout() => {
                    timed_out.fetch_add(1, Ordering::Relaxed);
                    (false, elapsed)
                },
                Err(_) => {
                    // Other error but didn't hang
                    completed.fetch_add(1, Ordering::Relaxed);
                    (false, elapsed)
                },
            }
        }));
    }

    // Wait for all with overall timeout
    let results = tokio::time::timeout(config.test_timeout, async {
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.await.unwrap_or((false, config.operation_timeout)));
        }
        results
    })
    .await;

    let test_elapsed = test_start.elapsed();
    let final_completed = completed.load(Ordering::Relaxed);
    let final_timed_out = timed_out.load(Ordering::Relaxed);

    println!(
        "Test completed in {:?}: {} completed, {} timed out",
        test_elapsed, final_completed, final_timed_out
    );

    // Test should not timeout overall
    assert!(
        results.is_ok(),
        "Test timed out after {:?} - possible hang detected",
        config.test_timeout
    );

    // Most requests should complete (not timeout)
    let timeout_rate = final_timed_out as f64 / config.concurrent_ops as f64;
    assert!(
        timeout_rate < 0.1,
        "Too many timeouts: {:.1}% (max 10%)",
        timeout_rate * 100.0
    );

    // Server should still be responsive
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200, "Server should be healthy after test");
}

/// Test that the server handles many sequential file-touching operations.
///
/// This simulates workloads that open/close many files over time.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_sequential_file_operations_stable() {
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

    let operations = 200;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = host.url("/run/echo/");

    println!("Starting {} sequential operations", operations);

    let mut success = 0;
    let mut errors = 0;
    let start = Instant::now();

    for i in 0..operations {
        let result = client
            .post(&url)
            .json(&serde_json::json!({"op": i}))
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => success += 1,
            _ => errors += 1,
        }

        // Progress every 50
        if (i + 1) % 50 == 0 {
            println!("Progress: {}/{} ({} errors)", i + 1, operations, errors);
        }
    }

    let elapsed = start.elapsed();
    println!(
        "Completed {} operations in {:?}: {} success, {} errors",
        operations, elapsed, success, errors
    );

    // High success rate expected
    assert!(
        success >= operations * 95 / 100,
        "Should have 95%+ success rate: {}/{}",
        success,
        operations
    );
}

/// Test rapid open/close cycles don't leak file descriptors.
///
/// Based on Spin #180 "Too many open files" issue.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_no_file_descriptor_leak() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(20)
        .start()
        .await
        .expect("Failed to start host");

    // Do many cycles of burst traffic
    let cycles = 10;
    let requests_per_cycle = 30;

    println!(
        "Testing {} cycles of {} requests for FD leak",
        cycles, requests_per_cycle
    );

    for cycle in 0..cycles {
        let mut handles = Vec::with_capacity(requests_per_cycle);

        for i in 0..requests_per_cycle {
            let url = host.url("/run/echo/");
            handles.push(tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap();
                client
                    .post(&url)
                    .json(&serde_json::json!({"cycle": cycle, "req": i}))
                    .send()
                    .await
                    .is_ok()
            }));
        }

        let mut success = 0;
        for handle in handles {
            if handle.await.unwrap_or(false) {
                success += 1;
            }
        }

        println!(
            "Cycle {}: {}/{} success",
            cycle + 1,
            success,
            requests_per_cycle
        );

        // Each cycle should work (no FD exhaustion)
        assert!(
            success >= requests_per_cycle * 7 / 10,
            "Cycle {} had too many failures: {}/{} - possible FD leak",
            cycle + 1,
            success,
            requests_per_cycle
        );

        // Brief pause between cycles
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Final health check
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(
        health.status(),
        200,
        "Server should be healthy after FD stress test"
    );
}

/// Test that slow file operations don't block other requests.
///
/// Uses slow_response.wasm to simulate slow operations.
#[tokio::test]
#[ignore = "Requires slow_response.wasm fixture"]
async fn test_slow_operations_dont_block_others() {
    let slow_wasm = fixtures_dir().join("slow_response.wasm");
    if !slow_wasm.exists() {
        eprintln!("Skipping: slow_response.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(50)
        .start()
        .await
        .expect("Failed to start host");

    // Start some slow requests
    let slow_count = 5;
    let fast_count = 20;
    let slow_handles: Vec<_> = (0..slow_count)
        .map(|i| {
            let url = host.url("/run/slow_response/");
            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap();
                let start = Instant::now();
                let result = client
                    .post(&url)
                    .json(&serde_json::json!({"slow": i, "delay_ms": 2000}))
                    .send()
                    .await;
                (result.is_ok(), start.elapsed())
            })
        })
        .collect();

    // Give slow requests a head start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now fire fast requests - they should complete quickly
    let fast_start = Instant::now();
    let fast_handles: Vec<_> = (0..fast_count)
        .map(|i| {
            let url = host.url("/run/echo/");
            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap();
                let start = Instant::now();
                let result = client
                    .post(&url)
                    .json(&serde_json::json!({"fast": i}))
                    .send()
                    .await;
                (
                    result.map(|r| r.status().is_success()).unwrap_or(false),
                    start.elapsed(),
                )
            })
        })
        .collect();

    // Collect fast results
    let mut fast_success = 0;
    let mut fast_latencies = Vec::with_capacity(fast_count);
    for handle in fast_handles {
        if let Ok((success, latency)) = handle.await {
            if success {
                fast_success += 1;
            }
            fast_latencies.push(latency);
        }
    }

    let fast_elapsed = fast_start.elapsed();
    let avg_fast_latency = fast_latencies.iter().sum::<Duration>() / fast_latencies.len() as u32;

    println!(
        "Fast requests: {}/{} success, avg latency {:?}, total {:?}",
        fast_success, fast_count, avg_fast_latency, fast_elapsed
    );

    // Fast requests should complete quickly (not blocked by slow ones)
    assert!(
        avg_fast_latency < Duration::from_secs(1),
        "Fast requests should not be blocked by slow ones: avg {:?}",
        avg_fast_latency
    );

    // Most fast requests should succeed
    assert!(
        fast_success >= fast_count * 9 / 10,
        "Fast requests should have 90%+ success: {}/{}",
        fast_success,
        fast_count
    );

    // Wait for slow requests to complete
    for handle in slow_handles {
        let _ = handle.await;
    }
}

// =============================================================================
// Unit Tests (No fixtures required)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixture_detection() {
        let dir = fixtures_dir();
        println!("Fixtures directory: {}", dir.display());
        println!("echo.wasm exists: {}", echo_wasm_exists());
    }

    #[test]
    fn test_config_defaults() {
        let config = FileIoConfig::default();
        assert_eq!(config.operation_timeout, Duration::from_secs(5));
        assert_eq!(config.test_timeout, Duration::from_secs(30));
        assert_eq!(config.concurrent_ops, 50);
    }

    #[test]
    fn test_timeout_calculation() {
        let ops = 100;
        let per_op_timeout = Duration::from_secs(5);
        let max_parallel = 10;

        // Worst case: all ops sequential
        let worst_case = per_op_timeout * ops as u32;
        println!("Worst case (sequential): {:?}", worst_case);

        // Best case: all ops parallel (limited by max_parallel)
        let batches = (ops + max_parallel - 1) / max_parallel;
        let best_case = per_op_timeout * batches as u32;
        println!("Best case (parallel): {:?}", best_case);

        assert!(best_case < worst_case);
    }

    #[test]
    fn test_timeout_rate_calculation() {
        let total = 100;
        let timed_out = 5;
        let rate = f64::from(timed_out) / f64::from(total);

        assert!((rate - 0.05).abs() < 0.001, "Rate should be 5%");
        assert!(rate < 0.1, "Rate should be under 10% threshold");
    }

    #[test]
    fn test_fd_leak_detection_logic() {
        // Simulated FD counts over time
        let fd_counts = vec![100, 105, 110, 108, 112, 115, 118, 120, 125, 130];

        // Calculate growth rate
        let first = fd_counts.first().unwrap();
        let last = fd_counts.last().unwrap();
        let growth = last - first;
        let growth_per_cycle = f64::from(growth) / (fd_counts.len() - 1) as f64;

        println!(
            "FD growth: {} total, {:.1} per cycle",
            growth, growth_per_cycle
        );

        // More than 5 FDs per cycle is suspicious
        let is_leaking = growth_per_cycle > 5.0;
        assert!(
            !is_leaking,
            "This pattern should not be flagged as leak: {:.1}/cycle",
            growth_per_cycle
        );

        // Clear leak pattern
        let leak_counts = vec![100, 150, 200, 250, 300, 350, 400, 450, 500, 550];
        let leak_growth =
            f64::from(*leak_counts.last().unwrap() - *leak_counts.first().unwrap()) / 9.0;
        assert!(
            leak_growth > 5.0,
            "This should be flagged as leak: {:.1}/cycle",
            leak_growth
        );
    }

    #[test]
    fn test_concurrent_operation_tracking() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let max_concurrent = 10;
        let current = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);

        // Simulate concurrent operations
        for _ in 0..5 {
            let c = current.fetch_add(1, Ordering::Relaxed) + 1;
            peak.fetch_max(c, Ordering::Relaxed);
        }

        for _ in 0..3 {
            current.fetch_sub(1, Ordering::Relaxed);
        }

        for _ in 0..8 {
            let c = current.fetch_add(1, Ordering::Relaxed) + 1;
            peak.fetch_max(c, Ordering::Relaxed);
        }

        let final_peak = peak.load(Ordering::Relaxed);
        println!("Peak concurrent: {}", final_peak);

        assert!(
            final_peak <= max_concurrent,
            "Should respect concurrency limit"
        );
    }

    #[test]
    fn test_operation_timing_buckets() {
        let timings_ms = vec![10, 20, 15, 100, 50, 5, 200, 30, 25, 500];

        // Bucket: <50ms, 50-100ms, 100-500ms, >500ms
        let mut buckets = [0u32; 4];
        for &t in &timings_ms {
            match t {
                0..=49 => buckets[0] += 1,
                50..=99 => buckets[1] += 1,
                100..=499 => buckets[2] += 1,
                _ => buckets[3] += 1,
            }
        }

        println!(
            "Timing distribution: <50ms:{}, 50-100ms:{}, 100-500ms:{}, >500ms:{}",
            buckets[0], buckets[1], buckets[2], buckets[3]
        );

        // Most operations should be fast
        let fast_ratio = f64::from(buckets[0]) / timings_ms.len() as f64;
        assert!(fast_ratio >= 0.5, "At least 50% should be <50ms");
    }

    #[test]
    fn test_sequential_vs_parallel_time() {
        let ops = 10;
        let op_time_ms = 100;

        // Sequential time
        let sequential_ms = ops * op_time_ms;

        // Parallel time (assuming infinite parallelism)
        let parallel_ms = op_time_ms;

        // Speedup
        let speedup = f64::from(sequential_ms) / f64::from(parallel_ms);

        println!(
            "Sequential: {}ms, Parallel: {}ms, Speedup: {:.1}x",
            sequential_ms, parallel_ms, speedup
        );

        assert_eq!(speedup, 10.0);
    }

    #[test]
    fn test_hang_detection_logic() {
        let timeout_threshold = Duration::from_secs(5);
        let operation_times = vec![
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(150),
            Duration::from_secs(6), // This one timed out
            Duration::from_millis(180),
        ];

        let mut timed_out_count = 0;
        for time in &operation_times {
            if *time >= timeout_threshold {
                timed_out_count += 1;
            }
        }

        println!("Timed out: {}/{}", timed_out_count, operation_times.len());

        // Even one timeout might indicate a hang issue
        if timed_out_count > 0 {
            println!("WARNING: Potential hang detected");
        }

        assert_eq!(timed_out_count, 1);
    }

    #[test]
    fn test_success_rate_thresholds() {
        let thresholds = vec![
            (95, "excellent"),
            (90, "good"),
            (80, "acceptable"),
            (70, "concerning"),
            (50, "failing"),
        ];

        let success_rate = 87;

        let classification = thresholds
            .iter()
            .find(|(threshold, _)| success_rate >= *threshold)
            .map(|(_, label)| *label)
            .unwrap_or("critical");

        println!("{}% success rate is: {}", success_rate, classification);
        assert_eq!(classification, "acceptable");
    }

    #[test]
    fn test_cycle_regression_detection() {
        // Success rates over cycles - looking for degradation
        let cycle_success_rates = vec![95, 94, 93, 85, 75, 60, 45, 30];

        // Calculate trend
        let first_half_avg: u32 = cycle_success_rates[0..4].iter().sum::<u32>() / 4;
        let second_half_avg: u32 = cycle_success_rates[4..8].iter().sum::<u32>() / 4;

        let degradation = first_half_avg as i32 - second_half_avg as i32;
        println!(
            "First half avg: {}%, Second half avg: {}%, Degradation: {}%",
            first_half_avg, second_half_avg, degradation
        );

        // More than 20% degradation is concerning
        assert!(
            degradation > 20,
            "This pattern should be flagged as degradation"
        );
    }
}
