//! Common test utilities for integration tests.
//!
//! Provides two test host implementations:
//! - `TestHost` - Mock HTTP server for fast unit tests (no WASM execution)
//! - `RealTestHost` - Actual mik runtime for full integration tests with WASM
//!
//! # Example: Using RealTestHost for WASM tests
//!
//! ```rust,ignore
//! #[tokio::test]
//! async fn test_wasm_execution() {
//!     let host = RealTestHost::builder()
//!         .with_modules_dir("tests/fixtures/modules")
//!         .start()
//!         .await
//!         .expect("Failed to start real host");
//!
//!     let resp = host.post_json("/run/echo/", &json!({"hello": "world"})).await.unwrap();
//!     assert_eq!(resp.status(), 200);
//! }
//! ```

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};
use rquickjs::{Context as JsContext, Function, Object, Runtime as JsRuntime, Value as JsValue};
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

// Import the real runtime
use mik::runtime::{Runtime, Server};

/// A test host that runs the mikrozen runtime on an ephemeral port.
///
/// The host is automatically shut down when dropped.
///
/// # Example
///
/// ```rust,ignore
/// #[tokio::test]
/// async fn test_health() {
///     let host = TestHost::builder()
///         .with_modules_dir("test_modules")
///         .start()
///         .await
///         .unwrap();
///
///     let resp = host.get("/health").await.unwrap();
///     assert_eq!(resp.status(), 200);
/// }
/// ```
pub struct TestHost {
    /// The address the server is listening on.
    addr: SocketAddr,
    /// Handle to the server task.
    server_handle: JoinHandle<()>,
    /// Shutdown signal sender.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Shared shutdown flag (for graceful shutdown).
    shutdown_flag: Arc<AtomicBool>,
    /// HTTP client for making requests.
    client: reqwest::Client,
}

/// Builder for configuring a `TestHost`.
pub struct TestHostBuilder {
    /// Directory containing .wasm modules.
    modules_dir: Option<PathBuf>,
    /// Directory for static files.
    static_dir: Option<PathBuf>,
    /// Directory for JS scripts.
    scripts_dir: Option<PathBuf>,
    /// Execution timeout in seconds.
    #[allow(dead_code)]
    execution_timeout_secs: u64,
    /// Maximum body size in MB.
    #[allow(dead_code)]
    max_body_size_mb: usize,
}

impl Default for TestHostBuilder {
    fn default() -> Self {
        Self {
            modules_dir: None,
            static_dir: None,
            scripts_dir: None,
            execution_timeout_secs: 10,
            max_body_size_mb: 1,
        }
    }
}

impl TestHostBuilder {
    /// Create a new test host builder with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the modules directory.
    ///
    /// If not set, tests requiring WASM modules will fail.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_modules_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.modules_dir = Some(path.into());
        self
    }

    /// Set the static files directory.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_static_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.static_dir = Some(path.into());
        self
    }

    /// Set the scripts directory.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_scripts_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.scripts_dir = Some(path.into());
        self
    }

    /// Set the execution timeout in seconds.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_execution_timeout(mut self, secs: u64) -> Self {
        self.execution_timeout_secs = secs;
        self
    }

    /// Set the maximum body size in MB.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_max_body_size_mb(mut self, mb: usize) -> Self {
        self.max_body_size_mb = mb;
        self
    }

    /// Start the test host.
    ///
    /// Finds an available port and starts the runtime server.
    /// Returns an error if the server fails to start.
    pub async fn start(self) -> anyhow::Result<TestHost> {
        // Find an available port
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        // Drop the listener to free the port for the actual server
        drop(listener);
        // Small delay to ensure port is fully released (Windows race condition fix)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Create shutdown signal
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = shutdown_flag.clone();

        // Build host configuration
        let modules_dir = self.modules_dir;
        let static_dir = self.static_dir;
        let scripts_dir = self.scripts_dir;

        // Spawn the server task
        let server_handle = tokio::spawn(async move {
            // Wait a moment for the test to set up
            tokio::time::sleep(Duration::from_millis(10)).await;

            // Try to start the actual runtime
            let result = start_test_server(
                addr,
                modules_dir,
                static_dir,
                scripts_dir,
                shutdown_flag_clone,
                shutdown_rx,
            )
            .await;

            if let Err(e) = result {
                eprintln!("Test server error: {e}");
            }
        });

        // Wait for server to be ready (simple health check with retries)
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;

        let health_url = format!("http://{}/health", addr);
        let mut retries = 50; // 5 seconds total with 100ms sleep

        loop {
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => break,
                _ if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Err(e) => {
                    // If server task panicked, we need to abort
                    if server_handle.is_finished() {
                        anyhow::bail!("Test server failed to start");
                    }
                    if retries == 0 {
                        anyhow::bail!("Test server not ready after 5 seconds: {e}");
                    }
                    retries -= 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Ok(_) => {
                    if retries == 0 {
                        anyhow::bail!("Test server health check failed");
                    }
                    retries -= 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
            }
        }

        Ok(TestHost {
            addr,
            server_handle,
            shutdown_tx: Some(shutdown_tx),
            shutdown_flag,
            client,
        })
    }
}

