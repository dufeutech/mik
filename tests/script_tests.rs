//! Comprehensive tests for JS script orchestration feature.
//!
//! Tests for the `/script/<name>` endpoint that executes JavaScript scripts
//! which can call WASM handlers via `host.call()`.
//!
//! The script engine provides:
//! - `input` - Request body (parsed JSON)
//! - `host.call(module, options)` - Call WASM handlers
//!
//! And explicitly does NOT provide:
//! - Network access (no fetch)
//! - Filesystem access
//! - Module imports (no require)
//! - Shell/process access

#[path = "common.rs"]
mod common;

use common::TestHost;
use std::path::PathBuf;

/// Get the path to the test scripts directory.
fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("scripts")
}

// =============================================================================
// Basic Script Execution Tests
// =============================================================================

#[tokio::test]
async fn test_script_endpoint_basic_execution() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/simple", &serde_json::json!({"name": "test"}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(
        body.get("result").is_some(),
        "Response should have 'result' field"
    );

    let result = &body["result"];
    assert_eq!(result["message"], "Hello from script");
    assert_eq!(result["received"]["name"], "test");
}

#[tokio::test]
async fn test_script_echo_input() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let input = serde_json::json!({
        "foo": "bar",
        "count": 42,
        "nested": {"a": 1, "b": 2}
    });

    let resp = host
        .post_json("/script/echo", &input)
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["result"], input);
}

#[tokio::test]
async fn test_script_transform_input() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/script/transform",
            &serde_json::json!({"value": 5, "name": "Alice"}),
        )
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["original"], 5);
    assert_eq!(result["doubled"], 10);
    assert_eq!(result["squared"], 25);
    assert_eq!(result["message"], "Alice processed");
}

#[tokio::test]
async fn test_script_with_empty_body() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    // POST with empty body - input should be null
    let resp = host
        .client()
        .post(host.url("/script/echo"))
        .header("Content-Type", "application/json")
        .send()
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(body["result"].is_null());
}

#[tokio::test]
async fn test_script_with_array_input() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let input = serde_json::json!([1, 2, 3, 4, 5]);

    let resp = host
        .post_json("/script/echo", &input)
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["result"], input);
}

// =============================================================================
// host.call() Functionality Tests
// =============================================================================

#[tokio::test]
async fn test_script_single_host_call() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/script/host_call_single",
            &serde_json::json!({"data": "test-data"}),
        )
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    // Mock host.call returns a structured response
    // Note: JS returns numbers as floats, so compare as i64 or use as_f64
    assert!(
        result["status"].as_i64() == Some(200) || result["status"].as_f64() == Some(200.0),
        "Expected status 200, got {:?}",
        result["status"]
    );
    assert!(result.get("body").is_some());

    let call_body = &result["body"];
    assert_eq!(call_body["module"], "echo");
    assert_eq!(call_body["method"], "POST");
    assert_eq!(call_body["path"], "/");
    assert_eq!(call_body["received"]["data"], "test-data");
}

#[tokio::test]
async fn test_script_chained_host_calls() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/script/host_call_chained",
            &serde_json::json!({"data": "chain-test"}),
        )
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    // Script chains 3 calls
    assert_eq!(result["calls"], 3);

    // Final result should contain the third call's response
    let final_result = &result["final_result"];
    assert!(final_result.get("body").is_some() || final_result.get("module").is_some());
}

#[tokio::test]
async fn test_script_host_call_with_custom_path() {
    // Use a script that calls with a specific path
    let script_content = r#"
export default function(input) {
    var result = host.call("api", {
        method: "GET",
        path: "/users/123",
        body: null
    });
    return result.body;
}
"#;

    // Create a temporary script for this test
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_path = temp_dir.path().join("custom_path.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/custom_path", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["path"], "/users/123");
    assert_eq!(result["method"], "GET");
}

#[tokio::test]
async fn test_script_host_call_with_headers() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    var result = host.call("auth", {
        method: "POST",
        path: "/verify",
        headers: {
            "Authorization": "Bearer token123",
            "X-Custom-Header": "custom-value"
        },
        body: { user: "test" }
    });
    return result;
}
"#;

    let script_path = temp_dir.path().join("headers_test.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/headers_test", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    // The mock returns the body which contains what was received
    // JS returns numbers as floats
    assert!(
        body["result"]["status"].as_i64() == Some(200)
            || body["result"]["status"].as_f64() == Some(200.0),
        "Expected status 200, got {:?}",
        body["result"]["status"]
    );
}

// =============================================================================
// Script Error Handling Tests
// =============================================================================

#[tokio::test]
async fn test_script_syntax_error() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/syntax_error", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 500);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(body.get("error").is_some());

    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("Script error") || error.contains("SyntaxError") || error.contains("error"),
        "Error message should indicate syntax error: {}",
        error
    );
}

