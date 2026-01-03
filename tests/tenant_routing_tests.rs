//! Integration tests for multi-tenant routing.
//!
//! Tests the `/tenant/<tenant-id>/<module>/` routing functionality.
//!
//! # Test Fixtures
//!
//! These tests require:
//! - `tests/fixtures/modules/echo.wasm` - Platform module
//! - `tests/fixtures/user-modules/tenant-abc/orders.wasm` - Tenant A module
//! - `tests/fixtures/user-modules/tenant-xyz/products.wasm` - Tenant B module

mod common;

use common::RealTestHost;
use std::path::PathBuf;

/// Check if required fixtures exist.
fn fixtures_available() -> bool {
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let platform_module = fixtures_dir.join("modules/echo.wasm");
    let tenant_a_module = fixtures_dir.join("user-modules/tenant-abc/orders.wasm");
    let tenant_b_module = fixtures_dir.join("user-modules/tenant-xyz/products.wasm");

    platform_module.exists() && tenant_a_module.exists() && tenant_b_module.exists()
}

/// Get the test fixtures directories.
fn get_fixture_dirs() -> (PathBuf, PathBuf) {
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let modules_dir = fixtures_dir.join("modules");
    let user_modules_dir = fixtures_dir.join("user-modules");
    (modules_dir, user_modules_dir)
}

/// Macro to skip tests if fixtures are not available.
macro_rules! require_fixtures {
    () => {
        if !fixtures_available() {
            eprintln!("Skipping: tenant routing fixtures not found");
            return;
        }
    };
}

// =============================================================================
// Platform Module Tests (/run/*)
// =============================================================================

#[tokio::test]
async fn test_platform_module_access() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Access platform module via /run/
    let resp = host
        .post_json("/run/echo/", &serde_json::json!({"source": "platform"}))
        .await
        .expect("Failed to call platform module");

    assert_eq!(resp.status(), 200, "Platform module should return 200");

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["source"], "platform", "Should echo the source field");
}

#[tokio::test]
async fn test_platform_module_with_subpath() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Access platform module with subpath
    let resp = host
        .post_json(
            "/run/echo/api/v1/items",
            &serde_json::json!({"action": "list"}),
        )
        .await
        .expect("Failed to call platform module with subpath");

    assert_eq!(
        resp.status(),
        200,
        "Platform module subpath should return 200"
    );
}

// =============================================================================
// Tenant Module Tests (/tenant/*)
// =============================================================================

#[tokio::test]
async fn test_tenant_module_access() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Access tenant A's orders module via /tenant/
    let resp = host
        .post_json(
            "/tenant/tenant-abc/orders/",
            &serde_json::json!({"tenant": "abc", "action": "create_order"}),
        )
        .await
        .expect("Failed to call tenant module");

    assert_eq!(resp.status(), 200, "Tenant module should return 200");

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["tenant"], "abc", "Should echo the tenant field");
}

#[tokio::test]
async fn test_tenant_module_with_subpath() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Access tenant module with subpath
    let resp = host
        .post_json(
            "/tenant/tenant-abc/orders/api/v1/items",
            &serde_json::json!({"resource": "items"}),
        )
        .await
        .expect("Failed to call tenant module with subpath");

    assert_eq!(
        resp.status(),
        200,
        "Tenant module subpath should return 200"
    );
}

#[tokio::test]
async fn test_different_tenants_different_modules() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Access tenant A's orders module
    let resp_a = host
        .post_json(
            "/tenant/tenant-abc/orders/",
            &serde_json::json!({"tenant": "abc"}),
        )
        .await
        .expect("Failed to call tenant A module");
    assert_eq!(resp_a.status(), 200, "Tenant A module should return 200");

    // Access tenant B's products module
    let resp_b = host
        .post_json(
            "/tenant/tenant-xyz/products/",
            &serde_json::json!({"tenant": "xyz"}),
        )
        .await
        .expect("Failed to call tenant B module");
    assert_eq!(resp_b.status(), 200, "Tenant B module should return 200");
}

// =============================================================================
// Tenant Isolation Tests
// =============================================================================

#[tokio::test]
async fn test_tenant_cannot_access_other_tenant_module() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Tenant A tries to access tenant B's products module
    // This should fail because tenant-abc doesn't have a products.wasm
    let resp = host
        .post_json(
            "/tenant/tenant-abc/products/",
            &serde_json::json!({"attempt": "cross-tenant"}),
        )
        .await
        .expect("Failed to make cross-tenant request");

    // Should return 404 (module not found for this tenant)
    assert_eq!(resp.status(), 404, "Cross-tenant access should return 404");
}

#[tokio::test]
async fn test_nonexistent_tenant_returns_404() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Try to access a non-existent tenant
    let resp = host
        .post_json(
            "/tenant/nonexistent-tenant/orders/",
            &serde_json::json!({"test": true}),
        )
        .await
        .expect("Failed to make request to nonexistent tenant");

    assert_eq!(resp.status(), 404, "Nonexistent tenant should return 404");
}

