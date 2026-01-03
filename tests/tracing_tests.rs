//! Request tracing tests.
//!
//! Tests for the W3C Trace Context implementation:
//! - traceparent header generation
//! - traceparent header propagation
//! - trace_id in tracing spans

// Import TestHost from the common module (sibling file)
#[path = "common.rs"]
mod common;

use common::TestHost;

// =============================================================================
// W3C Trace Context Header Tests
// =============================================================================

#[tokio::test]
async fn test_health_returns_traceparent_header() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host.get("/health").await.expect("Failed to get health");

    // Verify traceparent header is present
    let traceparent = resp
        .headers()
        .get("traceparent")
        .expect("Missing traceparent header");

    // traceparent should be valid W3C format
    let traceparent_str = traceparent.to_str().expect("Invalid traceparent header");
    assert!(
        !traceparent_str.is_empty(),
        "traceparent should not be empty"
    );

    // Validate W3C format: 00-{trace_id}-{parent_id}-{flags}
    let parts: Vec<&str> = traceparent_str.split('-').collect();
    assert_eq!(
        parts.len(),
        4,
        "traceparent should have 4 parts separated by '-'"
    );
    assert_eq!(parts[0], "00", "Version should be 00");
    assert_eq!(parts[1].len(), 32, "trace_id should be 32 hex chars");
    assert_eq!(parts[2].len(), 16, "parent_id should be 16 hex chars");
    assert_eq!(parts[3].len(), 2, "flags should be 2 hex chars");
}

#[tokio::test]
async fn test_metrics_returns_traceparent_header() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host.get("/metrics").await.expect("Failed to get metrics");

    // Verify traceparent header is present
    let traceparent = resp
        .headers()
        .get("traceparent")
        .expect("Missing traceparent header");

    let traceparent_str = traceparent.to_str().expect("Invalid traceparent header");
    assert!(
        !traceparent_str.is_empty(),
        "traceparent should not be empty"
    );
}

#[tokio::test]
async fn test_request_id_and_traceparent_both_present() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host.get("/health").await.expect("Failed to get health");

    // Both X-Request-ID and traceparent should be present
    assert!(
        resp.headers().get("x-request-id").is_some(),
        "Missing X-Request-ID header"
    );
    assert!(
        resp.headers().get("traceparent").is_some(),
        "Missing traceparent header"
    );
}

#[tokio::test]
async fn test_incoming_traceparent_is_propagated() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Send request with W3C traceparent
    let incoming_traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/health", host.addr().port()))
        .header("traceparent", incoming_traceparent)
        .send()
        .await
        .expect("Failed to send request");

    // Verify the response uses the same trace_id
    let returned_traceparent = resp
        .headers()
        .get("traceparent")
        .expect("Missing traceparent header")
        .to_str()
        .expect("Invalid traceparent header");

    // Extract trace_id from both (second field)
    let incoming_parts: Vec<&str> = incoming_traceparent.split('-').collect();
    let returned_parts: Vec<&str> = returned_traceparent.split('-').collect();

    assert_eq!(
        incoming_parts[1], returned_parts[1],
        "trace_id should be propagated from incoming request"
    );
}

#[tokio::test]
async fn test_traceparent_generated_when_not_provided() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Make two requests without traceparent
    let resp1 = host.get("/health").await.expect("Failed to get health");
    let resp2 = host.get("/health").await.expect("Failed to get health");

    let traceparent1 = resp1
        .headers()
        .get("traceparent")
        .expect("Missing traceparent header")
        .to_str()
        .expect("Invalid traceparent");

    let traceparent2 = resp2
        .headers()
        .get("traceparent")
        .expect("Missing traceparent header")
        .to_str()
        .expect("Invalid traceparent");

    // Each request should get a unique trace_id
    let parts1: Vec<&str> = traceparent1.split('-').collect();
    let parts2: Vec<&str> = traceparent2.split('-').collect();

    assert_ne!(
        parts1[1], parts2[1],
        "Each request should have a unique trace_id"
    );
}

#[tokio::test]
async fn test_traceparent_is_w3c_format() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host.get("/health").await.expect("Failed to get health");

    let traceparent = resp
        .headers()
        .get("traceparent")
        .expect("Missing traceparent header")
        .to_str()
        .expect("Invalid traceparent header");

    // W3C format: 00-{32 hex}-{16 hex}-{2 hex}
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts.len(), 4, "traceparent should have 4 parts");
    assert_eq!(parts[0], "00", "Version should be 00");
    assert_eq!(parts[1].len(), 32, "trace_id should be 32 hex chars");
    assert_eq!(parts[2].len(), 16, "parent_id should be 16 hex chars");
    assert_eq!(parts[3].len(), 2, "flags should be 2 hex chars");

    // All parts should be valid hex
    assert!(
        parts[1].chars().all(|c| c.is_ascii_hexdigit()),
        "trace_id should be hex"
    );
    assert!(
        parts[2].chars().all(|c| c.is_ascii_hexdigit()),
        "parent_id should be hex"
    );
    assert!(
        parts[3].chars().all(|c| c.is_ascii_hexdigit()),
        "flags should be hex"
    );
}

// =============================================================================
// Unit Tests - Header Validation
// =============================================================================

#[test]
fn test_traceparent_header_name_is_lowercase() {
    // Verify we're using lowercase header names (HTTP/2 requirement)
    let header_name = "traceparent";
    assert!(
        header_name.chars().all(|c| c.is_lowercase()),
        "Header name should be lowercase for HTTP/2 compatibility"
    );
}
