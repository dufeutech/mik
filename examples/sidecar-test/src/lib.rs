//! Sidecar Test Handler - Tests HTTP calls to all mikcar sidecars
//!
//! This handler tests the integration between mik runtime and mikcar sidecars
//! by making HTTP requests to storage, kv, and sql services.
//!
//! Uses the mikrozen SDK for routing and responses.

#[allow(warnings)]
mod bindings;

use bindings::exports::mik::core::handler::{self, Guest, Response};
use bindings::wasi::http::{outgoing_handler, types as http_types};
use mik_sdk::prelude::*;

// Sidecar configuration
// Use localhost for local testing, or "mikcar:3001" when running in docker-compose
const SIDECAR_HOST: &str = "localhost:3001";
const AUTH_TOKEN: &str = "Bearer dev-token";

router! {
    "/" | "" => home,
    "/test/all" => test_all,
    "/test/storage" => test_storage,
    "/test/kv" => test_kv,
    "/test/sql" => test_sql,
    "/test/sql-script" => test_sql_script,
    "/test/sql-script-rollback" => test_sql_script_rollback,
}

fn home(_req: &Request) -> Response {
    ok!({
        "service": "sidecar-test",
        "version": "0.4.0",
        "endpoints": ["/", "/test/all", "/test/storage", "/test/kv", "/test/sql", "/test/sql-script", "/test/sql-script-rollback"]
    })
}

// =============================================================================
// Test All Sidecars
// =============================================================================

fn test_all(_req: &Request) -> Response {
    let storage = test_storage_inner();
    let kv = test_kv_inner();
    let sql = test_sql_inner();

    let all_success = storage.success && kv.success && sql.success;

    Response {
        status: if all_success { 200 } else { 502 },
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(format!(
            r#"{{"test":"all","storage":{},"kv":{},"sql":{},"success":{}}}"#,
            storage.json, kv.json, sql.json, all_success
        ).into_bytes()),
    }
}

// =============================================================================
// Storage Tests (S3/MinIO)
// =============================================================================

fn test_storage(_req: &Request) -> Response {
    let result = test_storage_inner();
    Response {
        status: if result.success { 200 } else { 502 },
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(result.json.into_bytes()),
    }
}

fn test_storage_inner() -> TestResult {
    let test_content = "hello-from-wasm-storage";

    // PUT object
    let put = http_request("PUT", "/storage/object/wasm-test.txt", Some(test_content.as_bytes()));
    let put_status = match &put {
        Ok(r) => r.status,
        Err(e) => return TestResult::error("storage", &format!("put_error: {e}")),
    };

    // GET object
    let get = http_request("GET", "/storage/object/wasm-test.txt", None);
    match get {
        Ok(r) => {
            let success = (put_status == 200 || put_status == 201) && r.status == 200 && r.body == test_content;
            TestResult {
                success,
                json: format!(
                    r#"{{"test":"storage","put_status":{},"get_status":{},"value":"{}","success":{}}}"#,
                    put_status, r.status, r.body.replace('"', "\\\""), success
                ),
            }
        }
        Err(e) => TestResult::error("storage", &format!("get_error: {e}")),
    }
}

// =============================================================================
// KV Tests (Redis)
// =============================================================================

fn test_kv(_req: &Request) -> Response {
    let result = test_kv_inner();
    Response {
        status: if result.success { 200 } else { 502 },
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(result.json.into_bytes()),
    }
}