impl TestHost {
    /// Create a new builder for configuring a test host.
    #[must_use]
    pub fn builder() -> TestHostBuilder {
        TestHostBuilder::new()
    }

    /// Get the address the server is listening on.
    #[allow(dead_code)]
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Get the base URL for the server.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Build a URL for the given path.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };
        format!("http://{}{}", self.addr, path)
    }

    /// Get a reference to the HTTP client.
    #[allow(dead_code)]
    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Make a GET request to the given path.
    pub async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        self.client.get(self.url(path)).send().await
    }

    /// Make a POST request to the given path with a JSON body.
    #[allow(dead_code)]
    pub async fn post_json<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> reqwest::Result<reqwest::Response> {
        self.client.post(self.url(path)).json(body).send().await
    }

    /// Make a POST request to the given path with a text body.
    #[allow(dead_code)]
    pub async fn post_text(
        &self,
        path: &str,
        body: impl Into<String>,
    ) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(self.url(path))
            .header("Content-Type", "text/plain")
            .body(body.into())
            .send()
            .await
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        // Signal shutdown
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Abort the server task (it should stop gracefully via shutdown flag)
        self.server_handle.abort();
    }
}

/// Start a test server using the mikrozen runtime.
///
/// This is a minimal implementation that creates a real HTTP server
/// using the runtime module.
async fn start_test_server(
    addr: SocketAddr,
    modules_dir: Option<PathBuf>,
    static_dir: Option<PathBuf>,
    scripts_dir: Option<PathBuf>,
    shutdown_flag: Arc<AtomicBool>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(addr).await?;

    // For now, implement a minimal mock server that handles /health
    // Real tests that need WASM execution should use the full runtime
    // once modules are available

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _remote_addr) = accept_result?;

                if shutdown_flag.load(Ordering::SeqCst) {
                    break;
                }

                let io = TokioIo::new(stream);
                let modules_dir = modules_dir.clone();
                let static_dir = static_dir.clone();
                let scripts_dir = scripts_dir.clone();

                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                        let modules_dir = modules_dir.clone();
                        let static_dir = static_dir.clone();
                        let scripts_dir = scripts_dir.clone();
                        async move {
                            handle_test_request(req, modules_dir, static_dir, scripts_dir).await
                        }
                    });

                    if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                        // Connection errors are expected during shutdown
                        if !e.to_string().contains("connection") {
                            eprintln!("Connection error: {}", e);
                        }
                    }
                });
            }
            _ = &mut shutdown_rx => {
                break;
            }
        }
    }

    Ok(())
}

