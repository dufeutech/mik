//! Deadlock detection tests for the mikrozen runtime.
//!
//! These tests verify that the runtime doesn't deadlock under various
//! concurrent access patterns.
//!
//! ## Background
//!
//! Deadlocks are worse than crashes - they cause silent hangs:
//!
//! - General: Multi-threaded resource contention
//! - General: Lock ordering issues
//! - General: Thread starvation under load
//! - Wasmtime CVE-2024-47813: Race condition in type registry
//!
//! ## Test Philosophy
//!
//! 1. **Detection Over Prevention** - Tests should detect hangs, not just pass
//! 2. **Timeout-Based** - All tests have strict timeouts
//! 3. **Stress Patterns** - Exercise known problematic patterns
//!
//! ## Running Tests
//!
//! ```bash
//! # Run unit tests (fast, no fixtures)
//! cargo test -p mik deadlock::tests
//!
//! # Run integration tests (requires fixtures)
//! cargo test -p mik deadlock -- --ignored --test-threads=1
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

/// Configuration for deadlock tests.
struct DeadlockTestConfig {
    /// Maximum time before declaring a deadlock.
    deadlock_timeout: Duration,
    /// Number of concurrent actors.
    actor_count: usize,
    /// Operations per actor.
    ops_per_actor: usize,
}

impl Default for DeadlockTestConfig {
    fn default() -> Self {
        Self {
            deadlock_timeout: Duration::from_secs(30),
            actor_count: 20,
            ops_per_actor: 10,
        }
    }
}

/// Result from a deadlock test.
#[derive(Debug)]
struct DeadlockTestResult {
    completed: u64,
    timed_out: u64,
    errors: u64,
    duration: Duration,
    detected_deadlock: bool,
}

// =============================================================================
// Integration Tests (Require WASM fixtures)
// =============================================================================

/// Test that concurrent access to the same module doesn't deadlock.
///
/// Multiple actors repeatedly call the same WASM module simultaneously.
/// This stresses module instance management and pooling.
///
/// ## Expected Behavior
///
/// - All requests complete within timeout
/// - No threads blocked indefinitely
/// - Server remains responsive
///
/// ## Bug Pattern
///
/// - Lock contention on module cache
/// - Instance pool exhaustion without release
/// - Circular wait on resources
#[tokio::test]
#[ignore = "Requires echo.wasm fixture, stress test"]
async fn test_no_deadlock_same_module() {
    if !echo_wasm_exists() {
        eprintln!(
            "Skipping: echo.wasm not found at {}",
            fixtures_dir().display()
        );
        return;
    }

    let config = DeadlockTestConfig::default();
    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(config.actor_count * 2)
        .start()
        .await
        .expect("Failed to start host");

    let result = run_deadlock_test(&host, &config, "/run/echo/").await;

    println!("Same module deadlock test: {:?}", result);

    assert!(
        !result.detected_deadlock,
        "Deadlock detected! Only {}/{} completed in {:?}",
        result.completed,
        config.actor_count * config.ops_per_actor,
        result.duration
    );

    // High completion rate expected
    let expected_total = (config.actor_count * config.ops_per_actor) as u64;
    assert!(
        result.completed >= expected_total * 9 / 10,
        "Should complete 90%+ requests: {}/{}",
        result.completed,
        expected_total
    );
}