fn test_kv_inner() -> TestResult {
    // SET value
    let set = http_request("POST", "/kv/set/wasm-test-key", Some(br#""hello-from-wasm-kv""#));
    let set_status = match &set {
        Ok(r) => r.status,
        Err(e) => return TestResult::error("kv", &format!("set_error: {e}")),
    };

    // GET value
    let get = http_request("GET", "/kv/get/wasm-test-key", None);
    match get {
        Ok(r) => {
            let success = set_status == 200 && r.status == 200;
            TestResult {
                success,
                json: format!(
                    r#"{{"test":"kv","set_status":{},"get_status":{},"value":"{}","success":{}}}"#,
                    set_status, r.status, r.body.replace('"', "\\\""), success
                ),
            }
        }
        Err(e) => TestResult::error("kv", &format!("get_error: {e}")),
    }
}

// =============================================================================
// SQL Tests (PostgreSQL)
// =============================================================================

fn test_sql(_req: &Request) -> Response {
    let result = test_sql_inner();
    Response {
        status: if result.success { 200 } else { 502 },
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(result.json.into_bytes()),
    }
}

fn test_sql_inner() -> TestResult {
    // Create table
    let create_sql = r#"{"sql":"CREATE TABLE IF NOT EXISTS wasm_test (id SERIAL PRIMARY KEY, message TEXT, created_at TIMESTAMP DEFAULT NOW())"}"#;
    let _ = http_request("POST", "/sql/execute", Some(create_sql.as_bytes()));

    // Insert
    let insert_sql = r#"{"sql":"INSERT INTO wasm_test (message) VALUES ('hello-from-wasm-sql') RETURNING id, message"}"#;
    let insert = http_request("POST", "/sql/query", Some(insert_sql.as_bytes()));
    let insert_status = match &insert {
        Ok(r) => r.status,
        Err(e) => return TestResult::error("sql", &format!("insert_error: {e}")),
    };

    // Select
    let select_sql = r#"{"sql":"SELECT * FROM wasm_test ORDER BY id DESC LIMIT 1"}"#;
    let select = http_request("POST", "/sql/query", Some(select_sql.as_bytes()));
    match select {
        Ok(r) => {
            let success = insert_status == 200 && r.status == 200;
            TestResult {
                success,
                json: format!(
                    r#"{{"test":"sql","insert_status":{},"select_status":{},"response":"{}","success":{}}}"#,
                    insert_status, r.status, r.body.replace('"', "\\\""), success
                ),
            }
        }
        Err(e) => TestResult::error("sql", &format!("select_error: {e}")),
    }
}

// =============================================================================
// SQL Script Tests (JavaScript with Transactions)
// =============================================================================

fn test_sql_script(_req: &Request) -> Response {
    let result = test_sql_script_inner();
    Response {
        status: if result.success { 200 } else { 502 },
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(result.json.into_bytes()),
    }
}

fn test_sql_script_inner() -> TestResult {
    // Test: Fund transfer with balance check
    // This is a financial transaction that MUST be atomic

    // First, set up accounts table
    let setup_sql = r#"{"sql":"CREATE TABLE IF NOT EXISTS accounts (id SERIAL PRIMARY KEY, name TEXT, balance DECIMAL(10,2))"}"#;
    let _ = http_request("POST", "/sql/execute", Some(setup_sql.as_bytes()));

    // Clear and insert test data
    let clear_sql = r#"{"sql":"DELETE FROM accounts"}"#;
    let _ = http_request("POST", "/sql/execute", Some(clear_sql.as_bytes()));

    let insert_sql = r#"{"sql":"INSERT INTO accounts (name, balance) VALUES ('Alice', 1000.00), ('Bob', 500.00)"}"#;
    let _ = http_request("POST", "/sql/execute", Some(insert_sql.as_bytes()));

    // Now test script: transfer $300 from Alice to Bob
    let script = r#"{
        "script": "var from = sql.query('SELECT id, balance FROM accounts WHERE name = $1', ['Alice']); var to = sql.query('SELECT id FROM accounts WHERE name = $1', ['Bob']); if (from.length === 0 || to.length === 0) throw new Error('Account not found'); if (parseFloat(from[0].balance) < 300) throw new Error('Insufficient funds'); sql.execute('UPDATE accounts SET balance = balance - $1 WHERE id = $2', [300, from[0].id]); sql.execute('UPDATE accounts SET balance = balance + $1 WHERE id = $2', [300, to[0].id]); return {success: true, transferred: 300};",
        "input": {}
    }"#;

    let result = http_request("POST", "/sql/script", Some(script.as_bytes()));

    match result {
        Ok(r) => {
            // Verify balances after transfer
            let alice_check = r#"{"sql":"SELECT balance FROM accounts WHERE name = 'Alice'"}"#;
            let bob_check = r#"{"sql":"SELECT balance FROM accounts WHERE name = 'Bob'"}"#;

            let alice = http_request("POST", "/sql/query", Some(alice_check.as_bytes()));
            let bob = http_request("POST", "/sql/query", Some(bob_check.as_bytes()));

            let alice_balance = alice.map(|r| r.body).unwrap_or_default();
            let bob_balance = bob.map(|r| r.body).unwrap_or_default();

            // Alice should have 700, Bob should have 800
            let alice_ok = alice_balance.contains("700");
            let bob_ok = bob_balance.contains("800");

            let success = r.status == 200 && alice_ok && bob_ok;
            TestResult {
                success,
                json: format!(
                    r#"{{"test":"sql_script","script_status":{},"script_response":"{}","alice_balance":"{}","bob_balance":"{}","success":{}}}"#,
                    r.status, r.body.replace('"', "\\\""), alice_balance.replace('"', "\\\""), bob_balance.replace('"', "\\\""), success
                ),
            }
        }
        Err(e) => TestResult::error("sql_script", &format!("script_error: {e}")),
    }
}

fn test_sql_script_rollback(_req: &Request) -> Response {
    let result = test_sql_script_rollback_inner();
    Response {
        status: if result.success { 200 } else { 502 },
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(result.json.into_bytes()),
    }
}

fn test_sql_script_rollback_inner() -> TestResult {
    // Test: Rollback on error - transfer that should fail due to insufficient funds

    // Reset accounts
    let clear_sql = r#"{"sql":"DELETE FROM accounts"}"#;
    let _ = http_request("POST", "/sql/execute", Some(clear_sql.as_bytes()));

    let insert_sql = r#"{"sql":"INSERT INTO accounts (name, balance) VALUES ('Carol', 100.00), ('Dave', 50.00)"}"#;
    let _ = http_request("POST", "/sql/execute", Some(insert_sql.as_bytes()));

    // Try to transfer $500 from Carol (who only has $100) - should FAIL and ROLLBACK
    let script = r#"{
        "script": "var from = sql.query('SELECT id, balance FROM accounts WHERE name = $1', ['Carol']); var to = sql.query('SELECT id FROM accounts WHERE name = $1', ['Dave']); sql.execute('UPDATE accounts SET balance = balance - $1 WHERE id = $2', [500, from[0].id]); if (parseFloat(from[0].balance) < 500) throw new Error('Insufficient funds after update'); sql.execute('UPDATE accounts SET balance = balance + $1 WHERE id = $2', [500, to[0].id]); return {success: true};",
        "input": {}
    }"#;

    let result = http_request("POST", "/sql/script", Some(script.as_bytes()));

    // Script should fail with error
    let script_failed = match &result {
        Ok(r) => r.status >= 400,
        Err(_) => true,
    };

    // Verify Carol's balance is UNCHANGED (rollback worked)
    let carol_check = r#"{"sql":"SELECT balance FROM accounts WHERE name = 'Carol'"}"#;
    let carol = http_request("POST", "/sql/query", Some(carol_check.as_bytes()));
    let carol_balance = carol.map(|r| r.body).unwrap_or_default();

    // Carol should still have 100 (the -500 update was rolled back)
    let rollback_worked = carol_balance.contains("100");

    let success = script_failed && rollback_worked;
    TestResult {
        success,
        json: format!(
            r#"{{"test":"sql_script_rollback","script_failed":{},"carol_balance":"{}","rollback_worked":{},"success":{}}}"#,
            script_failed, carol_balance.replace('"', "\\\""), rollback_worked, success
        ),
    }
}

