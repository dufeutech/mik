//! `JavaScript` runtime setup and execution.
//!
//! Handles rquickjs Runtime/Context creation and script execution.

use rquickjs::{Context as JsContext, FromJs, Function, Object, Runtime, Value as JsValue};
use std::sync::Arc;

use super::bindings::{HostBridge, HostBridgeGuard, native_host_call};
use super::context::{js_to_json, preprocess_script};

/// Maximum iterations to wait for async Promise resolution.
const MAX_ASYNC_ITERATIONS: usize = 10000;

/// Run `JavaScript` with `host.call()` capability (blocking).
pub(crate) fn run_js_script(
    script: &str,
    input: &serde_json::Value,
    bridge: Arc<HostBridge>,
) -> std::result::Result<serde_json::Value, String> {
    // Set thread-local bridge with RAII guard (clears on drop, even on panic)
    let _guard = HostBridgeGuard::set(bridge);

    // Create QuickJS runtime
    let runtime = Runtime::new().map_err(|e| format!("Failed to create JS runtime: {e}"))?;
    let context =
        JsContext::full(&runtime).map_err(|e| format!("Failed to create JS context: {e}"))?;

    context.with(|ctx| {
        let globals = ctx.globals();

        // Register native __host_call function
        let host_call_fn = Function::new(ctx.clone(), native_host_call)
            .map_err(|e| format!("Failed to create host_call function: {e}"))?;

        globals
            .set("__host_call", host_call_fn)
            .map_err(|e| format!("Failed to set __host_call: {e}"))?;

        // Create host.call() wrapper in JavaScript
        let host_wrapper = r"
            var host = {
                call: function(module, options) {
                    options = options || {};
                    var result = __host_call(module, JSON.stringify(options));
                    return JSON.parse(result);
                }
            };
        ";
        ctx.eval::<(), _>(host_wrapper)
            .map_err(|e| format!("Failed to create host wrapper: {e}"))?;

        // Set input object
        let input_json =
            serde_json::to_string(&input).map_err(|e| format!("Failed to serialize input: {e}"))?;
        let input_script = format!("var input = {input_json};");
        ctx.eval::<(), _>(input_script.as_str())
            .map_err(|e| format!("Failed to set input: {e}"))?;

        // Preprocess and execute the script
        let wrapped_script = preprocess_script(script);
        let result: JsValue<'_> = ctx
            .eval(wrapped_script.as_str())
            .map_err(|e| format!("Script error: {e}"))?;

        // Check if this is an async result object
        if let Ok(obj) = Object::from_js(&ctx, result.clone())
            && obj.get::<_, bool>("resolved").is_ok()
        {
            // This is an async script result, run pending jobs until resolved
            return resolve_async_result(&runtime, &ctx, &obj);
        }

        // Synchronous result - convert directly
        js_to_json(&ctx, result)
    })
    // _guard is dropped here, clearing the thread-local bridge
}

/// Resolve an async script result by running pending jobs.
fn resolve_async_result<'js>(
    runtime: &Runtime,
    ctx: &rquickjs::Ctx<'js>,
    result_obj: &Object<'js>,
) -> std::result::Result<serde_json::Value, String> {
    for _ in 0..MAX_ASYNC_ITERATIONS {
        // Check if resolved
        let resolved = result_obj.get::<_, bool>("resolved").unwrap_or(false);

        if resolved {
            // Check for error
            if let Ok(error) = result_obj.get::<_, String>("error")
                && !error.is_empty()
            {
                return Err(format!("Async script error: {error}"));
            }

            // Return the value
            let value: JsValue<'_> = result_obj
                .get("value")
                .map_err(|e| format!("Failed to get async result value: {e}"))?;

            return js_to_json(ctx, value);
        }

        // Run pending jobs (Promise callbacks)
        match runtime.execute_pending_job() {
            Ok(true) => {
                // Job executed, continue checking
            },
            Ok(false) => {
                // No more pending jobs but still not resolved - check once more
                let resolved = result_obj.get::<_, bool>("resolved").unwrap_or(false);
                if resolved {
                    continue;
                }
                // Give a small sleep to allow any internal scheduling
                std::thread::sleep(std::time::Duration::from_micros(100));
            },
            Err(e) => {
                return Err(format!("JS job execution error: {e:?}"));
            },
        }
    }

    Err("Async script timeout: Promise did not resolve within iteration limit".to_string())
}
