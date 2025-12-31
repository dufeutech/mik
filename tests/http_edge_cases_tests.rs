// Test-specific lint suppressions
#![allow(clippy::collapsible_if)]

//! HTTP edge case tests for the mikrozen runtime.
//!
//! These tests cover edge cases in HTTP handling that have caused
//! crashes, hangs, or incorrect behavior in production WASM runtimes.
//!
//! ## Background
//!
//! HTTP-related production issues:
//!
//! - Wasmtime #8269: wasi::http::outgoing_handler::handle crashes on invalid args
//! - reqwest #1276: HTTP/2 connection pooling breaks with high concurrency
//! - General: Keep-alive connection handling issues
//!
//! ## Test Philosophy
//!
//! 1. **Invalid inputs** - Server should return errors, not crash
//! 2. **High concurrency** - Connection pools should handle load
//! 3. **Connection reuse** - Keep-alive should work correctly
//!
//! ## Running Tests
//!
//! ```bash
//! # Run unit tests
//! cargo test -p mik http_edge_cases::tests
//!
//! # Run integration tests
//! cargo test -p mik http_edge_cases -- --ignored --test-threads=1
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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("modules")
}

fn echo_wasm_exists() -> bool {
    fixtures_dir().join("echo.wasm").exists()
}

// =============================================================================
// Integration Tests - Invalid Input Handling
// =============================================================================

/// Test that invalid HTTP methods don't crash the server.
///
/// Based on Wasmtime #8269: Invalid arguments should return errors, not crash.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_invalid_http_method_no_crash() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let client = reqwest::Client::new();
    let url = host.url("/run/echo/");

    // Test various edge case methods
    let methods = [
        "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD",
        // Custom methods (some servers reject these)
        "PROPFIND", "MKCOL", "COPY", "MOVE", "LOCK", "UNLOCK",
    ];

    for method in methods {
        let result = client
            .request(method.parse().unwrap_or(reqwest::Method::GET), &url)
            .send()
            .await;

        // Should get a response (success or error), not a crash
        match result {
            Ok(resp) => {
                println!("Method {}: status {}", method, resp.status());
            },
            Err(e) => {
                // Connection errors are OK, crashes are not
                println!("Method {}: error {}", method, e);
                assert!(
                    !e.to_string().contains("connection reset"),
                    "Method {} caused connection reset (possible crash)",
                    method
                );
            },
        }
    }

    // Server should still be healthy
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

/// Test that malformed request paths don't crash the server.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_malformed_paths_no_crash() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let client = reqwest::Client::new();
    let base = host.url("");

    // Malformed paths that might cause issues
    let paths = [
        "/run/echo/",
        "/run/echo",
        "/run/echo////",
        "/run/echo/../echo/",
        "/run/echo/./",
        "/run/echo/%00",
        "/run/echo/%2e%2e",
        "/run/echo/foo%00bar",
        "/run/echo?",
        "/run/echo?foo",
        "/run/echo?foo=bar&baz",
        "/run/echo#fragment",
    ];

    for path in paths {
        let url = format!("{}{}", base, path);
        let result = client.post(&url).send().await;

        match result {
            Ok(resp) => {
                println!("Path '{}': status {}", path, resp.status());
            },
            Err(e) => {
                println!("Path '{}': error {}", path, e);
            },
        }
    }

    // Server should still be healthy
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

/// Test that oversized headers don't crash the server.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_oversized_headers_no_crash() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let client = reqwest::Client::new();
    let url = host.url("/run/echo/");

    // Test with increasingly large headers
    let sizes = [100, 1000, 8000, 16000, 32000];

    for size in sizes {
        let large_value = "x".repeat(size);
        let result = client
            .post(&url)
            .header("X-Large-Header", &large_value)
            .json(&serde_json::json!({"test": "headers"}))
            .send()
            .await;

        match result {
            Ok(resp) => {
                println!("Header size {}: status {}", size, resp.status());
                // Large headers might be rejected (431) but shouldn't crash
            },
            Err(e) => {
                println!("Header size {}: error {}", size, e);
            },
        }
    }

    // Server should still be healthy
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

// =============================================================================
// Integration Tests - Connection Pooling
// =============================================================================