/// Handle a test request.
///
/// This is a minimal handler that supports /health and basic routing.
/// For full WASM execution tests, the runtime needs actual modules.
async fn handle_test_request(
    req: Request<hyper::body::Incoming>,
    modules_dir: Option<PathBuf>,
    static_dir: Option<PathBuf>,
    scripts_dir: Option<PathBuf>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let path = req.uri().path();

    // Generate request ID (matches real runtime behavior)
    let request_id = Uuid::new_v4();

    // Extract traceparent from incoming header, or generate new (W3C Trace Context)
    let traceparent = if let Some(tp) = req
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
    {
        tp.to_string()
    } else {
        // Generate W3C traceparent format: version-trace_id-parent_id-flags
        // Use UUID for random generation (trace_id = 32 hex, parent_id = first 16 hex of another UUID)
        let trace_uuid = Uuid::new_v4();
        let parent_uuid = Uuid::new_v4();
        let trace_id = trace_uuid.simple().to_string();
        let parent_id = &parent_uuid.simple().to_string()[..16];
        format!("00-{trace_id}-{parent_id}-01")
    };

    // Health endpoint
    if path == "/health" {
        let body = serde_json::json!({
            "status": "ready",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "cache_size": 0,
            "cache_capacity": 100,
            "cache_bytes": 0,
            "cache_max_bytes": 268435456,
            "total_requests": 1,
            "memory": {
                "allocated_bytes": null,
                "limit_per_request_bytes": 134217728
            }
        });

        return Ok(Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .header("X-Request-ID", request_id.to_string())
            .header("traceparent", &traceparent)
            .body(Full::new(Bytes::from(body.to_string())))
            .unwrap());
    }

    // Metrics endpoint
    if path == "/metrics" {
        let metrics = "# HELP mik_requests_total Total number of HTTP requests received\n\
                       # TYPE mik_requests_total counter\n\
                       mik_requests_total 1\n";

        return Ok(Response::builder()
            .status(200)
            .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .header("X-Request-ID", request_id.to_string())
            .header("traceparent", &traceparent)
            .body(Full::new(Bytes::from(metrics)))
            .unwrap());
    }

    // Static file handling
    if path.starts_with("/static/") {
        if let Some(ref dir) = static_dir {
            let file_path = path.strip_prefix("/static/").unwrap_or("");
            let full_path = dir.join(file_path);

            if full_path.exists()
                && full_path.is_file()
                && let Ok(contents) = tokio::fs::read(&full_path).await
            {
                let content_type = mime_guess::from_path(&full_path)
                    .first_or_octet_stream()
                    .to_string();

                return Ok(Response::builder()
                    .status(200)
                    .header("Content-Type", content_type)
                    .body(Full::new(Bytes::from(contents)))
                    .unwrap());
            }

            return Ok(Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("File not found")))
                .unwrap());
        }

        return Ok(Response::builder()
            .status(404)
            .body(Full::new(Bytes::from("Static file serving not enabled")))
            .unwrap());
    }

    // Module handling (/run/*)
    if path.starts_with("/run/") {
        if modules_dir.is_none() {
            return Ok(Response::builder()
                .status(503)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(
                    r#"{"error":"No modules directory configured"}"#,
                )))
                .unwrap());
        }

        // For now, return a placeholder response
        // Real module execution requires the full runtime
        return Ok(Response::builder()
            .status(501)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(r#"{"error":"WASM module execution not implemented in test mode. Use full runtime tests."}"#)))
            .unwrap());
    }

    // Script handling (/script/*)
    if path.starts_with("/script/") {
        return handle_script_request(req, scripts_dir).await;
    }

    // 404 for everything else
    Ok(Response::builder()
        .status(404)
        .body(Full::new(Bytes::from("Not found")))
        .unwrap())
}

// =============================================================================
// Script Handling (for tests)
// =============================================================================

// Thread-local storage for mock host.call() results (reserved for future use)
thread_local! {
    #[allow(dead_code)]
    static HOST_CALL_RESULTS: RefCell<HashMap<String, serde_json::Value>> = RefCell::new(HashMap::new());
}