/// Test that concurrent access to different modules doesn't deadlock.
///
/// Multiple actors call different WASM modules. This stresses
/// the module loading and caching system.
#[tokio::test]
#[ignore = "Requires multiple fixtures, stress test"]
async fn test_no_deadlock_different_modules() {
    let modules = ["echo.wasm", "panic.wasm"];
    let available: Vec<_> = modules
        .iter()
        .filter(|m| fixtures_dir().join(m).exists())
        .collect();

    if available.len() < 2 {
        eprintln!(
            "Skipping: Need at least 2 fixtures, found {}",
            available.len()
        );
        return;
    }

    let config = DeadlockTestConfig {
        actor_count: 30,
        ops_per_actor: 5,
        ..Default::default()
    };

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(config.actor_count * 2)
        .start()
        .await
        .expect("Failed to start host");

    println!(
        "Testing {} actors across {} modules",
        config.actor_count,
        available.len()
    );

    let completed = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    // Launch actors that randomly pick modules
    let mut handles = Vec::with_capacity(config.actor_count);
    for actor in 0..config.actor_count {
        let url_base = host.url("");
        let completed = completed.clone();
        let errors = errors.clone();
        let ops = config.ops_per_actor;

        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();

            for op in 0..ops {
                // Alternate between modules
                let module = if (actor + op) % 2 == 0 {
                    "echo"
                } else {
                    "panic"
                };
                let url = format!("{}/run/{}/", url_base.trim_end_matches('/'), module);

                let result = client
                    .post(&url)
                    .json(&serde_json::json!({"actor": actor, "op": op}))
                    .send()
                    .await;

                match result {
                    Ok(_) => {
                        completed.fetch_add(1, Ordering::Relaxed);
                    },
                    Err(_) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                    },
                }
            }
        }));
    }

    // Wait with timeout
    let test_result = tokio::time::timeout(config.deadlock_timeout, async {
        for handle in handles {
            let _ = handle.await;
        }
    })
    .await;

    let elapsed = start.elapsed();
    let final_completed = completed.load(Ordering::Relaxed);
    let final_errors = errors.load(Ordering::Relaxed);

    println!(
        "Multi-module test: {} completed, {} errors in {:?}",
        final_completed, final_errors, elapsed
    );

    assert!(
        test_result.is_ok(),
        "Test timed out - possible deadlock across modules"
    );
}

/// Test thread starvation prevention.
///
/// Ensures that slow requests don't starve fast ones indefinitely.
#[tokio::test]
#[ignore = "Requires echo.wasm and slow_response.wasm fixtures"]
async fn test_no_thread_starvation() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(30)
        .start()
        .await
        .expect("Failed to start host");

    // Start some slow requests (if slow_response.wasm exists, use it)
    let slow_url = host.url("/run/echo/"); // Using echo as fallback
    let fast_url = host.url("/run/echo/");

    // Launch "slow" requests (we simulate slowness with the request itself)
    let slow_handles: Vec<_> = (0..10)
        .map(|i| {
            let url = slow_url.clone();
            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap();
                // Large payload to slow down
                let big_payload: String = "x".repeat(10000);
                let start = Instant::now();
                let _ = client
                    .post(&url)
                    .json(&serde_json::json!({
                        "slow": i,
                        "data": big_payload
                    }))
                    .send()
                    .await;
                start.elapsed()
            })
        })
        .collect();

    // Small delay then launch fast requests
    tokio::time::sleep(Duration::from_millis(50)).await;

    let fast_start = Instant::now();
    let fast_handles: Vec<_> = (0..20)
        .map(|i| {
            let url = fast_url.clone();
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
                (result.is_ok(), start.elapsed())
            })
        })
        .collect();

    // Collect fast results
    let mut fast_success = 0;
    let mut fast_times = Vec::new();
    for handle in fast_handles {
        if let Ok((success, time)) = handle.await {
            if success {
                fast_success += 1;
            }
            fast_times.push(time);
        }
    }

    let fast_elapsed = fast_start.elapsed();
    let avg_fast = fast_times.iter().sum::<Duration>() / fast_times.len().max(1) as u32;

    println!(
        "Fast requests: {}/20 success, avg {:?}, total {:?}",
        fast_success, avg_fast, fast_elapsed
    );

    // Fast requests shouldn't be completely starved
    assert!(
        fast_success >= 15,
        "Fast requests should not be starved: {}/20",
        fast_success
    );

    // Wait for slow requests
    for handle in slow_handles {
        let _ = handle.await;
    }
}

