//! WASM integration tests using RealTestHost.
//!
//! These tests run against the actual mik runtime with real WASM module execution.
//! They test:
//! - Module loading and execution (echo.wasm)
//! - Panic handling and resilience (panic.wasm)
//! - Timeout/epoch interruption (infinite_loop.wasm)
//! - Memory limits (memory_hog.wasm)
//! - Fuel metering (fuel_burner.wasm)

use std::path::PathBuf;
use std::time::{Duration, Instant};

mod common;
use common::RealTestHost;

/// Get the path to the test fixtures directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("modules")
}

/// Check if a specific fixture exists.
fn fixture_exists(name: &str) -> bool {
    fixtures_dir().join(name).exists()
}

/// Helper macro to skip tests if fixtures are missing.
macro_rules! require_fixture {
    ($name:expr) => {
        if !fixture_exists($name) {
            eprintln!("Skipping: {} not found. Run build script first.", $name);
            return;
        }
    };
}

// =============================================================================
// Echo Module Tests
// =============================================================================

#[tokio::test]
async fn test_echo_module_basic() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let resp = host
        .post_json("/run/echo/", &serde_json::json!({"message": "hello"}))
        .await
        .expect("Request failed");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["message"], "hello");
}

#[tokio::test]
async fn test_echo_module_complex_json() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let input = serde_json::json!({
        "string": "hello",
        "number": 42,
        "float": 1.234,
        "boolean": true,
        "null": null,
        "array": [1, 2, 3],
        "nested": {
            "a": "b",
            "c": [4, 5, 6]
        }
    });

    let resp = host
        .post_json("/run/echo/", &input)
        .await
        .expect("Request failed");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body, input);
}

#[tokio::test]
async fn test_echo_module_empty_body() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let resp = host
        .post_json("/run/echo/", &serde_json::json!({}))
        .await
        .expect("Request failed");

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_echo_module_concurrent_requests() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(10)
        .start()
        .await
        .expect("Failed to start host");

    let mut handles = Vec::new();

    for i in 0..5 {
        let url = host.url("/run/echo/");
        let client = host.client().clone();
        let body = serde_json::json!({"id": i});

        handles.push(tokio::spawn(async move {
            client.post(&url).json(&body).send().await
        }));
    }

    let results = futures::future::join_all(handles).await;

    for result in results {
        let resp = result.expect("Task panicked").expect("Request failed");
        assert_eq!(resp.status(), 200);
    }
}

// =============================================================================
// Panic Module Tests (Resilience)
// =============================================================================