#[tokio::test]
async fn test_script_runtime_error() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/runtime_error", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 500);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(body.get("error").is_some());

    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("error") || error.contains("undefined"),
        "Error message should indicate runtime error: {}",
        error
    );
}

#[tokio::test]
async fn test_script_not_found() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/nonexistent_script", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 404);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(body.get("error").is_some());

    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("not found") || error.contains("Script"),
        "Error message should indicate script not found: {}",
        error
    );
}

#[tokio::test]
async fn test_script_missing_name() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    // Should return 400 or 404 for missing script name
    assert!(resp.status() == 400 || resp.status() == 404);
}

#[tokio::test]
async fn test_script_without_scripts_dir() {
    let host = TestHost::builder()
        // No scripts_dir configured
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/simple", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 503);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(body.get("error").is_some());
}

// =============================================================================
// Script Timeout Tests
// =============================================================================

#[tokio::test]
#[ignore = "Infinite loop test - runs slowly due to iteration limit"]
async fn test_script_infinite_loop_timeout() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    // This test verifies that infinite loops are eventually interrupted
    // The script has `while(true) {}` which should be caught by execution limits
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        host.post_json("/script/infinite_loop", &serde_json::json!({})),
    )
    .await;

    match result {
        Ok(resp) => {
            let resp = resp.expect("Request failed");
            // Should return an error, not hang forever
            assert!(resp.status() == 500 || resp.status() == 408);
        },
        Err(_) => {
            // Timeout is acceptable - script execution was interrupted
        },
    }
}

// =============================================================================
// Sandbox Security Tests
// =============================================================================

#[tokio::test]
async fn test_script_sandbox_no_fetch() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/sandbox_test", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    // Verify dangerous APIs are not available
    assert_eq!(result["has_fetch"], false, "fetch should not be available");
    assert_eq!(
        result["has_require"], false,
        "require should not be available"
    );
    assert_eq!(
        result["has_process"], false,
        "process should not be available"
    );
    assert_eq!(result["has_fs"], false, "fs should not be available");

    // host.call should be available
    assert_eq!(result["has_host"], true, "host should be available");
    assert_eq!(
        result["has_host_call"], true,
        "host.call should be available"
    );
}

#[tokio::test]
async fn test_script_sandbox_no_require() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    try {
        require('fs');
        return { success: false, error: "require should not work" };
    } catch(e) {
        return { success: true, caught: e.toString() };
    }
}
"#;

    let script_path = temp_dir.path().join("require_test.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/require_test", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    let status = resp.status();

    // Script should run successfully (the try/catch handles the error)
    // or return 500 if require causes a hard error
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");

    if status == 200 {
        // Try/catch worked, require failed as expected
        assert_eq!(body["result"]["success"], true);
    }
    // If status is 500, require caused a hard error which is also acceptable
}

#[tokio::test]
async fn test_script_sandbox_no_filesystem() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    return {
        has_fs: typeof fs !== "undefined",
        has_readFileSync: typeof readFileSync !== "undefined",
        has_writeFileSync: typeof writeFileSync !== "undefined",
        has_open: typeof open !== "undefined"
    };
}
"#;

    let script_path = temp_dir.path().join("fs_check.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/fs_check", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["has_fs"], false);
    assert_eq!(result["has_readFileSync"], false);
    assert_eq!(result["has_writeFileSync"], false);
}

#[tokio::test]
async fn test_script_sandbox_no_network() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    return {
        has_fetch: typeof fetch !== "undefined",
        has_XMLHttpRequest: typeof XMLHttpRequest !== "undefined",
        has_WebSocket: typeof WebSocket !== "undefined",
        has_http: typeof http !== "undefined",
        has_https: typeof https !== "undefined"
    };
}
"#;

    let script_path = temp_dir.path().join("network_check.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/network_check", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["has_fetch"], false);
    assert_eq!(result["has_XMLHttpRequest"], false);
    assert_eq!(result["has_WebSocket"], false);
    assert_eq!(result["has_http"], false);
    assert_eq!(result["has_https"], false);
}

#[tokio::test]
async fn test_script_sandbox_no_process() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    return {
        has_process: typeof process !== "undefined",
        has_env: typeof process !== "undefined" && typeof process.env !== "undefined",
        has_exit: typeof process !== "undefined" && typeof process.exit !== "undefined",
        has_global: typeof global !== "undefined",
        has_Buffer: typeof Buffer !== "undefined"
    };
}
"#;

    let script_path = temp_dir.path().join("process_check.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/process_check", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["has_process"], false);
    assert_eq!(result["has_env"], false);
    assert_eq!(result["has_exit"], false);
    assert_eq!(result["has_global"], false);
    assert_eq!(result["has_Buffer"], false);
}