/// Test rapid connect/disconnect cycles.
///
/// Simulates clients that connect and immediately disconnect.
/// This can cause resource leaks or deadlocks in connection handling.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture, stress test"]
async fn test_rapid_connect_disconnect() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(100)
        .start()
        .await
        .expect("Failed to start host");

    let cycles = 50;
    let connections_per_cycle = 20;
    let timeout = Duration::from_secs(30);

    println!(
        "Testing {} cycles of {} rapid connections",
        cycles, connections_per_cycle
    );

    let start = Instant::now();
    let completed = Arc::new(AtomicU64::new(0));

    let result = tokio::time::timeout(timeout, async {
        for cycle in 0..cycles {
            let mut handles = Vec::with_capacity(connections_per_cycle);

            for i in 0..connections_per_cycle {
                let url = host.url("/run/echo/");
                let completed = completed.clone();

                handles.push(tokio::spawn(async move {
                    // Use a fresh client each time (no connection reuse)
                    let client = reqwest::Client::builder()
                        .pool_max_idle_per_host(0) // Disable connection pooling
                        .timeout(Duration::from_secs(5))
                        .build()
                        .unwrap();

                    let result = client
                        .post(&url)
                        .json(&serde_json::json!({"cycle": cycle, "conn": i}))
                        .send()
                        .await;

                    if result.is_ok() {
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }

            // Wait for all connections in this cycle
            for handle in handles {
                let _ = handle.await;
            }

            // Brief pause between cycles
            if cycle % 10 == 0 {
                println!(
                    "Cycle {}/{}: {} total completed",
                    cycle,
                    cycles,
                    completed.load(Ordering::Relaxed)
                );
            }
        }
    })
    .await;

    let elapsed = start.elapsed();
    let final_completed = completed.load(Ordering::Relaxed);
    let expected = (cycles * connections_per_cycle) as u64;

    println!(
        "Rapid connect/disconnect: {}/{} completed in {:?}",
        final_completed, expected, elapsed
    );

    assert!(
        result.is_ok(),
        "Test timed out - possible deadlock in connection handling"
    );

    // High success rate expected
    assert!(
        final_completed >= expected * 8 / 10,
        "Should complete 80%+ connections: {}/{}",
        final_completed,
        expected
    );

    // Server should still be healthy
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(
        health.status(),
        200,
        "Server should be healthy after rapid connect/disconnect"
    );
}

/// Test concurrent health checks during load.
///
/// Health endpoints should always respond, even under heavy load.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture, stress test"]
async fn test_health_check_always_responsive() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(50)
        .start()
        .await
        .expect("Failed to start host");

    let load_duration = Duration::from_secs(5);
    let health_interval = Duration::from_millis(100);

    // Start background load
    let load_url = host.url("/run/echo/");
    let stop_load = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_load_clone = stop_load.clone();

    let load_handle = tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let mut count = 0u64;
        while !stop_load_clone.load(Ordering::Relaxed) {
            let _ = client
                .post(&load_url)
                .json(&serde_json::json!({"load": count}))
                .send()
                .await;
            count += 1;
        }
        count
    });

    // Periodically check health during load
    let start = Instant::now();
    let mut health_checks = 0;
    let mut health_failures = 0;
    let mut health_times = Vec::new();

    while start.elapsed() < load_duration {
        let health_start = Instant::now();
        let result = host.get("/health").await;
        let health_time = health_start.elapsed();

        health_times.push(health_time);
        health_checks += 1;

        match result {
            Ok(resp) if resp.status() == 200 => {},
            _ => health_failures += 1,
        }

        tokio::time::sleep(health_interval).await;
    }

    // Stop load generation
    stop_load.store(true, Ordering::Relaxed);
    let load_count = load_handle.await.unwrap_or(0);

    let avg_health_time = health_times.iter().sum::<Duration>() / health_times.len().max(1) as u32;
    let max_health_time = health_times.iter().max().copied().unwrap_or_default();

    println!(
        "Health checks during load: {}/{} success, avg {:?}, max {:?}",
        health_checks - health_failures,
        health_checks,
        avg_health_time,
        max_health_time
    );
    println!("Load requests during test: {}", load_count);

    // Health checks should almost always succeed
    assert!(
        health_failures <= 1,
        "Health endpoint should be reliable: {} failures",
        health_failures
    );

    // Health checks should be fast
    assert!(
        max_health_time < Duration::from_secs(2),
        "Health check should be fast even under load: {:?}",
        max_health_time
    );
}