/// Handle a script request at /script/<name>
async fn handle_script_request(
    req: Request<hyper::body::Incoming>,
    scripts_dir: Option<PathBuf>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    use http_body_util::BodyExt;

    let path = req.uri().path();
    let script_name = path
        .strip_prefix("/script/")
        .unwrap_or("")
        .split('/')
        .next()
        .filter(|s| !s.is_empty());

    let Some(script_name) = script_name else {
        return Ok(Response::builder()
            .status(400)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(r#"{"error":"Missing script name"}"#)))
            .unwrap());
    };

    let Some(ref dir) = scripts_dir else {
        return Ok(Response::builder()
            .status(503)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(r#"{"error":"Scripts not enabled"}"#)))
            .unwrap());
    };

    // Load script file
    let script_file = dir.join(format!("{script_name}.js"));
    let script = match tokio::fs::read_to_string(&script_file).await {
        Ok(s) => s,
        Err(_) => {
            return Ok(Response::builder()
                .status(404)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(format!(
                    r#"{{"error":"Script not found: {}"}}"#,
                    script_name
                ))))
                .unwrap());
        },
    };

    // Read request body
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();

    let input: serde_json::Value = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null)
    };

    // Execute script
    let result = tokio::task::spawn_blocking(move || execute_test_script(&script, &input)).await;

    match result {
        Ok(Ok(value)) => {
            let response = serde_json::json!({
                "result": value,
                "calls_executed": 0
            });
            Ok(Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(
                    serde_json::to_vec(&response).unwrap(),
                )))
                .unwrap())
        },
        Ok(Err(e)) => Ok(Response::builder()
            .status(500)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(format!(r#"{{"error":"{}"}}"#, e))))
            .unwrap()),
        Err(e) => Ok(Response::builder()
            .status(500)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(format!(
                r#"{{"error":"Script panicked: {}"}}"#,
                e
            ))))
            .unwrap()),
    }
}

/// Execute a JavaScript script (similar to run_simple_script in script.rs tests).
fn execute_test_script(
    script: &str,
    input: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let runtime = JsRuntime::new().map_err(|e| format!("Failed to create JS runtime: {e}"))?;

    // Set memory and execution limits for safety
    runtime.set_memory_limit(16 * 1024 * 1024); // 16MB max
    runtime.set_max_stack_size(1024 * 1024); // 1MB stack

    let context =
        JsContext::full(&runtime).map_err(|e| format!("Failed to create JS context: {e}"))?;

    context.with(|ctx| {
        let globals = ctx.globals();

        // Register mock host.call() function
        let host_call_fn = Function::new(ctx.clone(), mock_host_call)
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

        // Preprocess and execute
        let processed = preprocess_script(script);
        let result: JsValue = ctx
            .eval(processed)
            .map_err(|e| format!("Script error: {e}"))?;

        js_to_json(&ctx, result)
    })
}

/// Mock host.call() - returns a simulated response for testing
#[allow(clippy::needless_pass_by_value)]
fn mock_host_call(module: String, options_json: String) -> rquickjs::Result<String> {
    let options: serde_json::Value = serde_json::from_str(&options_json)
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let body = options
        .get("body")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let path = options.get("path").and_then(|v| v.as_str()).unwrap_or("/");
    let method = options
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("POST");

    // Create mock response (echo the request for testing)
    let response = serde_json::json!({
        "status": 200,
        "headers": [["content-type", "application/json"]],
        "body": {
            "module": module,
            "method": method,
            "path": path,
            "received": body
        }
    });

    serde_json::to_string(&response).map_err(|_| rquickjs::Error::Exception)
}

/// Preprocess a script to support `export default function(input) { ... }` syntax.
fn preprocess_script(script: &str) -> String {
    let is_async = script.contains("export default async");

    let transformed = script
        .replace(
            "export default async function",
            "var __default__ = async function",
        )
        .replace("export default function", "var __default__ = function")
        .replace(
            "export default async (",
            "var __default__ = async function(",
        )
        .replace("export default (", "var __default__ = function(");

    if is_async {
        format!(
            "{transformed}\n\
             var __async_result__ = {{ resolved: false, value: null, error: null }};\n\
             __default__(input).then(\n\
               function(r) {{ __async_result__.resolved = true; __async_result__.value = r; }},\n\
               function(e) {{ __async_result__.resolved = true; __async_result__.error = e ? e.toString() : 'Unknown error'; }}\n\
             );\n\
             __async_result__;"
        )
    } else {
        format!("{transformed}\n__default__(input);")
    }
}

