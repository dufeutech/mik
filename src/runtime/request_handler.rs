//! HTTP request handling for WASI runtime.
//!
//! This module contains the main request handling logic including:
//! - Path validation and routing
//! - Module resolution (single and multi-module modes)
//! - Request body collection with size limits
//! - Request path rewriting

use crate::constants;
use crate::runtime::compression::{accepts_gzip, maybe_compress_response};
use crate::runtime::endpoints::{handle_health_endpoint, handle_metrics_endpoint};
use crate::runtime::error::{self, Error};
use crate::runtime::host_state::HyperCompatibleBody;
use crate::runtime::schema_handler;
use crate::runtime::script;
use crate::runtime::spans::{SpanBuilder, SpanCollector, SpanSummary};
use crate::runtime::static_files::serve_static_file;
use crate::runtime::types::ErrorCategory;
use crate::runtime::wasm_executor::execute_wasm_request;
use crate::runtime::{
    HEALTH_PATH, METRICS_PATH, OPENAPI_PREFIX, RUN_PREFIX, SCRIPT_PREFIX, STATIC_PREFIX,
    SharedState,
};
use anyhow::Result;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::{Request, Response, Uri};
use percent_encoding::percent_decode_str;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::{error, info, warn};
use uuid::Uuid;
use wasmtime::component::Component;

/// Maximum allowed path length (prevents `DoS` via extremely long URLs).
const MAX_PATH_LENGTH: usize = constants::MAX_PATH_LENGTH;

/// Retry-After header value (seconds) for circuit breaker responses.
/// Tells clients to wait 30 seconds before retrying a failed service.
const CIRCUIT_BREAKER_RETRY_AFTER_SECS: &str = "30";

/// Retry-After header value (seconds) for module overload responses.
/// Tells clients to wait 5 seconds before retrying an overloaded module.
const MODULE_OVERLOAD_RETRY_AFTER_SECS: &str = "5";

/// Handle an HTTP request using the shared runtime state.
///
/// This is the main request handler that can be used by both the standard
/// runtime and the high-performance server backends.
///
/// # Arguments
///
/// * `shared` - Shared runtime state containing engine, cache, and config
/// * `req` - The incoming HTTP request
/// * `remote_addr` - Remote address of the client
///
/// # Returns
///
/// The HTTP response to send back to the client.
pub async fn handle_request(
    shared: Arc<SharedState>,
    req: Request<hyper::body::Incoming>,
    remote_addr: SocketAddr,
) -> Result<Response<Full<Bytes>>> {
    let request_id = Uuid::new_v4();
    let start_time = Instant::now();

    let trace_id = req
        .headers()
        .get("x-trace-id")
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| request_id.to_string(), String::from);

    shared.request_counter.fetch_add(1, Ordering::Relaxed);

    let method = req.method();
    let path = req.uri().path();

    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        trace_id = %trace_id,
        method = %method,
        path = %path,
        remote_addr = %remote_addr
    );
    let _enter = span.enter();
    info!("Request started");

    let client_accepts_gzip = accepts_gzip(&req);

    // Handle built-in endpoints
    if path == HEALTH_PATH {
        return handle_health_endpoint(
            &shared,
            &req,
            &request_id,
            &trace_id,
            start_time,
            client_accepts_gzip,
        );
    }
    if path == METRICS_PATH {
        return handle_metrics_endpoint(
            &shared,
            &request_id,
            &trace_id,
            start_time,
            client_accepts_gzip,
        );
    }

    // Create span collector and root request span for timing data
    let span_collector = SpanCollector::new();
    let request_span = SpanBuilder::new("request");
    let request_span_id = request_span.span_id().to_string();

    // Handle the request and log result
    let result = handle_request_inner(
        shared.clone(),
        req,
        remote_addr,
        client_accepts_gzip,
        &trace_id,
        span_collector.clone(),
        &request_span_id,
    )
    .await;
    let duration = start_time.elapsed();

    // Complete root request span based on result
    match &result {
        Ok(_) => span_collector.add(request_span.finish()),
        Err(e) => span_collector.add(request_span.finish_with_error(e.to_string())),
    }

    // Collect and log span summary
    let spans = span_collector.collect();
    if !spans.is_empty() {
        let summary = SpanSummary::new(&trace_id, duration.as_millis() as u64, spans);
        info!(
            trace_id = %summary.trace_id,
            total_ms = summary.total_ms,
            handler_calls = summary.handler_calls,
            spans = ?summary.spans,
            "Request timing spans"
        );
    }

    match &result {
        Ok(response) => {
            let status = response.status().as_u16();
            info!(
                status = status,
                duration_ms = duration.as_millis() as u64,
                "Request completed"
            );
        },
        Err(e) => {
            let category = categorize_error(e);
            error!(
                error = %e,
                category = %category,
                duration_ms = duration.as_millis() as u64,
                "Request failed"
            );
        },
    }

    // Add request ID and trace ID to response headers
    result.map(|mut resp| {
        // Safe: UUID::to_string() always produces valid header characters
        if let Ok(header_value) = request_id.to_string().parse() {
            resp.headers_mut().insert("X-Request-ID", header_value);
        }
        // Add trace ID for distributed tracing
        if let Ok(header_value) = trace_id.parse() {
            resp.headers_mut().insert("X-Trace-ID", header_value);
        }
        resp
    })
}

