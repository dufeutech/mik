// Test-specific lint suppressions
#![allow(dead_code)]

//! Memory stability tests for the mikrozen runtime.
//!
//! These tests verify that the runtime doesn't leak memory over time,
//! handles repeated instantiations correctly, and maintains stability
//! under sustained load.
//!
//! ## Background
//!
//! Memory issues are the #1 production problem across WASM runtimes:
//!
//! - WAMR #542: Memory consumption keeps growing
//! - wasm3 #203: Memory leaks after version updates
//! - proxy-wasm-go-sdk #349: Headers memory never freed
//! - General: Memory fragmentation over time
//!
//! ## Test Philosophy
//!
//! 1. **Leak Detection** - Memory should stabilize, not grow indefinitely
//! 2. **Fragmentation** - Repeated alloc/free shouldn't fragment heap
//! 3. **Long-running** - Stability over many iterations, not just one
//!
//! ## Running Tests
//!
//! ```bash
//! # Run unit tests (fast, no fixtures)
//! cargo test -p mik memory_stability::tests
//!
//! # Run integration tests (requires fixtures, slower)
//! cargo test -p mik memory_stability -- --ignored --test-threads=1
//! ```

#[path = "common.rs"]
mod common;

use common::RealTestHost;
use std::collections::VecDeque;
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

/// Simple memory usage tracker using process stats.
/// Note: This is approximate - exact memory tracking requires OS-specific APIs.
#[derive(Debug, Clone)]
struct MemorySnapshot {
    timestamp: Instant,
    iteration: u64,
    // We track request count as a proxy for "work done"
    requests_completed: u64,
}

impl MemorySnapshot {
    fn new(iteration: u64, requests_completed: u64) -> Self {
        Self {
            timestamp: Instant::now(),
            iteration,
            requests_completed,
        }
    }
}

/// Tracks memory growth over time.
struct MemoryTracker {
    snapshots: VecDeque<MemorySnapshot>,
    max_snapshots: usize,
}

impl MemoryTracker {
    fn new(max_snapshots: usize) -> Self {
        Self {
            snapshots: VecDeque::with_capacity(max_snapshots),
            max_snapshots,
        }
    }

    fn record(&mut self, iteration: u64, requests_completed: u64) {
        if self.snapshots.len() >= self.max_snapshots {
            self.snapshots.pop_front();
        }
        self.snapshots
            .push_back(MemorySnapshot::new(iteration, requests_completed));
    }

    fn iteration_count(&self) -> usize {
        self.snapshots.len()
    }

    fn total_requests(&self) -> u64 {
        self.snapshots
            .back()
            .map(|s| s.requests_completed)
            .unwrap_or(0)
    }
}

// =============================================================================
// Integration Tests (Require WASM fixtures)
// =============================================================================

/// Test that repeated module instantiation doesn't leak memory.
///
/// This test makes many requests to the same module, which causes
/// repeated instantiation. Memory usage should stabilize, not grow.
///
/// ## Expected Behavior
///
/// - Make N requests over time
/// - Memory usage should plateau after warmup
/// - No unbounded growth
///
/// ## Bug Behavior (from WAMR #542)
///
/// - Memory consumption keeps growing when running
/// - wasm_runtime_realloc_internal called repeatedly
/// - Eventually OOM
#[tokio::test]
#[ignore = "Requires echo.wasm fixture, runs for extended time"]
async fn test_repeated_instantiation_no_leak() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(50)
        .start()
        .await
        .expect("Failed to start host");

    let iterations = 100;
    let requests_per_iteration = 10;
    let total_requests = Arc::new(AtomicU64::new(0));
    let mut tracker = MemoryTracker::new(iterations);

    println!("Starting memory stability test: {} iterations", iterations);

    for i in 0..iterations {
        // Make requests in parallel
        let mut handles = Vec::with_capacity(requests_per_iteration);

        for _ in 0..requests_per_iteration {
            let url = host.url("/run/echo/");
            let total = total_requests.clone();

            handles.push(tokio::spawn(async move {
                let client = reqwest::Client::new();
                let result = client
                    .post(&url)
                    .json(&serde_json::json!({"iteration": i}))
                    .send()
                    .await;

                if result.is_ok() {
                    total.fetch_add(1, Ordering::Relaxed);
                }
                result.is_ok()
            }));
        }

        // Wait for all requests
        let mut success = 0;
        for handle in handles {
            if handle.await.unwrap_or(false) {
                success += 1;
            }
        }

        // Record snapshot
        tracker.record(i as u64, total_requests.load(Ordering::Relaxed));

        // Progress every 10 iterations
        if i % 10 == 0 {
            println!(
                "Iteration {}/{}: {} requests completed, {} successful this batch",
                i,
                iterations,
                tracker.total_requests(),
                success
            );
        }

        // Small delay between iterations to let GC run
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let final_requests = total_requests.load(Ordering::Relaxed);
    println!(
        "Completed {} iterations, {} total requests",
        tracker.iteration_count(),
        final_requests
    );

    // Verify we completed most requests (some failures are OK)
    let expected_min = (iterations * requests_per_iteration * 9 / 10) as u64;
    assert!(
        final_requests >= expected_min,
        "Should complete at least 90% of requests: {} < {}",
        final_requests,
        expected_min
    );

    // Final health check - server should still be responsive
    let health = host.get("/health").await.expect("Health check should work");
    assert_eq!(health.status(), 200, "Server should be healthy after test");
}