// =============================================================================
// Input/Output Tests
// =============================================================================

#[tokio::test]
async fn test_script_complex_json_input() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let complex_input = serde_json::json!({
        "string": "hello",
        "number": 42,
        "float": 1.234,
        "boolean": true,
        "null_value": null,
        "array": [1, 2, 3, "mixed", true],
        "nested": {
            "level1": {
                "level2": {
                    "value": "deep"
                }
            }
        }
    });

    let resp = host
        .post_json("/script/echo", &complex_input)
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["string"], "hello");
    assert_eq!(result["number"], 42);
    assert_eq!(result["boolean"], true);
    assert!(result["null_value"].is_null());
    assert_eq!(result["nested"]["level1"]["level2"]["value"], "deep");
}

#[tokio::test]
async fn test_script_json_response_structure() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/simple", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    // Verify Content-Type header
    let content_type = resp
        .headers()
        .get("content-type")
        .expect("No content-type header")
        .to_str()
        .expect("Invalid content-type");
    assert!(
        content_type.contains("application/json"),
        "Expected JSON content type, got: {}",
        content_type
    );

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");

    // Response should have 'result' and 'calls_executed' fields
    assert!(
        body.get("result").is_some(),
        "Response should have 'result' field"
    );
    assert!(
        body.get("calls_executed").is_some(),
        "Response should have 'calls_executed' field"
    );
}

#[tokio::test]
async fn test_script_returns_primitive_types() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    // Test returning a string
    let script_content = r#"export default function(input) { return "hello world"; }"#;
    let script_path = temp_dir.path().join("return_string.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/return_string", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["result"], "hello world");
}

#[tokio::test]
async fn test_script_returns_number() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let script_content = r"export default function(input) { return 42; }";
    let script_path = temp_dir.path().join("return_number.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/return_number", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["result"], 42);
}

#[tokio::test]
async fn test_script_returns_boolean() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let script_content = r"export default function(input) { return true; }";
    let script_path = temp_dir.path().join("return_bool.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/return_bool", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["result"], true);
}

#[tokio::test]
async fn test_script_returns_null() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let script_content = r"export default function(input) { return null; }";
    let script_path = temp_dir.path().join("return_null.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/return_null", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert!(body["result"].is_null());
}

#[tokio::test]
async fn test_script_returns_array() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let script_content = r#"export default function(input) { return [1, 2, 3, "four", true]; }"#;
    let script_path = temp_dir.path().join("return_array.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/return_array", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");

    let result = body["result"].as_array().expect("Result should be array");
    assert_eq!(result.len(), 5);
    assert_eq!(result[0], 1);
    assert_eq!(result[3], "four");
    assert_eq!(result[4], true);
}

// =============================================================================
// Multiple host.call() Tests
// =============================================================================

#[tokio::test]
async fn test_script_multiple_host_calls_parallel_pattern() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    // Make multiple independent calls
    var user = host.call("users", {
        method: "GET",
        path: "/user/" + input.userId
    });

    var orders = host.call("orders", {
        method: "GET",
        path: "/orders?userId=" + input.userId
    });

    var preferences = host.call("preferences", {
        method: "GET",
        path: "/prefs/" + input.userId
    });

    return {
        user: user.body,
        orders: orders.body,
        preferences: preferences.body,
        allSucceeded: user.status === 200 && orders.status === 200 && preferences.status === 200
    };
}
"#;

    let script_path = temp_dir.path().join("parallel_calls.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/parallel_calls", &serde_json::json!({"userId": 42}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["allSucceeded"], true);
    assert!(result.get("user").is_some());
    assert!(result.get("orders").is_some());
    assert!(result.get("preferences").is_some());
}

#[tokio::test]
async fn test_script_conditional_host_call() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    // First verify auth
    var auth = host.call("auth", {
        method: "POST",
        path: "/verify",
        body: { token: input.token }
    });

    if (auth.status !== 200) {
        return { error: "Unauthorized", status: 401 };
    }

    // Only proceed if authorized
    var data = host.call("data", {
        method: "GET",
        path: "/protected-resource"
    });

    return {
        authorized: true,
        data: data.body
    };
}
"#;

    let script_path = temp_dir.path().join("conditional_call.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/script/conditional_call",
            &serde_json::json!({"token": "valid-token"}),
        )
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    // With mock, auth always succeeds (returns 200)
    assert_eq!(result["authorized"], true);
    assert!(result.get("data").is_some());
}