#[tokio::test]
async fn test_panic_module_returns_error() {
    require_fixture!("panic.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let result = host.post_json("/run/panic/", &serde_json::json!({})).await;

    // Panic can either return 5xx OR close the connection
    // Either is acceptable - the module failed and server is still running
    match result {
        Ok(resp) => {
            assert!(
                resp.status().is_server_error(),
                "Panic should return 5xx, got {}",
                resp.status()
            );
        },
        Err(_) => {
            // Connection closed is acceptable for panics
            // The important thing is the server is still running
        },
    }

    // Verify server is still running
    let health = host
        .get("/health")
        .await
        .expect("Server should still be running");
    assert_eq!(health.status(), 200);
}

#[tokio::test]
async fn test_panic_doesnt_affect_other_modules() {
    require_fixture!("panic.wasm");
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    // First, cause a panic - may return 5xx or close connection
    let _ = host.post_json("/run/panic/", &serde_json::json!({})).await;

    // Give server a moment to recover
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Then, verify echo still works - this is the important part
    let echo_resp = host
        .post_json("/run/echo/", &serde_json::json!({"test": "after_panic"}))
        .await
        .expect("Echo should still work after panic in other module");

    assert_eq!(echo_resp.status(), 200);

    let body: serde_json::Value = echo_resp.json().await.expect("Failed to parse");
    assert_eq!(body["test"], "after_panic");
}

// =============================================================================
// Infinite Loop Tests (Timeout/Epoch Interruption)
// =============================================================================

#[tokio::test]
async fn test_infinite_loop_is_interrupted() {
    require_fixture!("infinite_loop.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_execution_timeout(2) // 2 second timeout
        .start()
        .await
        .expect("Failed to start host");

    let start = Instant::now();

    let result = host
        .post_json("/run/infinite_loop/", &serde_json::json!({}))
        .await;

    let elapsed = start.elapsed();

    // Should return error (timeout) or close connection
    match result {
        Ok(resp) => {
            assert!(
                resp.status().is_server_error(),
                "Infinite loop should timeout, got {}",
                resp.status()
            );
        },
        Err(_) => {
            // Connection closed is acceptable - epoch interruption may close connection
        },
    }

    // Should complete within reasonable time (not hang forever)
    assert!(
        elapsed < Duration::from_secs(30),
        "Should complete within 30s, took {:?}",
        elapsed
    );

    // Verify server is still running
    let health = host
        .get("/health")
        .await
        .expect("Server should still be running");
    assert_eq!(health.status(), 200);
}

#[tokio::test]
async fn test_timeout_doesnt_block_other_requests() {
    require_fixture!("infinite_loop.wasm");
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_execution_timeout(2)
        .with_max_concurrent_requests(10) // Must be >= default max_per_module_requests (10)
        .start()
        .await
        .expect("Failed to start host");

    // Start an infinite loop request in the background
    let url_loop = host.url("/run/infinite_loop/");
    let client = host.client().clone();
    let loop_handle = tokio::spawn(async move {
        client
            .post(&url_loop)
            .json(&serde_json::json!({}))
            .send()
            .await
    });

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Meanwhile, echo should still work - this is the key assertion
    let echo_resp = host
        .post_json("/run/echo/", &serde_json::json!({"test": "concurrent"}))
        .await
        .expect("Echo request failed while infinite loop running");

    assert_eq!(echo_resp.status(), 200);

    // Wait for the loop to timeout (result may be error or connection close)
    let _ = loop_handle.await.expect("Task panicked");
}

// =============================================================================
// Memory Hog Tests (ResourceLimiter)
// =============================================================================

#[tokio::test]
async fn test_memory_hog_is_limited() {
    require_fixture!("memory_hog.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_memory_limit_mb(32) // 32MB limit
        .with_execution_timeout(10)
        .start()
        .await
        .expect("Failed to start host");

    let result = host
        .post_json("/run/memory_hog/", &serde_json::json!({}))
        .await;

    // Should return error (memory limit exceeded) or close connection (trap)
    match result {
        Ok(resp) => {
            assert!(
                resp.status().is_server_error(),
                "Memory hog should fail, got {}",
                resp.status()
            );
        },
        Err(_) => {
            // Connection closed is acceptable - memory trap may close connection
        },
    }

    // Verify server is still running
    let health = host
        .get("/health")
        .await
        .expect("Server should still be running");
    assert_eq!(health.status(), 200);
}

// =============================================================================
// Fuel Burner Tests (CPU Limiting)
// =============================================================================

#[tokio::test]
async fn test_fuel_burner_exhausts_fuel() {
    require_fixture!("fuel_burner.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_execution_timeout(10) // Long timeout so fuel runs out first
        .start()
        .await
        .expect("Failed to start host");

    let start = Instant::now();

    let result = host
        .post_json("/run/fuel_burner/", &serde_json::json!({}))
        .await;

    let elapsed = start.elapsed();

    // Should return error (fuel exhausted) or close connection (trap)
    match result {
        Ok(resp) => {
            assert!(
                resp.status().is_server_error(),
                "Fuel burner should fail, got {}",
                resp.status()
            );
        },
        Err(_) => {
            // Connection closed is acceptable - fuel exhaustion may close connection
        },
    }

    // Should complete relatively quickly (fuel exhaustion, not full timeout)
    assert!(
        elapsed < Duration::from_secs(8),
        "Should exhaust fuel before timeout, took {:?}",
        elapsed
    );

    // Verify server is still running
    let health = host
        .get("/health")
        .await
        .expect("Server should still be running");
    assert_eq!(health.status(), 200);
}

// =============================================================================
// Module Not Found Tests
// =============================================================================

#[tokio::test]
async fn test_nonexistent_module_returns_404() {
    require_fixture!("echo.wasm"); // Need at least one module for host to start

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let resp = host
        .post_json("/run/nonexistent/", &serde_json::json!({}))
        .await
        .expect("Request failed");

    assert_eq!(resp.status(), 404);
}

// =============================================================================
// Health and Metrics Tests
// =============================================================================

#[tokio::test]
async fn test_health_endpoint_with_real_runtime() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let resp = host.get("/health").await.expect("Request failed");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse");
    assert_eq!(body["status"], "ready");
    assert!(body["cache_size"].is_number());
    assert!(body["cache_capacity"].is_number());
}

#[tokio::test]
async fn test_metrics_endpoint_with_real_runtime() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    // Make a request to generate metrics
    host.post_json("/run/echo/", &serde_json::json!({}))
        .await
        .expect("Request failed");

    let resp = host.get("/metrics").await.expect("Request failed");

    assert_eq!(resp.status(), 200);

    let body = resp.text().await.expect("Failed to get text");
    assert!(body.contains("mik_requests_total"));
}

// =============================================================================
// Cache Tests
// =============================================================================

#[tokio::test]
async fn test_module_caching() {
    require_fixture!("echo.wasm");

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_cache_size(10)
        .start()
        .await
        .expect("Failed to start host");

    // First request - cold cache
    let start1 = Instant::now();
    let resp1 = host
        .post_json("/run/echo/", &serde_json::json!({"n": 1}))
        .await
        .expect("Request 1 failed");
    let cold_time = start1.elapsed();
    assert_eq!(resp1.status(), 200, "First request should succeed");

    // Second request - warm cache (instance reuse)
    let start2 = Instant::now();
    let resp2 = host
        .post_json("/run/echo/", &serde_json::json!({"n": 2}))
        .await
        .expect("Request 2 failed");
    let warm_time = start2.elapsed();
    assert_eq!(resp2.status(), 200, "Second request should succeed");

    // Log timing for analysis (warm should generally be faster than cold)
    println!("Cold: {:?}, Warm: {:?}", cold_time, warm_time);

    // Verify both requests returned correct data
    let body1: serde_json::Value = resp1.json().await.expect("Parse failed");
    let body2: serde_json::Value = resp2.json().await.expect("Parse failed");
    assert_eq!(body1["n"], 1);
    assert_eq!(body2["n"], 2);
}