/// Convert JavaScript value to serde_json::Value
#[allow(clippy::needless_pass_by_value)]
fn js_to_json<'js>(
    ctx: &rquickjs::Ctx<'js>,
    value: JsValue<'js>,
) -> Result<serde_json::Value, String> {
    use rquickjs::FromJs;

    if value.is_null() || value.is_undefined() {
        Ok(serde_json::Value::Null)
    } else if let Some(b) = value.as_bool() {
        Ok(serde_json::Value::Bool(b))
    } else if let Some(i) = value.as_int() {
        Ok(serde_json::Value::Number(i.into()))
    } else if let Some(f) = value.as_float() {
        Ok(serde_json::Number::from_f64(f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number))
    } else if let Ok(s) = String::from_js(ctx, value.clone()) {
        Ok(serde_json::Value::String(s))
    } else if let Ok(arr) = rquickjs::Array::from_js(ctx, value.clone()) {
        let mut json_arr = Vec::new();
        for i in 0..arr.len() {
            if let Ok(item) = arr.get::<JsValue>(i) {
                json_arr.push(js_to_json(ctx, item)?);
            }
        }
        Ok(serde_json::Value::Array(json_arr))
    } else if let Ok(obj) = Object::from_js(ctx, value.clone()) {
        let mut json_obj = serde_json::Map::new();
        for (key, val) in obj.props::<String, JsValue>().flatten() {
            json_obj.insert(key, js_to_json(ctx, val)?);
        }
        Ok(serde_json::Value::Object(json_obj))
    } else {
        Ok(serde_json::Value::Null)
    }
}

// =============================================================================
// RealTestHost - Actual mik runtime for full integration tests
// =============================================================================

/// A test host that runs the **actual** mik runtime on an ephemeral port.
///
/// Unlike `TestHost` which is a mock, `RealTestHost` starts the real wasmtime
/// runtime with actual WASM module execution, epoch interruption, fuel metering,
/// circuit breakers, and all other production features.
///
/// The host is automatically shut down when dropped.
///
/// # Example
///
/// ```rust,ignore
/// #[tokio::test]
/// async fn test_wasm_echo() {
///     let host = RealTestHost::builder()
///         .with_modules_dir("tests/fixtures/modules")
///         .start()
///         .await
///         .expect("Failed to start real host");
///
///     let resp = host
///         .post_json("/run/echo/", &serde_json::json!({"message": "hello"}))
///         .await
///         .unwrap();
///
///     assert_eq!(resp.status(), 200);
///     let body: serde_json::Value = resp.json().await.unwrap();
///     assert_eq!(body["message"], "hello");
/// }
/// ```
pub struct RealTestHost {
    /// The address the server is listening on.
    addr: SocketAddr,
    /// Handle to the server task.
    server_handle: JoinHandle<()>,
    /// HTTP client for making requests.
    client: reqwest::Client,
}

/// Builder for configuring a `RealTestHost`.
pub struct RealTestHostBuilder {
    /// Directory containing .wasm modules.
    modules_dir: PathBuf,
    /// Directory for user/tenant modules.
    user_modules_dir: Option<PathBuf>,
    /// Directory for static files.
    static_dir: Option<PathBuf>,
    /// Directory for JS scripts.
    scripts_dir: Option<PathBuf>,
    /// Execution timeout in seconds.
    execution_timeout_secs: u64,
    /// Memory limit per request in MB.
    memory_limit_mb: usize,
    /// Maximum concurrent requests.
    max_concurrent_requests: usize,
    /// Maximum body size in MB.
    max_body_size_mb: usize,
    /// Cache size (number of modules).
    cache_size: usize,
}