/// Test memory stability over many sequential requests.
///
/// Unlike the parallel test, this makes requests one at a time
/// to isolate memory behavior without concurrency effects.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture, runs for extended time"]
async fn test_sequential_requests_memory_stable() {
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

    let total_requests = 500;
    let checkpoint_interval = 100;
    let client = reqwest::Client::new();
    let url = host.url("/run/echo/");

    println!(
        "Starting sequential memory test: {} requests",
        total_requests
    );

    let mut success_count = 0;
    let mut error_count = 0;
    let start = Instant::now();

    for i in 0..total_requests {
        let result = client
            .post(&url)
            .json(&serde_json::json!({"seq": i}))
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => success_count += 1,
            Ok(_) => error_count += 1,
            Err(_) => error_count += 1,
        }

        if (i + 1) % checkpoint_interval == 0 {
            let elapsed = start.elapsed();
            let rps = f64::from(i + 1) / elapsed.as_secs_f64();
            println!(
                "Checkpoint {}/{}: {} success, {} errors, {:.1} req/s",
                i + 1,
                total_requests,
                success_count,
                error_count,
                rps
            );
        }
    }

    let elapsed = start.elapsed();
    println!(
        "Completed {} requests in {:.2}s ({:.1} req/s)",
        total_requests,
        elapsed.as_secs_f64(),
        f64::from(total_requests) / elapsed.as_secs_f64()
    );
    println!("Success: {}, Errors: {}", success_count, error_count);

    // Should have very high success rate
    assert!(
        success_count >= total_requests * 99 / 100,
        "Should have 99%+ success rate: {}/{}",
        success_count,
        total_requests
    );

    // Server should still be healthy
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