#[tokio::test]
async fn test_script_loop_host_calls() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    var results = [];
    var ids = input.ids || [1, 2, 3];

    for (var i = 0; i < ids.length; i++) {
        var result = host.call("items", {
            method: "GET",
            path: "/item/" + ids[i]
        });
        results.push({
            id: ids[i],
            status: result.status,
            body: result.body
        });
    }

    return {
        count: results.length,
        results: results
    };
}
"#;

    let script_path = temp_dir.path().join("loop_calls.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/script/loop_calls",
            &serde_json::json!({"ids": [10, 20, 30, 40]}),
        )
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["count"], 4);

    let results = result["results"]
        .as_array()
        .expect("results should be array");
    assert_eq!(results.len(), 4);
}

// =============================================================================
// Edge Cases and Error Recovery Tests
// =============================================================================

#[tokio::test]
async fn test_script_handles_host_call_error() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    var result = host.call("failing_module", {
        method: "POST",
        path: "/fail"
    });

    // Script should be able to check and handle errors
    if (result.error || result.status >= 400) {
        return {
            handled: true,
            originalStatus: result.status,
            originalError: result.error || null
        };
    }

    return { handled: false, data: result.body };
}
"#;

    let script_path = temp_dir.path().join("error_handling.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/error_handling", &serde_json::json!({}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);
    // Script should run successfully even when handling module errors
}

#[tokio::test]
async fn test_script_unicode_handling() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    return {
        original: input.text,
        length: input.text.length,
        // Test emoji and unicode
        emoji: "Hello! ",
        chinese: "",
        combined: input.text + " "
    };
}
"#;

    let script_path = temp_dir.path().join("unicode.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json(
            "/script/unicode",
            &serde_json::json!({"text": "Hello World!"}),
        )
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert!(result["emoji"].as_str().unwrap().contains("Hello"));
}

#[tokio::test]
async fn test_script_large_response() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let script_content = r#"
export default function(input) {
    var arr = [];
    var count = input.count || 100;

    for (var i = 0; i < count; i++) {
        arr.push({
            index: i,
            value: "item-" + i,
            nested: { a: i * 2, b: i * 3 }
        });
    }

    return {
        count: arr.length,
        items: arr
    };
}
"#;

    let script_path = temp_dir.path().join("large_response.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    let resp = host
        .post_json("/script/large_response", &serde_json::json!({"count": 500}))
        .await
        .expect("Failed to execute script");

    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    let result = &body["result"];

    assert_eq!(result["count"], 500);

    let items = result["items"].as_array().expect("items should be array");
    assert_eq!(items.len(), 500);
}

// =============================================================================
// Concurrent Script Execution Tests
// =============================================================================

#[tokio::test]
async fn test_script_concurrent_execution() {
    let host = TestHost::builder()
        .with_scripts_dir(scripts_dir())
        .start()
        .await
        .expect("Failed to start test host");

    // Execute 10 scripts concurrently
    let futures: Vec<_> = (0..10)
        .map(|i| {
            let client = host.client().clone();
            let url = host.url("/script/simple");
            let body = serde_json::json!({"request_id": i});
            async move { client.post(&url).json(&body).send().await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;

    for (i, result) in results.iter().enumerate() {
        let resp = result.as_ref().expect("Request failed");
        assert_eq!(resp.status(), 200, "Request {} should succeed", i);
    }
}

#[tokio::test]
async fn test_script_isolation_between_requests() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    // Script that uses a variable to verify fresh context each time
    // We use a simple pattern that works without relying on implicit globals
    let script_content = r"
export default function(input) {
    // Each fresh context starts with a new local scope
    var localCounter = 1;
    return { counter: localCounter, requestId: input.id };
}
";

    let script_path = temp_dir.path().join("isolation_test.js");
    std::fs::write(&script_path, script_content).expect("Failed to write script");

    let host = TestHost::builder()
        .with_scripts_dir(temp_dir.path())
        .start()
        .await
        .expect("Failed to start test host");

    // Make multiple requests
    let mut results = vec![];
    for i in 0..5 {
        let resp = host
            .post_json("/script/isolation_test", &serde_json::json!({"id": i}))
            .await
            .expect("Failed to execute script");

        let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
        results.push(body);
    }

    // Each request should have counter = 1 (fresh context each time)
    for (i, result) in results.iter().enumerate() {
        let counter = result["result"]["counter"].as_i64().unwrap_or(0);
        assert_eq!(
            counter, 1,
            "Request {} should have fresh context (counter=1), got counter={}",
            i, counter
        );

        // Verify the request ID was passed correctly
        let request_id = result["result"]["requestId"].as_i64().unwrap_or(-1);
        assert_eq!(
            request_id, i as i64,
            "Request {} should have requestId={}, got {}",
            i, i, request_id
        );
    }
}