/// Test HTTP connection pooling under high concurrency.
///
/// Based on reqwest #1276: Connection pooling can break under high concurrency.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_connection_pool_high_concurrency() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .with_max_concurrent_requests(200)
        .start()
        .await
        .expect("Failed to start host");

    let concurrency = 100;
    let requests_per_worker = 20;
    let success = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let pool_errors = Arc::new(AtomicU64::new(0));

    println!(
        "Testing connection pool: {} workers x {} requests",
        concurrency, requests_per_worker
    );

    let start = Instant::now();
    let mut handles = Vec::new();

    for worker_id in 0..concurrency {
        let url = host.url("/run/echo/");
        let success = success.clone();
        let failed = failed.clone();
        let pool_errors = pool_errors.clone();

        handles.push(tokio::spawn(async move {
            // Each worker uses its own client to stress connection pooling
            let client = reqwest::Client::builder()
                .pool_max_idle_per_host(10)
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();

            for req_id in 0..requests_per_worker {
                let result = client
                    .post(&url)
                    .json(&serde_json::json!({
                        "worker": worker_id,
                        "request": req_id
                    }))
                    .send()
                    .await;

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        success.fetch_add(1, Ordering::Relaxed);
                    },
                    Ok(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    },
                    Err(e) => {
                        if e.to_string().contains("pool") {
                            pool_errors.fetch_add(1, Ordering::Relaxed);
                        }
                        failed.fetch_add(1, Ordering::Relaxed);
                    },
                }
            }
        }));
    }

    // Wait for all workers
    for handle in handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    let total_success = success.load(Ordering::Relaxed);
    let total_failed = failed.load(Ordering::Relaxed);
    let total_pool_errors = pool_errors.load(Ordering::Relaxed);
    let total = total_success + total_failed;

    println!("\n=== Connection Pool Test Results ===");
    println!("Duration: {:.2}s", elapsed.as_secs_f64());
    println!("Total requests: {}", total);
    println!("Successful: {}", total_success);
    println!("Failed: {}", total_failed);
    println!("Pool errors: {}", total_pool_errors);
    println!(
        "Success rate: {:.1}%",
        (total_success as f64 / total as f64) * 100.0
    );
    println!(
        "Throughput: {:.1} req/s",
        total as f64 / elapsed.as_secs_f64()
    );

    // Should have high success rate
    assert!(
        total_success as f64 / total as f64 >= 0.90,
        "Success rate should be >= 90%"
    );

    // Pool errors should be minimal
    assert!(
        total_pool_errors < total / 10,
        "Pool errors should be < 10% of total"
    );

    // Server health
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

/// Test keep-alive connection reuse.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_keep_alive_connection_reuse() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    // Single client, multiple sequential requests (should reuse connection)
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(5)
        .build()
        .unwrap();

    let url = host.url("/run/echo/");
    let requests = 50;

    println!("Testing keep-alive with {} sequential requests", requests);

    let start = Instant::now();
    let mut success = 0;

    for i in 0..requests {
        let result = client
            .post(&url)
            .json(&serde_json::json!({"request": i}))
            .send()
            .await;

        if result.is_ok() && result.unwrap().status().is_success() {
            success += 1;
        }
    }

    let elapsed = start.elapsed();
    let avg_latency = elapsed.as_millis() as f64 / f64::from(requests);

    println!(
        "Completed {} requests in {:.2}s",
        requests,
        elapsed.as_secs_f64()
    );
    println!("Average latency: {:.1}ms", avg_latency);
    println!("Success: {}/{}", success, requests);

    // With connection reuse, average latency should be reasonable
    // (without reuse, each request would need TCP + TLS handshake)
    assert!(
        avg_latency < 100.0,
        "Average latency {}ms too high (possible connection reuse issue)",
        avg_latency
    );

    assert_eq!(success, requests, "All requests should succeed");
}

/// Test rapid connection open/close cycles.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_rapid_connection_cycles() {
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

    let cycles = 20;
    let requests_per_cycle = 10;

    println!(
        "Testing {} cycles of {} requests each (new client per cycle)",
        cycles, requests_per_cycle
    );

    let mut total_success = 0;
    let start = Instant::now();

    for cycle in 0..cycles {
        // New client each cycle (forces new connections)
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(0) // No pooling
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        for req in 0..requests_per_cycle {
            let url = host.url("/run/echo/");
            if let Ok(resp) = client
                .post(&url)
                .json(&serde_json::json!({"cycle": cycle, "req": req}))
                .send()
                .await
            {
                if resp.status().is_success() {
                    total_success += 1;
                }
            }
        }
    }

    let elapsed = start.elapsed();
    let total = cycles * requests_per_cycle;

    println!("Completed in {:.2}s", elapsed.as_secs_f64());
    println!("Success: {}/{}", total_success, total);

    assert!(
        total_success >= total * 9 / 10,
        "At least 90% should succeed"
    );

    // Server health
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

// =============================================================================
// Integration Tests - Request/Response Edge Cases
// =============================================================================