/// Validates path length and returns a 414 response if too long.
pub(crate) fn validate_path_length(path: &str) -> Option<Response<Full<Bytes>>> {
    if path.len() > MAX_PATH_LENGTH {
        Response::builder()
            .status(414)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(format!(
                r#"{{"error":"URI too long","length":{},"max":{}}}"#,
                path.len(),
                MAX_PATH_LENGTH
            ))))
            .ok()
    } else {
        None
    }
}

/// Validates Content-Length header and returns a 413 response if too large.
pub(crate) fn validate_content_length(
    headers: &hyper::HeaderMap,
    max_body: usize,
) -> Option<Response<Full<Bytes>>> {
    if let Some(content_length) = headers.get("content-length")
        && let Ok(len_str) = content_length.to_str()
        && let Ok(len) = len_str.parse::<usize>()
        && len > max_body
    {
        Response::builder()
            .status(413)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(format!(
                r#"{{"error":"Request body too large","size":{len},"max":{max_body}}}"#
            ))))
            .ok()
    } else {
        None
    }
}

/// Parses the module route from a run path, returning (`module_name`, `handler_path`).
pub(crate) fn parse_module_route(run_path: &str) -> (String, String) {
    if run_path.is_empty() {
        (String::new(), "/".to_string())
    } else if let Some(idx) = run_path.find('/') {
        let module_part = &run_path[..idx];
        let rest = &run_path[idx..];

        // Only decode if percent-encoded characters are present
        let decoded_module = if module_part.contains('%') {
            percent_decode_str(module_part)
                .decode_utf8_lossy()
                .into_owned()
        } else {
            module_part.to_string()
        };

        let decoded_rest = if rest.contains('%') {
            percent_decode_str(rest).decode_utf8_lossy().into_owned()
        } else {
            rest.to_string()
        };

        (decoded_module, decoded_rest)
    } else {
        // No slash found - entire run_path is the module
        let decoded = if run_path.contains('%') {
            percent_decode_str(run_path)
                .decode_utf8_lossy()
                .into_owned()
        } else {
            run_path.to_string()
        };
        (decoded, "/".to_string())
    }
}

/// Result type for module resolution.
pub(crate) enum ModuleResolution {
    /// Successfully resolved module with component and handler path.
    Success {
        component: Arc<Component>,
        handler_path: String,
        module_name: Option<String>,
        module_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    },
    /// Early return with a response (error or not found).
    Response(Response<Full<Bytes>>),
}

/// Resolves the module component from the path.
pub(crate) async fn resolve_module(
    shared: &Arc<SharedState>,
    path: &str,
) -> Result<ModuleResolution> {
    // All module routes must start with /run/
    let Some(run_path) = path.strip_prefix(RUN_PREFIX) else {
        return Ok(ModuleResolution::Response(not_found(
            "Not found. WASM modules are served at /run/<module>/",
        )?));
    };

    let (module, handler_path) = parse_module_route(run_path);

    if module.is_empty() {
        return Ok(ModuleResolution::Response(not_found(
            "No module specified. Use /run/<module>/",
        )?));
    }

    // Single component mode: check if module matches
    if let (Some(comp), Some(expected_name)) =
        (&shared.single_component, &shared.single_component_name)
    {
        if module == *expected_name {
            return Ok(ModuleResolution::Success {
                component: comp.clone(),
                handler_path,
                module_name: Some(module),
                module_permit: None,
            });
        }
        let err = error::Error::module_not_found(&module);
        return Ok(ModuleResolution::Response(error_response(&err)?));
    }

    // Multi-module mode: load from directory
    resolve_multi_module(shared, &module, handler_path).await
}

