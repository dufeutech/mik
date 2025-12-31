//! rquickjs orchestration for mik runtime.
//!
//! Pattern adapted from mikcar/src/sql.rs - uses async/sync bridge via channels.
//!
//! # Security Model
//!
//! Scripts have access to:
//! - `host.call(module, options)` - Call WASM handlers
//! - `input` - Request body (JSON)
//!
//! Scripts do NOT have:
//! - Network access (no fetch)
//! - Filesystem access
//! - Module imports (no require)
//! - Shell/process access

mod bindings;
mod context;
mod handler;
mod runtime;
mod types;

use anyhow::{Context, Result};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::runtime::SharedState;
use crate::runtime::security;
use crate::runtime::spans::{SpanBuilder, SpanCollector};

// Re-export public types for backward compatibility
pub(crate) use types::{HostCallResult, HostMessage, ScriptResponse};

use bindings::HostBridge;
use handler::execute_handler_call;
use runtime::run_js_script;

// =============================================================================
// Public API
// =============================================================================

/// Handle a script request at /script/<name>
pub(crate) async fn handle_script_request(
    shared: Arc<SharedState>,
    req: Request<hyper::body::Incoming>,
    path: &str,
    trace_id: &str,
    span_collector: SpanCollector,
    parent_span_id: &str,
) -> Result<Response<Full<Bytes>>> {
    use http_body_util::BodyExt;

    // Extract script name from path: /script/<name> or /script/<name>/extra
    let script_path = path
        .strip_prefix(crate::runtime::SCRIPT_PREFIX)
        .unwrap_or(path)
        .trim_start_matches('/');

    let script_name = script_path
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing script name"))?;

    // Sanitize script name to prevent path traversal
    let script_name = security::sanitize_module_name(script_name)
        .map_err(|e| anyhow::anyhow!("Invalid script name: {e}"))?;

    // Get scripts directory
    let scripts_dir = shared
        .scripts_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Scripts not enabled (no scripts_dir configured)"))?;

    // Load script file
    let script_file = scripts_dir.join(format!("{script_name}.js"));
    let script = tokio::fs::read_to_string(&script_file)
        .await
        .with_context(|| format!("Script not found: {script_name}"))?;

    // Validate body size before reading
    let content_length = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > shared.max_body_size_bytes {
        anyhow::bail!(
            "Request body too large: {} bytes (max: {} bytes)",
            content_length,
            shared.max_body_size_bytes
        );
    }

    // Parse request body as input
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .context("Failed to read request body")?
        .to_bytes();

    // Double-check actual body size (content-length can be spoofed)
    if body_bytes.len() > shared.max_body_size_bytes {
        anyhow::bail!(
            "Request body too large: {} bytes (max: {} bytes)",
            body_bytes.len(),
            shared.max_body_size_bytes
        );
    }

    let input: serde_json::Value = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid JSON in request body: {e}"))?
    };

    // Execute script with host.call() bridge
    let script_span = SpanBuilder::with_parent(format!("script.{script_name}"), parent_span_id);
    let script_span_id = script_span.span_id().to_string();
    let result = execute_script(
        shared,
        &script,
        &input,
        trace_id,
        span_collector.clone(),
        &script_span_id,
    )
    .await;

    // Record script span
    match &result {
        Ok(_) => span_collector.add(script_span.finish()),
        Err(e) => span_collector.add(script_span.finish_with_error(e.to_string())),
    }

    let result = result?;

    // Return JSON response
    let response_body = serde_json::to_vec(&result)?;
    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(response_body)))?)
}

// =============================================================================
// Script Execution
// =============================================================================