#[tokio::test]
async fn test_tenant_cannot_access_platform_modules() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Try to access platform's echo module via tenant route
    // This should fail because tenant-abc doesn't have an echo.wasm
    let resp = host
        .post_json(
            "/tenant/tenant-abc/echo/",
            &serde_json::json!({"attempt": "access-platform"}),
        )
        .await
        .expect("Failed to make request");

    assert_eq!(
        resp.status(),
        404,
        "Tenant route should not access platform modules"
    );
}

// =============================================================================
// Error Cases
// =============================================================================

#[tokio::test]
async fn test_tenant_routing_without_user_modules_dir() {
    require_fixtures!();

    let (modules_dir, _) = get_fixture_dirs();

    // Start without user_modules_dir configured
    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        // No user_modules_dir set
        .start()
        .await
        .expect("Failed to start test host");

    // Try to access tenant route - should fail since user_modules not configured
    let resp = host
        .post_json(
            "/tenant/tenant-abc/orders/",
            &serde_json::json!({"test": true}),
        )
        .await
        .expect("Failed to make tenant request");

    // Should return error status (tenant routing not configured)
    // 503 = service unavailable, 404 = not found, 400 = bad request (config missing)
    assert!(
        resp.status() == 503 || resp.status() == 404 || resp.status() == 400,
        "Tenant routing without config should return 503, 404, or 400, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn test_invalid_tenant_path_format() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Try malformed tenant paths
    let resp = host.get("/tenant/").await.expect("Failed to make request");
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "Missing tenant-id should return 400 or 404"
    );

    let resp = host
        .get("/tenant/tenant-abc")
        .await
        .expect("Failed to make request");
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "Missing module should return 400 or 404"
    );
}

// =============================================================================
// Gateway API Tests
// =============================================================================

#[tokio::test]
async fn test_gateway_handlers_includes_tenant_modules() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    // Call the gateway handlers endpoint
    let resp = host
        .get("/_mik/handlers")
        .await
        .expect("Failed to call handlers endpoint");

    assert_eq!(resp.status(), 200, "Handlers endpoint should return 200");

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");

    // Check that we have handlers listed
    let data = body["data"].as_array().expect("data should be an array");
    assert!(!data.is_empty(), "Should have at least one handler");

    // Check meta includes tenant count
    let meta = &body["meta"];
    assert!(
        meta["tenant_count"].as_u64().unwrap_or(0) > 0,
        "Should have tenant modules"
    );
}

#[tokio::test]
async fn test_gateway_handlers_links_format() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .get("/_mik/handlers")
        .await
        .expect("Failed to call handlers endpoint");

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let data = body["data"].as_array().expect("data should be an array");

    for handler in data {
        let links = &handler["links"];
        let self_link = links["self"].as_str().expect("self link should exist");

        // Platform handlers should have /run/ prefix
        // Tenant handlers should have /tenant/<id>/ prefix
        let is_tenant = handler["attributes"]["tenant_id"].is_string();

        if is_tenant {
            assert!(
                self_link.starts_with("/tenant/"),
                "Tenant handler self link should start with /tenant/: {}",
                self_link
            );
        } else {
            assert!(
                self_link.starts_with("/run/"),
                "Platform handler self link should start with /run/: {}",
                self_link
            );
        }
    }
}

// =============================================================================
// Response Headers Tests
// =============================================================================

#[tokio::test]
async fn test_tenant_response_includes_handler_header() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/tenant/tenant-abc/orders/",
            &serde_json::json!({"test": true}),
        )
        .await
        .expect("Failed to call tenant module");

    assert_eq!(resp.status(), 200);

    // Check X-Mik-Handler header exists
    let handler_header = resp.headers().get("x-mik-handler");
    assert!(
        handler_header.is_some(),
        "Response should include X-Mik-Handler header"
    );

    // The handler name should include tenant context
    let handler_name = handler_header.unwrap().to_str().unwrap();
    assert!(
        handler_name.contains("tenant-abc") || handler_name.contains("orders"),
        "Handler header should reference tenant or module: {}",
        handler_name
    );
}

#[tokio::test]
async fn test_tenant_response_includes_duration_header() {
    require_fixtures!();

    let (modules_dir, user_modules_dir) = get_fixture_dirs();

    let host = RealTestHost::builder()
        .with_modules_dir(&modules_dir)
        .with_user_modules_dir(&user_modules_dir)
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/tenant/tenant-abc/orders/",
            &serde_json::json!({"test": true}),
        )
        .await
        .expect("Failed to call tenant module");

    assert_eq!(resp.status(), 200);

    // Check X-Mik-Duration-Ms header exists
    let duration_header = resp.headers().get("x-mik-duration-ms");
    assert!(
        duration_header.is_some(),
        "Response should include X-Mik-Duration-Ms header"
    );

    // Duration should be a valid number
    let duration_str = duration_header.unwrap().to_str().unwrap();
    let duration: u64 = duration_str
        .parse()
        .expect("Duration should be a valid number");
    assert!(duration < 10000, "Duration should be reasonable (< 10s)");
}