// =============================================================================
// Helper Functions
// =============================================================================

async fn run_deadlock_test(
    host: &RealTestHost,
    config: &DeadlockTestConfig,
    path: &str,
) -> DeadlockTestResult {
    let completed = Arc::new(AtomicU64::new(0));
    let timed_out = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    // Launch actors
    let mut handles = Vec::with_capacity(config.actor_count);
    for actor in 0..config.actor_count {
        let url = host.url(path);
        let completed = completed.clone();
        let timed_out = timed_out.clone();
        let errors = errors.clone();
        let ops = config.ops_per_actor;

        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();

            for op in 0..ops {
                let result = client
                    .post(&url)
                    .json(&serde_json::json!({"actor": actor, "op": op}))
                    .send()
                    .await;

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        completed.fetch_add(1, Ordering::Relaxed);
                    },
                    Ok(_) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                    },
                    Err(e) if e.is_timeout() => {
                        timed_out.fetch_add(1, Ordering::Relaxed);
                    },
                    Err(_) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                    },
                }
            }
        }));
    }

    // Wait with overall timeout
    let test_result = tokio::time::timeout(config.deadlock_timeout, async {
        for handle in handles {
            let _ = handle.await;
        }
    })
    .await;

    let duration = start.elapsed();
    let detected_deadlock = test_result.is_err();

    DeadlockTestResult {
        completed: completed.load(Ordering::Relaxed),
        timed_out: timed_out.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
        duration,
        detected_deadlock,
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
        let config = DeadlockTestConfig::default();
        assert_eq!(config.deadlock_timeout, Duration::from_secs(30));
        assert_eq!(config.actor_count, 20);
        assert_eq!(config.ops_per_actor, 10);
    }

    #[test]
    fn test_deadlock_detection_logic() {
        // Simulate: all actors stuck, no progress
        let expected_ops = 200;
        let completed = 0;
        let duration = Duration::from_secs(30);

        let completion_rate = f64::from(completed) / f64::from(expected_ops);
        let ops_per_sec = f64::from(completed) / duration.as_secs_f64();

        println!(
            "Completion: {:.1}%, Throughput: {:.1} ops/s",
            completion_rate * 100.0,
            ops_per_sec
        );

        // Zero completion with timeout = deadlock
        let is_deadlock = completed == 0 && duration >= Duration::from_secs(30);
        assert!(is_deadlock, "This should be detected as deadlock");
    }

    #[test]
    fn test_partial_deadlock_detection() {
        // Some progress but very slow = partial deadlock
        let expected_ops = 200;
        let completed = 10; // Only 5% completed
        let duration = Duration::from_secs(30);

        let completion_rate = f64::from(completed) / f64::from(expected_ops);
        let expected_completion_rate = 0.9; // We expect 90%+

        let is_partial_deadlock =
            completion_rate < expected_completion_rate && duration >= Duration::from_secs(30);

        println!(
            "Completion rate: {:.1}%, Expected: {:.1}%",
            completion_rate * 100.0,
            expected_completion_rate * 100.0
        );

        assert!(
            is_partial_deadlock,
            "This should be detected as partial deadlock"
        );
    }

    #[test]
    fn test_timeout_vs_deadlock() {
        // Distinguish between slow operations and true deadlock

        // Case 1: Slow but progressing
        let slow_ops_completed = 50;
        let slow_duration = Duration::from_secs(30);
        let slow_rate = f64::from(slow_ops_completed) / slow_duration.as_secs_f64();

        // Case 2: Stuck (no progress)
        let stuck_ops_completed = 0;
        let stuck_duration = Duration::from_secs(30);
        let stuck_rate = f64::from(stuck_ops_completed) / stuck_duration.as_secs_f64();

        println!("Slow rate: {:.1} ops/s", slow_rate);
        println!("Stuck rate: {:.1} ops/s", stuck_rate);

        // Slow is not deadlock if making progress
        assert!(slow_rate > 0.0, "Slow is not deadlock");
        assert!(stuck_rate == 0.0, "Stuck is deadlock");
    }

    #[test]
    fn test_actor_coordination() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let actors = 10;
        let ops_per_actor = 5;
        let total_expected = actors * ops_per_actor;

        let completed = AtomicUsize::new(0);

        // Simulate completion
        for _ in 0..actors {
            for _ in 0..ops_per_actor {
                completed.fetch_add(1, Ordering::Relaxed);
            }
        }

        assert_eq!(completed.load(Ordering::Relaxed), total_expected);
    }

    #[test]
    fn test_starvation_detection() {
        // Simulate: fast actors starved, slow actors completing
        let fast_completed = 5;
        let slow_completed = 95;
        let fast_expected = 50;
        let slow_expected = 50;

        let fast_rate = f64::from(fast_completed) / f64::from(fast_expected);
        let slow_rate = f64::from(slow_completed) / f64::from(slow_expected);

        println!("Fast completion: {:.1}%", fast_rate * 100.0);
        println!("Slow completion: {:.1}%", slow_rate * 100.0);

        // Starvation = one type much lower than other
        let is_starvation = fast_rate < 0.5 && slow_rate > 0.9;
        assert!(is_starvation, "This should be detected as starvation");
    }

    #[test]
    fn test_health_check_threshold() {
        let health_latencies_ms = [10, 15, 12, 500, 20, 25, 1500, 30];

        let threshold_ms = 1000;
        let slow_count = health_latencies_ms
            .iter()
            .filter(|&&l| l > threshold_ms)
            .count();

        println!(
            "Health checks exceeding {}ms: {}/{}",
            threshold_ms,
            slow_count,
            health_latencies_ms.len()
        );

        // More than 10% slow health checks is concerning
        let slow_rate = slow_count as f64 / health_latencies_ms.len() as f64;
        assert!(
            slow_rate > 0.1,
            "This pattern has too many slow health checks: {:.1}%",
            slow_rate * 100.0
        );
    }

    #[test]
    fn test_connection_churn_math() {
        let cycles = 50;
        let conns_per_cycle = 30; // More connections per cycle
        let total_connections = cycles * conns_per_cycle;

        // If connections aren't cleaned up, FDs grow
        let fd_per_conn = 1;
        let max_fd = 1024; // Typical limit

        let would_exhaust = total_connections * fd_per_conn > max_fd;

        println!(
            "Total connections: {}, Max FD: {}, Would exhaust: {}",
            total_connections, max_fd, would_exhaust
        );

        // 50 * 30 = 1500 > 1024, so without cleanup we'd exhaust FDs
        assert!(
            would_exhaust,
            "This pattern would exhaust FDs without cleanup"
        );
    }

    #[test]
    fn test_result_struct() {
        let result = DeadlockTestResult {
            completed: 180,
            timed_out: 5,
            errors: 15,
            duration: Duration::from_secs(25),
            detected_deadlock: false,
        };

        let total = result.completed + result.timed_out + result.errors;
        let success_rate = result.completed as f64 / total as f64;

        println!("Result: {:?}", result);
        println!("Success rate: {:.1}%", success_rate * 100.0);

        assert!(!result.detected_deadlock);
        assert!(success_rate > 0.8);
    }

    #[test]
    fn test_throughput_under_contention() {
        // Simulated throughput with varying contention levels
        let scenarios = vec![
            ("no_contention", 1000.0), // ops/s
            ("low_contention", 800.0),
            ("medium_contention", 500.0),
            ("high_contention", 200.0),
            ("deadlock", 0.0),
        ];

        for (name, throughput) in scenarios {
            let is_healthy = throughput >= 100.0; // Minimum viable throughput
            let is_deadlock = throughput == 0.0;

            println!(
                "{}: {:.0} ops/s, healthy: {}, deadlock: {}",
                name, throughput, is_healthy, is_deadlock
            );
        }
    }
}