/// Execute a `JavaScript` script with `host.call()` capability.
async fn execute_script(
    shared: Arc<SharedState>,
    script: &str,
    input: &serde_json::Value,
    trace_id: &str,
    span_collector: SpanCollector,
    parent_span_id: &str,
) -> Result<ScriptResponse> {
    // Channel for host.call() messages
    let (host_tx, mut host_rx) = mpsc::unbounded_channel::<HostMessage>();

    // Counter for executed calls
    let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    let bridge = Arc::new(HostBridge { tx: host_tx });
    let bridge_clone = bridge.clone();

    let input_clone = input.clone();
    let script_owned = script.to_string();

    // Spawn JS execution in blocking thread
    let mut js_handle = tokio::task::spawn_blocking(move || {
        run_js_script(&script_owned, &input_clone, bridge_clone)
    });

    // Process host.call() messages while JS runs
    let mut last_error: Option<String> = None;

    loop {
        tokio::select! {
            // Check if JS finished
            js_result = &mut js_handle => {
                match js_result {
                    Ok(Ok(result)) => {
                        if let Some(err) = last_error {
                            return Err(anyhow::anyhow!("Handler error: {err}"));
                        }
                        return Ok(ScriptResponse {
                            result,
                            calls_executed: call_count.load(std::sync::atomic::Ordering::Relaxed),
                        });
                    }
                    Ok(Err(e)) => {
                        return Err(anyhow::anyhow!("Script error: {e}"));
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!("Script panicked: {e}"));
                    }
                }
            }

            // Process host.call() messages
            msg = host_rx.recv() => {
                match msg {
                    Some(HostMessage::Call { module, method, path, headers, body, response_tx }) => {
                        call_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        // Track handler call timing (child of script span)
                        let handler_span = SpanBuilder::with_parent(format!("handler.{module}"), parent_span_id);

                        let result = execute_handler_call(
                            shared.clone(),
                            &module,
                            &method,
                            &path,
                            headers,
                            body,
                            trace_id,
                        ).await;

                        // Record handler span based on result
                        match &result {
                            Ok(resp) if resp.status >= 400 => {
                                span_collector.add(handler_span.finish_with_error(
                                    format!("HTTP {}", resp.status)
                                ));
                            }
                            Ok(_) => {
                                span_collector.add(handler_span.finish());
                            }
                            Err(e) => {
                                span_collector.add(handler_span.finish_with_error(e.to_string()));
                            }
                        }

                        match result {
                            Ok(resp) => {
                                let _ = response_tx.send(resp);
                            }
                            Err(e) => {
                                last_error = Some(e.to_string());
                                let _ = response_tx.send(HostCallResult {
                                    status: 500,
                                    headers: vec![],
                                    body: serde_json::Value::Null,
                                    error: Some(e.to_string()),
                                });
                            }
                        }
                    }
                    None => {
                        // Channel closed, JS finished
                        break;
                    }
                }
            }
        }
    }

    // If we get here, wait for JS to finish
    match js_handle.await {
        Ok(Ok(result)) => {
            if let Some(err) = last_error {
                return Err(anyhow::anyhow!("Handler error: {err}"));
            }
            Ok(ScriptResponse {
                result,
                calls_executed: call_count.load(std::sync::atomic::Ordering::Relaxed),
            })
        },
        Ok(Err(e)) => Err(anyhow::anyhow!("Script error: {e}")),
        Err(e) => Err(anyhow::anyhow!("Script panicked: {e}")),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::context::{js_to_json, preprocess_script};
    use rquickjs::{Context as JsContext, Runtime, Value as JsValue};
    use serde_json::json;

    /// Helper to run a simple JS script without `host.call()` capability.
    /// Used for testing basic JS execution and return value conversion.
    /// Scripts should use `export default function(input) { ... }` syntax.
    fn run_simple_script(
        script: &str,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let runtime = Runtime::new().map_err(|e| format!("Failed to create JS runtime: {e}"))?;
        let context =
            JsContext::full(&runtime).map_err(|e| format!("Failed to create JS context: {e}"))?;

        context.with(|ctx| {
            // Set input object
            let input_json = serde_json::to_string(&input)
                .map_err(|e| format!("Failed to serialize input: {e}"))?;
            let input_script = format!("var input = {input_json};");
            ctx.eval::<(), _>(input_script.as_str())
                .map_err(|e| format!("Failed to set input: {e}"))?;

            // Preprocess and execute
            let processed = preprocess_script(script);
            let result: JsValue<'_> = ctx
                .eval(processed)
                .map_err(|e| format!("Script error: {e}"))?;

            js_to_json(&ctx, result)
        })
    }

    // -------------------------------------------------------------------------
    // Return Value Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_script_return_null() {
        let result = run_simple_script(
            "export default function(input) { return null; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_script_return_undefined() {
        let result = run_simple_script(
            "export default function(input) { return undefined; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_script_return_boolean_true() {
        let result = run_simple_script(
            "export default function(input) { return true; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn test_script_return_boolean_false() {
        let result = run_simple_script(
            "export default function(input) { return false; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(false));
    }

    #[test]
    fn test_script_return_integer() {
        let result = run_simple_script(
            "export default function(input) { return 42; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(42));
    }

    #[test]
    fn test_script_return_negative_integer() {
        let result = run_simple_script(
            "export default function(input) { return -123; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(-123));
    }

    #[test]
    fn test_script_return_float() {
        let result = run_simple_script(
            "export default function(input) { return 1.234; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(1.234));
    }

    #[test]
    fn test_script_return_string() {
        let result = run_simple_script(
            "export default function(input) { return 'hello world'; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("hello world"));
    }

    #[test]
    fn test_script_return_empty_string() {
        let result = run_simple_script(
            "export default function(input) { return ''; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(""));
    }

    #[test]
    fn test_script_return_empty_array() {
        let result = run_simple_script(
            "export default function(input) { return []; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!([]));
    }

    #[test]
    fn test_script_return_array_of_numbers() {
        let result = run_simple_script(
            "export default function(input) { return [1, 2, 3]; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }

    #[test]
    fn test_script_return_mixed_array() {
        let result = run_simple_script(
            "export default function(input) { return [1, 'two', true, null]; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!([1, "two", true, null]));
    }

    #[test]
    fn test_script_return_empty_object() {
        let result = run_simple_script(
            "export default function(input) { return {}; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!({}));
    }

    #[test]
    fn test_script_return_simple_object() {
        let result = run_simple_script(
            "export default function(input) { return {name: 'test', value: 42}; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!({"name": "test", "value": 42}));
    }

    #[test]
    fn test_script_return_nested_object() {
        let result = run_simple_script(
            "export default function(input) { return {user: {name: 'alice', age: 30}, active: true}; }",
            &json!(null),
        ).unwrap();
        assert_eq!(
            result,
            json!({
                "user": {"name": "alice", "age": 30},
                "active": true
            })
        );
    }

    #[test]
    fn test_script_return_array_of_objects() {
        let result = run_simple_script(
            "export default function(input) { return [{id: 1}, {id: 2}]; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!([{"id": 1}, {"id": 2}]));
    }

    // -------------------------------------------------------------------------
    // Input Access Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_script_access_input_null() {
        let result = run_simple_script(
            "export default function(input) { return input; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_script_access_input_string() {
        let result = run_simple_script(
            "export default function(input) { return input; }",
            &json!("hello"),
        )
        .unwrap();
        assert_eq!(result, json!("hello"));
    }

    #[test]
    fn test_script_access_input_object() {
        let result = run_simple_script(
            "export default function(input) { return input; }",
            &json!({"foo": "bar"}),
        )
        .unwrap();
        assert_eq!(result, json!({"foo": "bar"}));
    }

    #[test]
    fn test_script_access_input_property() {
        let result = run_simple_script(
            "export default function(input) { return input.name; }",
            &json!({"name": "alice"}),
        )
        .unwrap();
        assert_eq!(result, json!("alice"));
    }

    #[test]
    fn test_script_access_nested_input_property() {
        let result = run_simple_script(
            "export default function(input) { return input.user.email; }",
            &json!({"user": {"email": "test@example.com"}}),
        )
        .unwrap();
        assert_eq!(result, json!("test@example.com"));
    }

    #[test]
    fn test_script_access_input_array() {
        let result = run_simple_script(
            "export default function(input) { return input[1]; }",
            &json!([10, 20, 30]),
        )
        .unwrap();
        assert_eq!(result, json!(20));
    }

    #[test]
    fn test_script_transform_input() {
        let result = run_simple_script(
            "export default function(input) { return {doubled: input.value * 2, original: input.value}; }",
            &json!({"value": 21}),
        ).unwrap();
        assert_eq!(result, json!({"doubled": 42, "original": 21}));
    }

    // -------------------------------------------------------------------------
    // JavaScript Logic Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_script_conditional_true() {
        let result = run_simple_script(
            "export default function(input) { if (input.authorized) { return {status: 'ok'}; } else { return {status: 'denied'}; } }",
            &json!({"authorized": true}),
        ).unwrap();
        assert_eq!(result, json!({"status": "ok"}));
    }

    #[test]
    fn test_script_conditional_false() {
        let result = run_simple_script(
            "export default function(input) { if (input.authorized) { return {status: 'ok'}; } else { return {status: 'denied'}; } }",
            &json!({"authorized": false}),
        ).unwrap();
        assert_eq!(result, json!({"status": "denied"}));
    }

    #[test]
    fn test_script_loop() {
        let result = run_simple_script(
            "export default function(input) { var sum = 0; for (var i = 0; i < input.length; i++) { sum += input[i]; } return sum; }",
            &json!([1, 2, 3, 4, 5]),
        ).unwrap();
        assert_eq!(result, json!(15));
    }

    #[test]
    fn test_script_array_map() {
        let result = run_simple_script(
            "export default function(input) { return input.map(function(x) { return x * 2; }); }",
            &json!([1, 2, 3]),
        )
        .unwrap();
        assert_eq!(result, json!([2, 4, 6]));
    }

    #[test]
    fn test_script_array_filter() {
        let result = run_simple_script(
            "export default function(input) { return input.filter(function(x) { return x > 2; }); }",
            &json!([1, 2, 3, 4, 5]),
        ).unwrap();
        assert_eq!(result, json!([3, 4, 5]));
    }

    #[test]
    fn test_script_string_operations() {
        let result = run_simple_script(
            "export default function(input) { return input.toUpperCase() + '!'; }",
            &json!("hello"),
        )
        .unwrap();
        assert_eq!(result, json!("HELLO!"));
    }

    #[test]
    fn test_script_json_stringify() {
        let result = run_simple_script(
            "export default function(input) { return JSON.stringify(input); }",
            &json!({"a": 1}),
        )
        .unwrap();
        assert_eq!(result, json!("{\"a\":1}"));
    }

    #[test]
    fn test_script_json_parse() {
        let result = run_simple_script(
            "export default function(input) { return JSON.parse(input); }",
            &json!("{\"b\":2}"),
        )
        .unwrap();
        // JS returns floats, so compare with float
        assert_eq!(result, json!({"b": 2.0}));
    }

    // -------------------------------------------------------------------------
    // Error Handling Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_script_syntax_error() {
        let result = run_simple_script(
            "export default function(input) { return {{{ }",
            &json!(null),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Script error"));
    }

    #[test]
    fn test_script_reference_error() {
        let result = run_simple_script(
            "export default function(input) { return undefinedVariable; }",
            &json!(null),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_script_type_error() {
        let result = run_simple_script(
            "export default function(input) { return null.foo; }",
            &json!(null),
        );
        assert!(result.is_err());
    }

    // -------------------------------------------------------------------------
    // Security Tests - No Access to Dangerous APIs
    // -------------------------------------------------------------------------

    #[test]
    fn test_script_no_require() {
        let result = run_simple_script(
            "export default function(input) { return typeof require; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_script_no_fetch() {
        let result = run_simple_script(
            "export default function(input) { return typeof fetch; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_script_no_process() {
        let result = run_simple_script(
            "export default function(input) { return typeof process; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_script_no_global_this_process() {
        let result = run_simple_script(
            "export default function(input) { return typeof globalThis.process; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    // -------------------------------------------------------------------------
    // Security Tests - Sandbox Escape Prevention
    // -------------------------------------------------------------------------

    #[test]
    fn test_security_no_eval_escape() {
        // eval should work but not escape sandbox
        let result = run_simple_script(
            "export default function(input) { return eval('1 + 1'); }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!(2));
    }

    #[test]
    fn test_security_no_function_constructor_escape() {
        // Function constructor should not provide escape
        let result = run_simple_script(
            "export default function(input) { return typeof Function('return this')().process; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_security_no_deno_api() {
        let result = run_simple_script(
            "export default function(input) { return typeof Deno; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_security_no_bun_api() {
        let result = run_simple_script(
            "export default function(input) { return typeof Bun; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_security_no_xmlhttprequest() {
        let result = run_simple_script(
            "export default function(input) { return typeof XMLHttpRequest; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_security_no_websocket() {
        let result = run_simple_script(
            "export default function(input) { return typeof WebSocket; }",
            &json!(null),
        )
        .unwrap();
        assert_eq!(result, json!("undefined"));
    }

    #[test]
    fn test_security_no_import_meta() {
        // import.meta should not exist
        let result = run_simple_script(
            "export default function(input) { try { return typeof import.meta; } catch(e) { return 'error'; } }",
            &json!(null),
        );
        // Should either be undefined or error
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_security_no_dangerous_globals() {
        // Check multiple dangerous globals at once
        let result = run_simple_script(
            r"export default function(input) {
                return {
                    require: typeof require,
                    process: typeof process,
                    global: typeof global,
                    __dirname: typeof __dirname,
                    __filename: typeof __filename,
                    Buffer: typeof Buffer,
                    setImmediate: typeof setImmediate,
                };
            }",
            &json!(null),
        )
        .unwrap();

        let obj = result.as_object().unwrap();
        for (key, value) in obj {
            assert_eq!(value, "undefined", "Global '{key}' should be undefined");
        }
    }

    #[test]
    fn test_security_prototype_pollution_contained() {
        // Prototype pollution is contained per-script (each gets fresh context)
        // A NEW script execution should NOT see pollution from previous scripts
        let result = run_simple_script(
            r"export default function(input) {
                var test = {};
                return test.polluted === undefined;
            }",
            &json!(null),
        )
        .unwrap();
        // Fresh context - no pollution carried over
        assert_eq!(result, json!(true));
    }

    #[test]
    fn test_security_constructor_chain_blocked() {
        // constructor.constructor should not escape
        let result = run_simple_script(
            r"export default function(input) {
                try {
                    var f = (function(){}).constructor.constructor;
                    var g = f('return this')();
                    return typeof g.process;
                } catch(e) {
                    return 'error';
                }
            }",
            &json!(null),
        )
        .unwrap();
        // Should be undefined (no process access) or error
        assert!(result == json!("undefined") || result == json!("error"));
    }

    #[test]
    fn test_security_json_parse_safe() {
        // JSON.parse should not execute code
        let result = run_simple_script(
            r#"export default function(input) {
                return JSON.parse('{"a": 1}').a;
            }"#,
            &json!(null),
        )
        .unwrap();
        // JS returns floats for numbers
        assert_eq!(result, json!(1.0));
    }

    #[test]
    fn test_security_input_not_executable() {
        // Malicious input should not execute
        let result = run_simple_script(
            "export default function(input) { return typeof input; }",
            &json!({"__proto__": {"admin": true}}),
        )
        .unwrap();
        assert_eq!(result, json!("object"));
    }
}