impl Default for RealTestHostBuilder {
    fn default() -> Self {
        // Default to test fixtures directory
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("modules");

        Self {
            modules_dir: fixtures_dir,
            user_modules_dir: None,
            static_dir: None,
            scripts_dir: None,
            execution_timeout_secs: 5, // Short timeout for tests
            memory_limit_mb: 64,       // 64MB limit for tests
            max_concurrent_requests: 10,
            max_body_size_mb: 1,
            cache_size: 10,
        }
    }
}

impl RealTestHostBuilder {
    /// Create a new builder with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the modules directory containing .wasm files.
    #[must_use]
    pub fn with_modules_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.modules_dir = path.into();
        self
    }

    /// Set the static files directory.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_static_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.static_dir = Some(path.into());
        self
    }

    /// Set the scripts directory for JS orchestration.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_scripts_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.scripts_dir = Some(path.into());
        self
    }

    /// Set the user modules directory for multi-tenant routing.
    ///
    /// When set, enables `/tenant/<tenant-id>/<module>/` routing.
    /// Structure: `user_modules_dir/{tenant-id}/{module}.wasm`
    #[allow(dead_code)]
    #[must_use]
    pub fn with_user_modules_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.user_modules_dir = Some(path.into());
        self
    }

    /// Set the execution timeout in seconds.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_execution_timeout(mut self, secs: u64) -> Self {
        self.execution_timeout_secs = secs;
        self
    }

    /// Set the memory limit per request in MB.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_memory_limit_mb(mut self, mb: usize) -> Self {
        self.memory_limit_mb = mb;
        self
    }

    /// Set the maximum concurrent requests.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_max_concurrent_requests(mut self, max: usize) -> Self {
        self.max_concurrent_requests = max;
        self
    }

    /// Set the module cache size.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_cache_size(mut self, size: usize) -> Self {
        self.cache_size = size;
        self
    }

    /// Set the maximum body size in MB.
    #[allow(dead_code)]
    #[must_use]
    pub fn with_max_body_size_mb(mut self, mb: usize) -> Self {
        self.max_body_size_mb = mb;
        self
    }

    /// Start the real mik runtime.
    ///
    /// This starts the actual wasmtime-based runtime with all production features:
    /// - WASM module loading and execution
    /// - Epoch-based interruption for timeouts
    /// - Fuel metering for CPU limits
    /// - Memory limits via ResourceLimiter
    /// - Circuit breaker per module
    /// - Rate limiting
    pub async fn start(self) -> anyhow::Result<RealTestHost> {
        // Find an available port
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        // Drop the listener to free the port for the actual server
        drop(listener);
        // Small delay to ensure port is fully released (Windows race condition fix)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify modules directory exists and has .wasm files
        if !self.modules_dir.exists() {
            anyhow::bail!(
                "Modules directory does not exist: {}",
                self.modules_dir.display()
            );
        }

        let wasm_files: Vec<_> = std::fs::read_dir(&self.modules_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "wasm"))
            .collect();

        if wasm_files.is_empty() {
            anyhow::bail!(
                "No .wasm files found in {}. Run the fixture build script first.",
                self.modules_dir.display()
            );
        }

        // Build the real runtime using RuntimeBuilder
        let mut builder = Runtime::builder()
            .modules_dir(&self.modules_dir)
            .cache_size(self.cache_size)
            .execution_timeout_secs(self.execution_timeout_secs)
            .memory_limit(self.memory_limit_mb * 1024 * 1024)
            .max_concurrent_requests(self.max_concurrent_requests)
            .max_body_size(self.max_body_size_mb * 1024 * 1024);

        if let Some(static_dir) = self.static_dir {
            builder = builder.static_dir(static_dir);
        }

        if let Some(user_modules_dir) = self.user_modules_dir {
            builder = builder.user_modules_dir(user_modules_dir);
        }

        // Note: scripts_dir is set via manifest, not builder method
        // For now, scripts won't work with RealTestHost

        let runtime = builder.build()?;
        let server = Server::new(runtime, addr);

        // Spawn the server task
        let server_handle = tokio::spawn(async move {
            if let Err(e) = server.serve().await {
                eprintln!("Real test server error: {e}");
            }
        });

        // Wait for server to be ready
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        let health_url = format!("http://{}/health", addr);
        let mut retries = 100; // 10 seconds total

        loop {
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => break,
                _ if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Err(e) => {
                    if server_handle.is_finished() {
                        anyhow::bail!("Real test server failed to start (task finished)");
                    }
                    if retries == 0 {
                        anyhow::bail!("Real test server not ready after 10 seconds: {e}");
                    }
                    retries -= 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
                Ok(_) => {
                    if retries == 0 {
                        anyhow::bail!("Real test server health check returned non-success");
                    }
                    retries -= 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                },
            }
        }

        Ok(RealTestHost {
            addr,
            server_handle,
            client,
        })
    }
}