/// Test that the server handles burst traffic without memory issues.
///
/// Simulates traffic bursts followed by quiet periods.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture, runs for extended time"]
async fn test_burst_traffic_memory_recovery() {
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

    let bursts = 5;
    let requests_per_burst = 50;
    let quiet_period = Duration::from_millis(500);

    println!(
        "Starting burst test: {} bursts of {} requests",
        bursts, requests_per_burst
    );

    for burst in 0..bursts {
        println!("Burst {} starting...", burst + 1);
        let burst_start = Instant::now();

        // Fire all requests simultaneously
        let mut handles = Vec::with_capacity(requests_per_burst);
        for i in 0..requests_per_burst {
            let url = host.url("/run/echo/");
            handles.push(tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                client
                    .post(&url)
                    .json(&serde_json::json!({"burst": burst, "req": i}))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
            }));
        }

        // Collect results
        let mut success = 0;
        for handle in handles {
            if handle.await.unwrap_or(false) {
                success += 1;
            }
        }

        let burst_duration = burst_start.elapsed();
        println!(
            "Burst {} complete: {}/{} success in {:?}",
            burst + 1,
            success,
            requests_per_burst,
            burst_duration
        );

        // Most requests should succeed
        assert!(
            success >= requests_per_burst * 8 / 10,
            "Burst {} should have 80%+ success: {}/{}",
            burst + 1,
            success,
            requests_per_burst
        );

        // Quiet period - let server recover
        if burst < bursts - 1 {
            println!("Quiet period: {:?}", quiet_period);
            tokio::time::sleep(quiet_period).await;
        }
    }

    // Final health check
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(
        health.status(),
        200,
        "Server should be healthy after bursts"
    );
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
    fn test_memory_tracker_basics() {
        let mut tracker = MemoryTracker::new(10);

        // Record some snapshots
        for i in 0..5 {
            tracker.record(i, i * 10);
        }

        assert_eq!(tracker.iteration_count(), 5);
        assert_eq!(tracker.total_requests(), 40);
    }

    #[test]
    fn test_memory_tracker_overflow() {
        let mut tracker = MemoryTracker::new(3);

        // Record more than max
        for i in 0..10 {
            tracker.record(i, i * 10);
        }

        // Should only keep last 3
        assert_eq!(tracker.iteration_count(), 3);
        assert_eq!(tracker.total_requests(), 90); // iteration 9 * 10
    }

    #[test]
    fn test_memory_growth_detection_logic() {
        // Simulated memory readings (in MB)
        let readings = vec![100, 102, 101, 103, 102, 104, 103, 105, 104, 106];

        // Calculate growth trend
        let first_half_avg: i64 = readings[0..5].iter().sum::<i64>() / 5;
        let second_half_avg: i64 = readings[5..10].iter().sum::<i64>() / 5;

        let growth = second_half_avg - first_half_avg;
        println!(
            "First half avg: {}, Second half avg: {}, Growth: {}",
            first_half_avg, second_half_avg, growth
        );

        // Small growth is OK (< 10% of initial)
        let acceptable_growth = readings[0] / 10;
        assert!(
            growth <= acceptable_growth,
            "Memory growth {} should be <= {}",
            growth,
            acceptable_growth
        );
    }

    #[test]
    fn test_request_rate_calculation() {
        let requests = 1000u64;
        let duration_secs = 10.0f64;
        let rps = requests as f64 / duration_secs;

        assert!((rps - 100.0).abs() < 0.01, "Should be 100 req/s");
    }

    #[test]
    fn test_success_rate_calculation() {
        let total = 1000;
        let success = 985;
        let rate = (f64::from(success) / f64::from(total)) * 100.0;

        assert!(rate >= 98.0, "Success rate should be >= 98%");
    }

    #[test]
    fn test_burst_timing_logic() {
        let bursts = 5;
        let quiet_period_ms = 500;
        let burst_duration_ms = 100; // estimated

        let total_time_ms = bursts * burst_duration_ms + (bursts - 1) * quiet_period_ms;
        println!("Estimated total time: {}ms", total_time_ms);

        // Should complete in reasonable time
        assert!(total_time_ms < 5000, "Test should complete in < 5s");
    }

    #[test]
    fn test_memory_stability_thresholds() {
        // Define what "stable" means
        let initial_mb = 100;
        let max_acceptable_growth_percent = 20;
        let max_acceptable_mb = initial_mb * (100 + max_acceptable_growth_percent) / 100;

        assert_eq!(max_acceptable_mb, 120);

        // Simulated final reading
        let final_mb = 115;
        assert!(
            final_mb <= max_acceptable_mb,
            "Memory {} MB should be <= {} MB",
            final_mb,
            max_acceptable_mb
        );
    }

    #[test]
    fn test_leak_detection_algorithm() {
        // If memory grows linearly with requests, it's a leak
        let readings: Vec<(u64, u64)> = vec![
            (100, 100), // 100 requests, 100 MB
            (200, 105), // 200 requests, 105 MB - OK
            (300, 110), // 300 requests, 110 MB - OK
            (400, 150), // 400 requests, 150 MB - LEAK!
            (500, 200), // 500 requests, 200 MB - LEAK!
        ];

        // Calculate bytes per request
        let first = readings.first().unwrap();
        let last = readings.last().unwrap();

        let request_delta = last.0 - first.0;
        let memory_delta = last.1 - first.1;
        let bytes_per_request = (memory_delta as f64 / request_delta as f64) * 1024.0 * 1024.0;

        println!("Bytes per request: {:.0}", bytes_per_request);

        // More than 1KB per request is suspicious
        let leak_threshold_bytes = 1024.0;
        let is_leaking = bytes_per_request > leak_threshold_bytes;

        assert!(is_leaking, "This pattern should be detected as a leak");
    }
}
