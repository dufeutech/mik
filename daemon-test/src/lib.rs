//! Daemon Test Handler - Tests HTTP calls to mik daemon embedded services
//!
//! This handler tests the integration between WASM handlers and the mik daemon
//! by making HTTP requests to KV, SQL, Storage, Cron, and Metrics services.
//!
//! ## Usage
//!
//! 1. Start the daemon: `mik api --port 9919`
//! 2. Build this handler: `cargo component build --release`
//! 3. Run with mik: `mik run target/wasm32-wasip1/release/daemon_test.wasm`
//! 4. Test endpoints:
//!    - GET /              - Service info
//!    - GET /test/all      - Test all services
//!    - GET /test/kv       - Test KV service
//!    - GET /test/sql      - Test SQL service
//!    - GET /test/storage  - Test Storage service
//!    - GET /test/cron     - Test Cron service
//!    - GET /test/metrics  - Test Metrics endpoint

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::wasi::http::{outgoing_handler, types as http_types};

struct Component;

/// Daemon HTTP API base URL (localhost:9919 is the default daemon port)
const DAEMON_HOST: &str = "127.0.0.1:9919";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_default();
        let path = path.split('?').next().unwrap_or("");

        let (status, body) = match path {
            "/" | "" => home(),
            "/test/all" => test_all(),
            "/test/kv" => test_kv(),
            "/test/sql" => test_sql(),
            "/test/storage" => test_storage(),
            "/test/cron" => test_cron(),
            "/test/metrics" => test_metrics(),
            "/health" => health_check(),
            _ => not_found(),
        };

        send_response(response_out, status, &body);
    }
}

fn send_response(response_out: ResponseOutparam, status: u16, body: &str) {
    let headers = Fields::new();
    let _ = headers.append(&"content-type".to_string(), &b"application/json".to_vec());

    let response = OutgoingResponse::new(headers);
    response.set_status_code(status).unwrap();

    let outgoing_body = response.body().unwrap();
    ResponseOutparam::set(response_out, Ok(response));

    let stream = outgoing_body.write().unwrap();
    stream.blocking_write_and_flush(body.as_bytes()).unwrap();
    drop(stream);
    OutgoingBody::finish(outgoing_body, None).unwrap();
}

fn home() -> (u16, String) {
    (200, r#"{
  "service": "daemon-test",
  "version": "0.1.0",
  "description": "Tests mik daemon embedded services via HTTP",
  "daemon_url": "http://127.0.0.1:9919",
  "endpoints": [
    "/",
    "/health",
    "/test/all",
    "/test/kv",
    "/test/sql",
    "/test/storage",
    "/test/cron",
    "/test/metrics"
  ]
}"#.to_string())
}