impl RealTestHost {
    /// Create a new builder for configuring a real test host.
    #[must_use]
    pub fn builder() -> RealTestHostBuilder {
        RealTestHostBuilder::new()
    }

    /// Get the address the server is listening on.
    #[allow(dead_code)]
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Get the base URL for the server.
    #[allow(dead_code)]
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Build a URL for the given path.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };
        format!("http://{}{}", self.addr, path)
    }

    /// Get a reference to the HTTP client.
    #[allow(dead_code)]
    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Make a GET request to the given path.
    pub async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        self.client.get(self.url(path)).send().await
    }

    /// Make a POST request to the given path with a JSON body.
    pub async fn post_json<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> reqwest::Result<reqwest::Response> {
        self.client.post(self.url(path)).json(body).send().await
    }

    /// Make a POST request to the given path with a text body.
    #[allow(dead_code)]
    pub async fn post_text(
        &self,
        path: &str,
        body: impl Into<String>,
    ) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(self.url(path))
            .header("Content-Type", "text/plain")
            .body(body.into())
            .send()
            .await
    }
}

impl Drop for RealTestHost {
    fn drop(&mut self) {
        // Abort the server task
        // The runtime will stop accepting new connections
        self.server_handle.abort();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_host_starts_and_stops() {
        let host = TestHost::builder()
            .start()
            .await
            .expect("Failed to start test host");

        // Verify server is running
        let resp = host.get("/health").await.expect("Failed to get health");
        assert_eq!(resp.status(), 200);

        // Drop will trigger shutdown
        drop(host);
    }

    #[tokio::test]
    async fn test_host_url_building() {
        let host = TestHost::builder()
            .start()
            .await
            .expect("Failed to start test host");

        assert!(host.url("/health").contains("/health"));
        assert!(host.url("health").contains("/health"));
        assert!(host.base_url().starts_with("http://127.0.0.1:"));
    }

    #[tokio::test]
    async fn test_real_host_starts_and_stops() {
        // Check if fixtures exist
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("modules");

        if !fixtures_dir.join("echo.wasm").exists() {
            eprintln!("Skipping: echo.wasm fixture not found");
            return;
        }

        let host = RealTestHost::builder()
            .with_modules_dir(&fixtures_dir)
            .start()
            .await
            .expect("Failed to start real test host");

        // Verify server is running
        let resp = host.get("/health").await.expect("Failed to get health");
        assert_eq!(resp.status(), 200);

        // Drop will trigger shutdown
        drop(host);
    }

    #[tokio::test]
    async fn test_real_host_echo_module() {
        // Check if fixtures exist
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("modules");

        if !fixtures_dir.join("echo.wasm").exists() {
            eprintln!("Skipping: echo.wasm fixture not found");
            return;
        }

        let host = RealTestHost::builder()
            .with_modules_dir(&fixtures_dir)
            .start()
            .await
            .expect("Failed to start real test host");

        // Test the echo module
        let resp = host
            .post_json("/run/echo/", &serde_json::json!({"message": "hello world"}))
            .await
            .expect("Failed to call echo module");

        assert_eq!(resp.status(), 200, "Echo module should return 200");

        let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
        assert_eq!(
            body["message"], "hello world",
            "Echo should return the message"
        );
    }
}