/// Resolves a module in multi-module mode (circuit breaker, semaphore, loading).
pub(crate) async fn resolve_multi_module(
    shared: &Arc<SharedState>,
    module: &str,
    handler_path: String,
) -> Result<ModuleResolution> {
    // Check circuit breaker before processing
    if let Err(e) = shared.circuit_breaker.check_request(module) {
        warn!("Circuit breaker blocked request to '{}': {}", module, e);
        let err = error::Error::circuit_breaker_open(module);
        let mut resp = error_response(&err)?;
        resp.headers_mut().insert(
            "Retry-After",
            CIRCUIT_BREAKER_RETRY_AFTER_SECS
                .to_string()
                .parse()
                .expect("valid Retry-After header value"),
        );
        return Ok(ModuleResolution::Response(resp));
    }

    // Acquire per-module semaphore permit
    let module_semaphore = shared.get_module_semaphore(module);
    let module_permit = if let Ok(permit) = module_semaphore.try_acquire_owned() {
        Some(permit)
    } else {
        warn!(
            "Module '{}' overloaded (max {} concurrent requests)",
            module, shared.config.max_per_module_requests
        );
        let err = error::Error::rate_limit_exceeded(format!(
            "Module '{}' overloaded (max {} concurrent)",
            module, shared.config.max_per_module_requests
        ));
        let mut resp = error_response(&err)?;
        resp.headers_mut().insert(
            "Retry-After",
            MODULE_OVERLOAD_RETRY_AFTER_SECS
                .to_string()
                .parse()
                .expect("valid Retry-After header value"),
        );
        return Ok(ModuleResolution::Response(resp));
    };

    match shared.get_or_load(module).await {
        Ok(comp) => Ok(ModuleResolution::Success {
            component: comp,
            handler_path,
            module_name: Some(module.to_string()),
            module_permit,
        }),
        Err(e) => {
            warn!("Module load failed: {}", e);
            // Record failure in circuit breaker
            shared.circuit_breaker.record_failure(module);
            // Use typed error for response
            let err = error::Error::module_not_found(module);
            Ok(ModuleResolution::Response(error_response(&err)?))
        },
    }
}

/// Collects request body with size limit, returning 413 if exceeded.
pub(crate) async fn collect_request_body(
    body: hyper::body::Incoming,
    max_body: usize,
) -> Result<Result<Bytes, Response<Full<Bytes>>>> {
    let limited_body = Limited::new(body, max_body);
    match limited_body.collect().await {
        Ok(collected) => Ok(Ok(collected.to_bytes())),
        Err(e) => {
            // Check if this is a size limit error using source chain
            let mut is_limit_error = false;
            let mut current: Option<&dyn std::error::Error> = Some(&*e);
            while let Some(err) = current {
                if err.to_string().contains("length limit") {
                    is_limit_error = true;
                    break;
                }
                current = err.source();
            }
            if is_limit_error {
                Ok(Err(Response::builder()
                    .status(413)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(format!(
                        r#"{{"error":"Request body too large","max":{max_body}}}"#
                    ))))?))
            } else {
                Err(anyhow::anyhow!("Failed to read request body: {e}"))
            }
        },
    }
}

/// Rewrite request to use new path.
pub(crate) fn rewrite_request_path(
    req: Request<hyper::body::Incoming>,
    new_path: impl AsRef<str>,
) -> Result<Request<hyper::body::Incoming>> {
    let new_path = new_path.as_ref();
    let (mut parts, body) = req.into_parts();

    // Build new URI with updated path
    let mut uri_parts = parts.uri.into_parts();
    uri_parts.path_and_query = Some(new_path.parse()?);
    parts.uri = Uri::from_parts(uri_parts)?;

    Ok(Request::from_parts(parts, body))
}