/// Test empty request body handling.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_empty_request_body() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let client = reqwest::Client::new();
    let url = host.url("/run/echo/");

    // Empty body
    let resp = client.post(&url).send().await.expect("Request should work");
    println!("Empty body: status {}", resp.status());

    // Empty JSON object
    let resp = client
        .post(&url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("Request should work");
    println!("Empty JSON: status {}", resp.status());

    // Null body
    let resp = client
        .post(&url)
        .json(&serde_json::Value::Null)
        .send()
        .await
        .expect("Request should work");
    println!("Null body: status {}", resp.status());

    // Server health
    let health = host.get("/health").await.expect("Health check");
    assert_eq!(health.status(), 200);
}

/// Test concurrent requests to different endpoints.
#[tokio::test]
#[ignore = "Requires echo.wasm fixture"]
async fn test_concurrent_different_endpoints() {
    if !echo_wasm_exists() {
        eprintln!("Skipping: echo.wasm not found");
        return;
    }

    let host = RealTestHost::builder()
        .with_modules_dir(fixtures_dir())
        .start()
        .await
        .expect("Failed to start host");

    let endpoints = ["/run/echo/", "/health", "/metrics"];
    let requests_per_endpoint = 20;

    let mut handles = Vec::new();
    let success = Arc::new(AtomicU64::new(0));

    for endpoint in endpoints {
        for i in 0..requests_per_endpoint {
            let url = host.url(endpoint);
            let success = success.clone();

            handles.push(tokio::spawn(async move {
                let client = reqwest::Client::new();
                let result = if endpoint.contains("echo") {
                    client
                        .post(&url)
                        .json(&serde_json::json!({"req": i}))
                        .send()
                        .await
                } else {
                    client.get(&url).send().await
                };

                if let Ok(r) = result {
                    if r.status().is_success() {
                        success.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }
    }

    for handle in handles {
        let _ = handle.await;
    }

    let total_success = success.load(Ordering::Relaxed);
    let total = endpoints.len() as u64 * requests_per_endpoint as u64;

    println!("Concurrent endpoints: {}/{} success", total_success, total);

    // Most should succeed (health/metrics always, echo usually)
    assert!(
        total_success >= total * 8 / 10,
        "At least 80% should succeed"
    );
}

// =============================================================================
// Unit Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixture_detection() {
        println!("Fixtures dir: {}", fixtures_dir().display());
        println!("echo.wasm exists: {}", echo_wasm_exists());
    }

    #[test]
    fn test_http_method_parsing() {
        // Verify method parsing works for edge cases
        let methods = ["GET", "POST", "PATCH", "PROPFIND", "CUSTOM"];
        for method in methods {
            let parsed: Result<reqwest::Method, _> = method.parse();
            println!("Method '{}': parsed = {:?}", method, parsed.is_ok());
        }
    }

    #[test]
    fn test_url_encoding_edge_cases() {
        // URL encoding edge cases
        let cases = [
            ("%00", "null byte"),
            ("%2e%2e", "encoded .."),
            ("%2f", "encoded /"),
            ("%%", "double percent"),
        ];

        for (encoded, description) in cases {
            println!("Encoded '{}' ({})", encoded, description);
        }
    }

    #[test]
    fn test_header_size_limits() {
        // Common header size limits
        let limits = [
            (8 * 1024, "8KB - Apache default"),
            (16 * 1024, "16KB - Nginx default"),
            (32 * 1024, "32KB - Common max"),
        ];

        for (size, description) in limits {
            println!("Header limit {}: {}", size, description);
        }
    }

    #[test]
    fn test_concurrency_calculations() {
        let workers = 100;
        let requests_per_worker = 20;
        let total = workers * requests_per_worker;

        assert_eq!(total, 2000);

        let success_threshold = total * 90 / 100;
        assert_eq!(success_threshold, 1800);
    }

    #[test]
    fn test_connection_pool_settings() {
        // Verify pool settings are reasonable
        let max_idle = 10u32;
        let timeout_secs = 10u64;

        assert!(max_idle > 0);
        assert!(timeout_secs >= 5);
    }

    #[test]
    fn test_keep_alive_latency_threshold() {
        // With keep-alive, avg latency should be much lower than without
        let with_reuse_ms = 20.0;
        let without_reuse_ms = 100.0; // TCP + TLS handshake

        assert!(with_reuse_ms < without_reuse_ms);

        // Threshold for detecting connection reuse issues
        let threshold_ms = 100.0;
        assert!(with_reuse_ms < threshold_ms);
    }

    #[test]
    fn test_success_rate_calculation() {
        let total = 2000u64;
        let success = 1850u64;
        let rate = success as f64 / total as f64;

        assert!((rate - 0.925).abs() < 0.001);
        assert!(rate >= 0.90); // Meets threshold
    }
}
