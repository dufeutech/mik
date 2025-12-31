//! Handler call execution for scripts.
//!
//! Executes WASM handler calls initiated by `host.call()` from `JavaScript`.

use anyhow::{Context, Result};
use http_body_util::Full;
use hyper::body::Bytes;
use std::sync::Arc;

use super::types::HostCallResult;
use crate::runtime::SharedState;

/// Execute a single handler call (check circuit breaker, rate limit, call WASM).
pub(crate) async fn execute_handler_call(
    shared: Arc<SharedState>,
    module: &str,
    method: &str,
    path: &str,
    headers: Vec<(String, String)>,
    body: Option<serde_json::Value>,
    trace_id: &str,
) -> Result<HostCallResult> {
    use http_body_util::BodyExt;

    // Check circuit breaker
    if let Err(e) = shared.circuit_breaker.check_request(module) {
        return Ok(HostCallResult {
            status: 503,
            headers: vec![],
            body: serde_json::json!({"error": "CIRCUIT_OPEN", "message": e.to_string()}),
            error: Some("CIRCUIT_OPEN".to_string()),
        });
    }

    // Acquire per-module semaphore
    let module_semaphore = shared.get_module_semaphore(module);
    let Ok(_permit) = module_semaphore.try_acquire() else {
        return Ok(HostCallResult {
            status: 429,
            headers: vec![],
            body: serde_json::json!({"error": "RATE_LIMITED", "message": "Module overloaded"}),
            error: Some("RATE_LIMITED".to_string()),
        });
    };

    // Load the WASM module
    let component = match shared.get_or_load(module).await {
        Ok(comp) => comp,
        Err(e) => {
            shared.circuit_breaker.record_failure(module);
            return Ok(HostCallResult {
                status: 404,
                headers: vec![],
                body: serde_json::json!({"error": "MODULE_NOT_FOUND", "message": e.to_string()}),
                error: Some("MODULE_NOT_FOUND".to_string()),
            });
        },
    };

    // Build the HTTP request for the handler
    let body_bytes = body
        .map(|b| serde_json::to_vec(&b).unwrap_or_default())
        .unwrap_or_default();

    // Build URI with localhost authority (required by WASI HTTP)
    let full_uri = format!("http://localhost{path}");

    let mut req_builder = hyper::Request::builder().method(method).uri(&full_uri);

    // Add Host header (required by WASI HTTP)
    req_builder = req_builder.header("host", "localhost");

    // Add trace ID for distributed tracing
    req_builder = req_builder.header("x-trace-id", trace_id);

    for (key, value) in &headers {
        req_builder = req_builder.header(key.as_str(), value.as_str());
    }

    // Add content-type if body present
    if !body_bytes.is_empty() {
        req_builder = req_builder.header("content-type", "application/json");
    }

    let req = req_builder
        .body(Full::new(Bytes::from(body_bytes)))
        .context("Failed to build request")?;

    // Execute the WASM handler
    let result =
        crate::runtime::execute_wasm_request_internal(shared.clone(), component, req).await;

    match result {
        Ok(response) => {
            shared.circuit_breaker.record_success(module);

            // Extract response parts
            let status = response.status().as_u16();
            let resp_headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            // Read response body
            let body_bytes = response
                .into_body()
                .collect()
                .await
                .map(http_body_util::Collected::to_bytes)
                .unwrap_or_default();

            let body: serde_json::Value =
                serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
                    serde_json::Value::String(String::from_utf8_lossy(&body_bytes).to_string())
                });

            Ok(HostCallResult {
                status,
                headers: resp_headers,
                body,
                error: None,
            })
        },
        Err(e) => {
            shared.circuit_breaker.record_failure(module);
            Ok(HostCallResult {
                status: 500,
                headers: vec![],
                body: serde_json::json!({"error": "HANDLER_ERROR", "message": e.to_string()}),
                error: Some("HANDLER_ERROR".to_string()),
            })
        },
    }
}