/// Inner request handler with full context.
pub(crate) async fn handle_request_inner(
    shared: Arc<SharedState>,
    req: Request<hyper::body::Incoming>,
    _remote_addr: SocketAddr,
    client_accepts_gzip: bool,
    trace_id: &str,
    span_collector: SpanCollector,
    parent_span_id: &str,
) -> Result<Response<Full<Bytes>>> {
    let path = req.uri().path();
    let max_body = shared.max_body_size_bytes;

    // Validate path length (DoS prevention)
    if let Some(resp) = validate_path_length(path) {
        return Ok(resp);
    }

    // Check Content-Length header (fast path for body size limit)
    if let Some(resp) = validate_content_length(req.headers(), max_body) {
        return Ok(resp);
    }

    // Handle static file requests
    if path.starts_with(STATIC_PREFIX) {
        return match &shared.static_dir {
            Some(dir) => serve_static_file(dir, path)
                .await
                .map(|resp| maybe_compress_response(resp, client_accepts_gzip)),
            None => not_found("Static file serving not enabled"),
        };
    }

    // Handle OpenAPI schema requests: /openapi/<module>
    if let Some(module_name) = path.strip_prefix(OPENAPI_PREFIX) {
        // Strip trailing slash if present
        let module_name = module_name.trim_end_matches('/');
        if module_name.is_empty() {
            return not_found("No module specified. Use /openapi/<module>");
        }
        let schema_path = schema_handler::get_schema_path(&shared.modules_dir, module_name);
        return Ok(schema_handler::serve_static_schema(&schema_path));
    }

    // Handle script requests
    if path.starts_with(SCRIPT_PREFIX) {
        let script_path = path.to_string();
        return match script::handle_script_request(
            shared.clone(),
            req,
            &script_path,
            trace_id,
            span_collector,
            parent_span_id,
        )
        .await
        {
            Ok(resp) => Ok(maybe_compress_response(resp, client_accepts_gzip)),
            Err(e) => {
                warn!("Script error: {}", e);
                let err = error::Error::script_error(&script_path, e.to_string());
                error_response(&err)
            },
        };
    }

    // Resolve module component
    let (component, handler_path, module_name, module_permit) =
        match resolve_module(&shared, path).await? {
            ModuleResolution::Success {
                component,
                handler_path,
                module_name,
                module_permit,
            } => (component, handler_path, module_name, module_permit),
            ModuleResolution::Response(resp) => return Ok(resp),
        };

    // Rewrite the request URI and collect body
    let req = rewrite_request_path(req, handler_path)?;
    let (parts, body) = req.into_parts();
    let body_bytes = match collect_request_body(body, max_body).await? {
        Ok(bytes) => bytes,
        Err(resp) => return Ok(resp),
    };
    let req = Request::from_parts(parts, HyperCompatibleBody(Full::new(body_bytes)));

    // Execute WASM request (keep module_permit in scope for semaphore)
    let _module_permit = module_permit;
    let result = execute_wasm_request(shared.clone(), component, req).await;

    // Record success/failure in circuit breaker
    if let Some(ref module) = module_name {
        match &result {
            Ok(_) => shared.circuit_breaker.record_success(module),
            Err(_) => shared.circuit_breaker.record_failure(module),
        }
    }

    result.map(|resp| maybe_compress_response(resp, client_accepts_gzip))
}

/// Categorize an error by walking the error chain.
///
/// Uses `to_string()` instead of `format!("{:?}")` to avoid potential panics
/// from buggy Debug implementations.
pub(crate) fn categorize_error(error: &anyhow::Error) -> ErrorCategory {
    // Walk the entire error chain for robust matching
    let mut current: &dyn std::error::Error = error.as_ref();
    loop {
        let msg = current.to_string();

        if msg.contains("timed out") {
            return ErrorCategory::Timeout;
        }
        if msg.contains("Module not found") || msg.contains("Failed to load") {
            return ErrorCategory::ModuleLoad;
        }
        if msg.contains("Not found") || msg.contains("Invalid") {
            return ErrorCategory::InvalidRequest;
        }
        if msg.contains("Failed to instantiate") {
            return ErrorCategory::Instantiation;
        }
        if msg.contains("Handler call failed") {
            return ErrorCategory::Execution;
        }
        if msg.contains("static file") {
            return ErrorCategory::StaticFile;
        }

        // Walk to next error in chain
        match current.source() {
            Some(source) => current = source,
            None => break,
        }
    }

    ErrorCategory::Internal
}

/// Create a 404 Not Found response.
pub(crate) fn not_found(message: &str) -> Result<Response<Full<Bytes>>> {
    Ok(Response::builder()
        .status(404)
        .body(Full::new(Bytes::from(message.to_string())))?)
}

/// Create an error response from a typed runtime error.
///
/// Uses the error's `status_code()` method for the HTTP status and
/// provides a JSON error body.
pub(crate) fn error_response(err: &Error) -> Result<Response<Full<Bytes>>> {
    let status = err.status_code();
    let body = serde_json::json!({
        "error": err.to_string(),
        "status": status
    });
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))?)
}

/// Convert a typed error to its error category for logging.
impl From<&Error> for ErrorCategory {
    fn from(err: &Error) -> Self {
        match err {
            Error::ModuleNotFound { .. } | Error::ModuleLoadFailed { .. } => Self::ModuleLoad,
            Error::ExecutionTimeout { .. } => Self::Timeout,
            Error::PathTraversal { .. } | Error::InvalidRequest(_) => Self::InvalidRequest,
            Error::ScriptNotFound { .. } | Error::ScriptError { .. } => Self::Script,
            Error::CircuitBreakerOpen { .. } | Error::RateLimitExceeded { .. } => Self::Reliability,
            Error::MemoryLimitExceeded { .. } => Self::Execution,
            _ => Self::Internal,
        }
    }
}