fn not_found() -> (u16, String) {
    (404, r#"{"error": "Not Found"}"#.to_string())
}

fn health_check() -> (u16, String) {
    match make_request("GET", "/health", None) {
        Ok((status, body)) => {
            let result = format!(
                r#"{{"daemon_reachable": true, "daemon_status": {}, "daemon_response": {}}}"#,
                status, body
            );
            (200, result)
        }
        Err(e) => {
            let result = format!(
                r#"{{"daemon_reachable": false, "error": "{}"}}"#,
                escape_json(&e)
            );
            (503, result)
        }
    }
}

/// Test all services at once
fn test_all() -> (u16, String) {
    let kv = test_kv_inner();
    let sql = test_sql_inner();
    let storage = test_storage_inner();
    let cron = test_cron_inner();
    let metrics = test_metrics_inner();

    let all_success = kv.0 && sql.0 && storage.0 && cron.0 && metrics.0;

    let result = format!(
        r#"{{"test": "all", "kv": {}, "sql": {}, "storage": {}, "cron": {}, "metrics": {}, "success": {}}}"#,
        kv.1, sql.1, storage.1, cron.1, metrics.1, all_success
    );

    (if all_success { 200 } else { 502 }, result)
}

// ============================================================================
// KV Service Tests
// ============================================================================

fn test_kv() -> (u16, String) {
    let (success, json) = test_kv_inner();
    (if success { 200 } else { 502 }, json)
}

fn test_kv_inner() -> (bool, String) {
    // 1. Set a value
    let set_body = r#"{"value": "hello-from-wasm", "ttl": 3600}"#;
    let set_result = make_request("PUT", "/kv/wasm-test-key", Some(set_body.as_bytes()));

    let set_ok = match &set_result {
        Ok((status, _)) => *status == 200 || *status == 201 || *status == 204,
        Err(_) => false,
    };

    if !set_ok {
        let err = set_result.err().unwrap_or_else(|| "Set failed".to_string());
        return (false, format!(r#"{{"test": "kv", "set_error": "{}", "success": false}}"#, escape_json(&err)));
    }

    // 2. Get the value back
    let get_result = make_request("GET", "/kv/wasm-test-key", None);

    match get_result {
        Ok((status, body)) => {
            let success = status == 200;
            (
                success,
                format!(
                    r#"{{"test": "kv", "set": "ok", "get_status": {}, "value": {}, "success": {}}}"#,
                    status, body, success
                ),
            )
        }
        Err(e) => (
            false,
            format!(r#"{{"test": "kv", "set": "ok", "get_error": "{}", "success": false}}"#, escape_json(&e)),
        ),
    }
}

// ============================================================================
// SQL Service Tests
// ============================================================================

fn test_sql() -> (u16, String) {
    let (success, json) = test_sql_inner();
    (if success { 200 } else { 502 }, json)
}

fn test_sql_inner() -> (bool, String) {
    // 1. Create table
    let create_sql = r#"{"sql": "CREATE TABLE IF NOT EXISTS wasm_test (id INTEGER PRIMARY KEY, message TEXT, created_at TEXT DEFAULT CURRENT_TIMESTAMP)"}"#;
    let _ = make_request("POST", "/sql/execute", Some(create_sql.as_bytes()));

    // 2. Insert data
    let insert_sql = r#"{"sql": "INSERT INTO wasm_test (message) VALUES ('hello-from-wasm')"}"#;
    let insert_result = make_request("POST", "/sql/execute", Some(insert_sql.as_bytes()));

    let insert_ok = match &insert_result {
        Ok((status, _)) => *status == 200,
        Err(_) => false,
    };

    if !insert_ok {
        let err = insert_result.err().unwrap_or_else(|| "Insert failed".to_string());
        return (false, format!(r#"{{"test": "sql", "insert_error": "{}", "success": false}}"#, escape_json(&err)));
    }

    // 3. Query data
    let select_sql = r#"{"sql": "SELECT * FROM wasm_test ORDER BY id DESC LIMIT 1"}"#;
    let select_result = make_request("POST", "/sql/query", Some(select_sql.as_bytes()));

    match select_result {
        Ok((status, body)) => {
            let success = status == 200;
            (
                success,
                format!(
                    r#"{{"test": "sql", "insert": "ok", "select_status": {}, "rows": {}, "success": {}}}"#,
                    status, body, success
                ),
            )
        }
        Err(e) => (
            false,
            format!(r#"{{"test": "sql", "insert": "ok", "select_error": "{}", "success": false}}"#, escape_json(&e)),
        ),
    }
}

// ============================================================================
// Storage Service Tests
// ============================================================================

fn test_storage() -> (u16, String) {
    let (success, json) = test_storage_inner();
    (if success { 200 } else { 502 }, json)
}

fn test_storage_inner() -> (bool, String) {
    let test_content = b"Hello from WASM storage test!";

    // 1. PUT object
    let put_result = make_request("PUT", "/storage/wasm-test/hello.txt", Some(test_content));

    let put_ok = match &put_result {
        Ok((status, _)) => *status == 200 || *status == 201 || *status == 204,
        Err(_) => false,
    };

    if !put_ok {
        let err = put_result.err().unwrap_or_else(|| "Put failed".to_string());
        return (false, format!(r#"{{"test": "storage", "put_error": "{}", "success": false}}"#, escape_json(&err)));
    }

    // 2. GET object
    let get_result = make_request("GET", "/storage/wasm-test/hello.txt", None);

    match get_result {
        Ok((status, body)) => {
            let content_matches = body.contains("Hello from WASM");
            let success = status == 200 && content_matches;
            (
                success,
                format!(
                    r#"{{"test": "storage", "put": "ok", "get_status": {}, "content_matches": {}, "success": {}}}"#,
                    status, content_matches, success
                ),
            )
        }
        Err(e) => (
            false,
            format!(r#"{{"test": "storage", "put": "ok", "get_error": "{}", "success": false}}"#, escape_json(&e)),
        ),
    }
}

// ============================================================================
// Cron Service Tests
// ============================================================================

fn test_cron() -> (u16, String) {
    let (success, json) = test_cron_inner();
    (if success { 200 } else { 502 }, json)
}

fn test_cron_inner() -> (bool, String) {
    // List cron jobs (read-only test)
    let list_result = make_request("GET", "/cron", None);

    match list_result {
        Ok((status, body)) => {
            let success = status == 200;
            (
                success,
                format!(
                    r#"{{"test": "cron", "list_status": {}, "jobs": {}, "success": {}}}"#,
                    status, body, success
                ),
            )
        }
        Err(e) => (
            false,
            format!(r#"{{"test": "cron", "list_error": "{}", "success": false}}"#, escape_json(&e)),
        ),
    }
}

// ============================================================================
// Metrics Service Tests
// ============================================================================

fn test_metrics() -> (u16, String) {
    let (success, json) = test_metrics_inner();
    (if success { 200 } else { 502 }, json)
}

fn test_metrics_inner() -> (bool, String) {
    // Get Prometheus metrics
    let metrics_result = make_request("GET", "/metrics", None);

    match metrics_result {
        Ok((status, body)) => {
            // Check if it looks like Prometheus format
            let has_metrics = body.contains("mik_") || body.contains("# HELP") || body.contains("# TYPE");
            let success = status == 200;
            (
                success,
                format!(
                    r#"{{"test": "metrics", "status": {}, "has_prometheus_format": {}, "sample": "{}", "success": {}}}"#,
                    status,
                    has_metrics,
                    escape_json(&body.chars().take(100).collect::<String>()),
                    success
                ),
            )
        }
        Err(e) => (
            false,
            format!(r#"{{"test": "metrics", "error": "{}", "success": false}}"#, escape_json(&e)),
        ),
    }
}

// ============================================================================
// HTTP Client Helper
// ============================================================================

fn make_request(method: &str, path: &str, body: Option<&[u8]>) -> Result<(u16, String), String> {
    use http_types::{Fields, Method, OutgoingBody, OutgoingRequest, Scheme};

    let headers = Fields::new();

    if body.is_some() {
        headers
            .append(&"content-type".to_string(), &b"application/json".to_vec())
            .map_err(|_| "Failed to set content-type")?;
    }

    let request = OutgoingRequest::new(headers);

    let wasi_method = match method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "HEAD" => Method::Head,
        _ => Method::Get,
    };

    request.set_method(&wasi_method).map_err(|_| "Failed to set method")?;
    request.set_scheme(Some(&Scheme::Http)).map_err(|_| "Failed to set scheme")?;
    request.set_authority(Some(DAEMON_HOST)).map_err(|_| "Failed to set authority")?;
    request.set_path_with_query(Some(path)).map_err(|_| "Failed to set path")?;

    if let Some(body_bytes) = body {
        let outgoing_body = request.body().map_err(|_| "Failed to get body")?;
        let stream = outgoing_body.write().map_err(|_| "Failed to get write stream")?;
        stream.blocking_write_and_flush(body_bytes).map_err(|_| "Failed to write body")?;
        drop(stream);
        OutgoingBody::finish(outgoing_body, None).map_err(|_| "Failed to finish body")?;
    }

    let future_response = outgoing_handler::handle(request, None)
        .map_err(|e| format!("HTTP request failed: {:?}", e))?;

    let response = loop {
        if let Some(result) = future_response.get() {
            break result
                .map_err(|_| "Response error")?
                .map_err(|e| format!("HTTP error: {:?}", e))?;
        }
        future_response.subscribe().block();
    };

    let status = response.status();

    let incoming_body = response.consume().map_err(|_| "Failed to consume body")?;
    let stream = incoming_body.stream().map_err(|_| "Failed to get body stream")?;

    let mut body_bytes = Vec::new();
    loop {
        match stream.blocking_read(65536) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    break;
                }
                body_bytes.extend_from_slice(&chunk);
            }
            Err(_) => break,
        }
    }

    let body_str = String::from_utf8(body_bytes).unwrap_or_default();
    Ok((status, body_str))
}

// ============================================================================
// Helpers
// ============================================================================

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

bindings::export!(Component with_types_in bindings);