// =============================================================================
// HTTP Client Helper
// =============================================================================

struct TestResult {
    success: bool,
    json: String,
}

impl TestResult {
    fn error(test: &str, msg: &str) -> Self {
        Self {
            success: false,
            json: format!(r#"{{"test":"{}","error":"{}","success":false}}"#, test, msg),
        }
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

fn http_request(method: &str, path: &str, body: Option<&[u8]>) -> Result<HttpResponse, String> {
    use http_types::{Fields, Method, OutgoingBody, OutgoingRequest, Scheme};

    // Create headers
    let headers = Fields::new();
    headers.append(&"authorization".to_string(), &AUTH_TOKEN.as_bytes().to_vec())
        .map_err(|_| "Failed to set auth header")?;
    if body.is_some() {
        headers.append(&"content-type".to_string(), &b"application/json".to_vec())
            .map_err(|_| "Failed to set content-type")?;
    }

    // Create request
    let request = OutgoingRequest::new(headers);
    let wasi_method = match method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        _ => Method::Get,
    };
    request.set_method(&wasi_method).map_err(|_| "Failed to set method")?;
    request.set_scheme(Some(&Scheme::Http)).map_err(|_| "Failed to set scheme")?;
    request.set_authority(Some(SIDECAR_HOST)).map_err(|_| "Failed to set authority")?;
    request.set_path_with_query(Some(path)).map_err(|_| "Failed to set path")?;

    // Write body
    if let Some(data) = body {
        let outgoing_body = request.body().map_err(|_| "Failed to get body")?;
        let stream = outgoing_body.write().map_err(|_| "Failed to get stream")?;
        stream.blocking_write_and_flush(data).map_err(|_| "Failed to write body")?;
        drop(stream);
        OutgoingBody::finish(outgoing_body, None).map_err(|_| "Failed to finish body")?;
    }

    // Send request
    let future = outgoing_handler::handle(request, None)
        .map_err(|e| format!("Request failed: {:?}", e))?;

    // Wait for response
    let response = loop {
        if let Some(result) = future.get() {
            break result
                .map_err(|_| "Response error")?
                .map_err(|e| format!("HTTP error: {:?}", e))?;
        }
        future.subscribe().block();
    };

    let status = response.status();

    // Read body
    let incoming_body = response.consume().map_err(|_| "Failed to consume body")?;
    let stream = incoming_body.stream().map_err(|_| "Failed to get stream")?;
    let mut bytes = Vec::new();
    loop {
        match stream.blocking_read(65536) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }

    Ok(HttpResponse {
        status,
        body: String::from_utf8(bytes).unwrap_or_default(),
    })
}
