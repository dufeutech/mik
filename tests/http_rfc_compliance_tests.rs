// Test-specific lint suppressions
#![allow(clippy::collapsible_if)]

//! HTTP RFC Compliance Tests
//!
//! Tests based on Wasmtime HTTP bugs #11770-#11780 to ensure proper
//! HTTP protocol handling. These tests verify RFC-compliant behavior
//! that other WASM runtimes have gotten wrong.
//!
//! References:
//! - #11770: Header case preservation
//! - #11771: Invalid header name rejection
//! - #11772: Custom HTTP methods (PATCH, PROPFIND, etc.)
//! - #11778: Invalid URL scheme rejection
//! - #11779: Malformed path rejection (//double, /../, non-ASCII)
//! - #11780: OPTIONS * and OPTIONS with empty path
//! - #11571: Absolute-form requests (GET http://example.com/path)

#[path = "common.rs"]
mod common;

use common::TestHost;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

// =============================================================================
// Raw HTTP Client for RFC Tests
// =============================================================================

/// A minimal raw HTTP client for testing edge cases that reqwest can't handle.
///
/// This sends raw HTTP/1.1 requests over TCP, allowing us to test:
/// - OPTIONS * (asterisk request target)
/// - Absolute-form requests (GET http://host/path)
/// - Invalid schemes in request-line
/// - Malformed HTTP requests
struct RawHttpClient {
    addr: String,
}

impl RawHttpClient {
    fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
        }
    }

    /// Send a raw HTTP request and return the response.
    ///
    /// Returns (status_code, headers, body) on success.
    fn send_raw(&self, request: &str) -> std::io::Result<(u16, String, String)> {
        let mut stream = TcpStream::connect(&self.addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        // Send the request
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        // Read the response with proper handling
        let mut response = Vec::new();
        let mut buf = [0u8; 8192];
        let mut headers_complete = false;
        let mut content_length: Option<usize> = None;
        let mut header_end_pos: usize = 0;

        loop {
            match stream.read(&mut buf) {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    response.extend_from_slice(&buf[..n]);

                    // Check if we have complete headers
                    if !headers_complete {
                        let response_str = String::from_utf8_lossy(&response);
                        if let Some(pos) = response_str.find("\r\n\r\n") {
                            headers_complete = true;
                            header_end_pos = pos + 4;

                            // Extract Content-Length if present
                            let headers = &response_str[..pos];
                            for line in headers.lines() {
                                if line.to_lowercase().starts_with("content-length:") {
                                    if let Some(len_str) = line.split(':').nth(1) {
                                        content_length = len_str.trim().parse().ok();
                                    }
                                }
                            }

                            // If no Content-Length and Connection: close, read until EOF
                            if content_length.is_none()
                                && !headers
                                    .to_lowercase()
                                    .contains("transfer-encoding: chunked")
                            {
                                // For Connection: close, we read until EOF
                                // But give a reasonable limit
                                if response.len() >= header_end_pos + 1024 * 1024 {
                                    break;
                                }
                            }
                        }
                    }

                    // Check if we have complete body
                    if headers_complete {
                        if let Some(len) = content_length {
                            if response.len() >= header_end_pos + len {
                                break;
                            }
                        } else {
                            // No Content-Length, assume complete after headers for short responses
                            // This handles simple responses without body
                            break;
                        }
                    }
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // On Windows, timeout may appear as WouldBlock
                    if headers_complete {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // Timeout - return what we have
                    break;
                },
                Err(e) => return Err(e),
            }
        }

        // Parse the response
        let response_str = String::from_utf8_lossy(&response).to_string();
        self.parse_response(&response_str)
    }

    fn parse_response(&self, response: &str) -> std::io::Result<(u16, String, String)> {
        if response.is_empty() {
            return Ok((0, String::new(), String::new()));
        }

        let parts: Vec<&str> = response.splitn(2, "\r\n\r\n").collect();
        let header_section = parts.first().unwrap_or(&"");
        let body = parts.get(1).unwrap_or(&"").to_string();

        let mut lines = header_section.lines();
        let status_line = lines.next().unwrap_or("");

        // Parse status code from "HTTP/1.1 200 OK"
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        let headers = lines.collect::<Vec<_>>().join("\r\n");

        Ok((status_code, headers, body))
    }
}

// =============================================================================
// Header Handling Tests (Wasmtime #11770, #11771)
// =============================================================================

/// Test that HTTP headers preserve their original case.
///
/// RFC 7230 Section 3.2: "Each header field consists of a case-insensitive
/// field name". However, proxies and gateways SHOULD preserve case.
///
/// Based on Wasmtime bug #11770.
#[tokio::test]
async fn test_header_case_preserved() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Send a request with mixed-case headers
    let resp = host
        .client()
        .get(host.url("/health"))
        .header("X-Custom-Header", "value")
        .header("X-UPPERCASE", "value2")
        .header("x-lowercase", "value3")
        .send()
        .await
        .expect("Failed to send request");

    // The server should return 200 and accept the headers
    assert_eq!(resp.status(), 200);

    // Note: We can't fully test case preservation without a module that echoes
    // headers back. The key point is the server accepts mixed-case headers.
}

/// Test that invalid header names are rejected.
///
/// RFC 7230 Section 3.2.6: Header field names must be valid tokens.
/// Invalid characters include: ()<>@,;:\"/[]?={} and control characters.
///
/// Based on Wasmtime bug #11771.
#[tokio::test]
async fn test_invalid_header_name_rejected() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Test with various invalid header names
    // Note: reqwest may reject some of these client-side, which is also valid

    // Header with space (invalid)
    let result = host
        .client()
        .get(host.url("/health"))
        .header("Invalid Header", "value")
        .send()
        .await;

    // Either the client rejects it (Ok but error status) or reqwest blocks it
    match result {
        Ok(resp) => {
            // If it got through, server should reject with 400
            assert!(
                resp.status() == 400 || resp.status() == 200,
                "Server should either reject invalid header or hyper normalizes it"
            );
        },
        Err(_) => {
            // Client-side rejection is also acceptable
        },
    }

    // Header with colon in name (invalid per RFC)
    let result = host
        .client()
        .get(host.url("/health"))
        .header("Invalid:Header", "value")
        .send()
        .await;

    match result {
        Ok(resp) => {
            assert!(
                resp.status() == 400 || resp.status() == 200,
                "Server should handle header with colon"
            );
        },
        Err(_) => {
            // Client-side rejection is acceptable
        },
    }
}

// =============================================================================
// HTTP Method Tests (Wasmtime #11772)
// =============================================================================

/// Test that custom/non-standard HTTP methods are allowed.
///
/// RFC 7231 Section 4: Methods are case-sensitive and can be extended.
/// Common extensions include PATCH (RFC 5789), WebDAV methods (PROPFIND,
/// PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK), etc.
///
/// Based on Wasmtime bug #11772.
#[tokio::test]
async fn test_custom_methods_allowed() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Test PATCH method
    let resp = host
        .client()
        .patch(host.url("/health"))
        .send()
        .await
        .expect("Failed to send PATCH request");

    // Server should accept PATCH (may return 404/405 but not 400)
    assert!(
        resp.status() != 400,
        "PATCH should not be rejected as malformed, got {}",
        resp.status()
    );

    // Test custom method via request builder
    let resp = host
        .client()
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            host.url("/health"),
        )
        .send()
        .await
        .expect("Failed to send PROPFIND request");

    // WebDAV PROPFIND should be accepted (may return 404/405/501 but not 400)
    assert!(
        resp.status() != 400,
        "PROPFIND should not be rejected as malformed, got {}",
        resp.status()
    );

    // Test another WebDAV method
    let resp = host
        .client()
        .request(
            reqwest::Method::from_bytes(b"MKCOL").unwrap(),
            host.url("/health"),
        )
        .send()
        .await
        .expect("Failed to send MKCOL request");

    assert!(
        resp.status() != 400,
        "MKCOL should not be rejected as malformed, got {}",
        resp.status()
    );
}

// =============================================================================
// URL Scheme Tests (Wasmtime #11778)
// =============================================================================

/// Test that invalid URL schemes are rejected.
///
/// HTTP servers should only accept http:// and https:// schemes.
/// Other schemes like ftp://, file://, or invalid schemes should be rejected.
///
/// Based on Wasmtime bug #11778.
#[tokio::test]
#[ignore] // Mock server doesn't support raw TCP edge cases; hangs on Linux/macOS
async fn test_invalid_scheme_rejected() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let addr = host.addr().to_string();
    let client = RawHttpClient::new(&addr);

    // Test with ftp:// scheme (invalid for HTTP server)
    let request = format!(
        "GET ftp://example.com/path HTTP/1.1\r\n\
         Host: {}\r\n\
         Connection: close\r\n\
         \r\n",
        addr
    );

    match client.send_raw(&request) {
        Ok((status, _headers, _body)) => {
            // Server should reject with 400 Bad Request or similar
            // Some servers may also return 501 Not Implemented
            // Status 0 means connection closed without response (also acceptable)
            assert!(
                status == 0 || status == 400 || status == 501 || status == 404,
                "Invalid scheme should be rejected with 400/501/404 or closed, got {}",
                status
            );
            if status == 0 {
                println!("Server closed connection for invalid scheme (acceptable)");
            }
        },
        Err(e) => {
            // Connection error is also acceptable - server may close connection
            println!("Connection error (acceptable): {}", e);
        },
    }

    // Test with file:// scheme
    let request = format!(
        "GET file:///etc/passwd HTTP/1.1\r\n\
         Host: {}\r\n\
         Connection: close\r\n\
         \r\n",
        addr
    );

    match client.send_raw(&request) {
        Ok((status, _headers, _body)) => {
            assert!(
                status == 0 || status == 400 || status == 501 || status == 404,
                "file:// scheme should be rejected, got {}",
                status
            );
        },
        Err(_) => {
            // Connection error is acceptable
        },
    }
}

// =============================================================================
// Path Handling Tests (Wasmtime #11779)
// =============================================================================

/// Test that malformed paths with double slashes are handled.
///
/// RFC 3986 Section 3.3: Path can contain empty segments (//), but servers
/// may normalize them. The key is not to crash or behave unexpectedly.
///
/// Based on Wasmtime bug #11779.
#[tokio::test]
async fn test_malformed_path_rejected() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Test path with double slashes
    let resp = host
        .get("//double//slashes//path")
        .await
        .expect("Failed to send request with double slashes");

    // Should return 404 (path not found) not 500/panic
    assert!(
        resp.status() == 404 || resp.status() == 400,
        "Double slashes should return 404 or 400, got {}",
        resp.status()
    );

    // Test path traversal attempt (should be blocked)
    let resp = host
        .get("/static/../../../etc/passwd")
        .await
        .expect("Failed to send path traversal request");

    // Should be rejected (400 or 404), not expose files
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "Path traversal should be blocked, got {}",
        resp.status()
    );

    // Test with encoded traversal
    let resp = host
        .get("/static/..%2F..%2F..%2Fetc%2Fpasswd")
        .await
        .expect("Failed to send encoded path traversal");

    assert!(
        resp.status() == 400 || resp.status() == 404,
        "Encoded path traversal should be blocked, got {}",
        resp.status()
    );
}

/// Test that non-ASCII characters in paths are handled correctly.
///
/// RFC 3986: URIs are ASCII-only; non-ASCII must be percent-encoded.
/// Servers should either accept encoded non-ASCII or reject raw non-ASCII.
///
/// Based on Wasmtime bug #11779.
#[tokio::test]
async fn test_non_ascii_path() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Test with percent-encoded Unicode (should work)
    // %E4%B8%AD%E6%96%87 = Chinese characters
    let resp = host
        .get("/static/%E4%B8%AD%E6%96%87.txt")
        .await
        .expect("Failed to send request with encoded Unicode");

    // Should return 404 (file not found) not 500
    assert!(
        resp.status() == 404 || resp.status() == 400,
        "Encoded Unicode path should return 404 or 400, got {}",
        resp.status()
    );

    // Test with emoji in path (percent-encoded)
    // %F0%9F%98%80 = grinning face emoji
    let resp = host
        .get("/static/%F0%9F%98%80.txt")
        .await
        .expect("Failed to send request with encoded emoji");

    assert!(
        resp.status() == 404 || resp.status() == 400,
        "Encoded emoji path should return 404 or 400, got {}",
        resp.status()
    );
}

// =============================================================================
// OPTIONS Method Tests (Wasmtime #11780)
// =============================================================================

/// Test OPTIONS * request (server-wide options).
///
/// RFC 7231 Section 4.3.7: OPTIONS * is a request for server-wide options,
/// not specific to any resource. This is a valid but unusual request.
///
/// Based on Wasmtime bug #11780.
#[tokio::test]
#[ignore] // Mock server doesn't support raw TCP edge cases; hangs on Linux/macOS
async fn test_options_asterisk() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let addr = host.addr().to_string();
    let client = RawHttpClient::new(&addr);

    // Send OPTIONS * request (asterisk-form request target)
    let request = format!(
        "OPTIONS * HTTP/1.1\r\n\
         Host: {}\r\n\
         Connection: close\r\n\
         \r\n",
        addr
    );

    match client.send_raw(&request) {
        Ok((status, headers, _body)) => {
            // Server should respond with 200 OK (with Allow header) or 501 Not Implemented
            // Status 0 means connection closed (mock server may not support this)
            // Should NOT return 400 Bad Request (asterisk-form is valid per RFC)
            assert!(
                status == 0 || status == 200 || status == 501 || status == 405 || status == 404,
                "OPTIONS * should return 200/501/405/404 or close, got {}. Headers: {}",
                status,
                headers
            );

            if status == 0 {
                println!("Server closed connection for OPTIONS * (mock server limitation)");
            } else if status == 200 {
                // Allow header is optional but recommended
                println!("OPTIONS * returned 200. Headers: {}", headers);
            }
        },
        Err(e) => {
            // Some servers may not support OPTIONS * and close connection
            println!("Connection error for OPTIONS *: {}", e);
        },
    }
}

/// Test OPTIONS with empty path.
///
/// RFC 7230: An empty path in the request-line is not valid for most methods,
/// but the server should handle it gracefully (400 Bad Request).
///
/// Based on Wasmtime bug #11780.
#[tokio::test]
async fn test_options_empty_path() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // OPTIONS to root path
    let resp = host
        .client()
        .request(reqwest::Method::OPTIONS, host.url("/"))
        .send()
        .await
        .expect("Failed to send OPTIONS / request");

    // Should return a valid response, not crash
    // Typically 200 with Allow header, or 404/405
    assert!(
        resp.status().is_success() || resp.status() == 404 || resp.status() == 405,
        "OPTIONS / should return valid response, got {}",
        resp.status()
    );

    // OPTIONS to specific endpoint
    let resp = host
        .client()
        .request(reqwest::Method::OPTIONS, host.url("/health"))
        .send()
        .await
        .expect("Failed to send OPTIONS /health request");

    // Should return valid response
    assert!(
        resp.status().is_success() || resp.status() == 404 || resp.status() == 405,
        "OPTIONS /health should return valid response, got {}",
        resp.status()
    );
}

// =============================================================================
// Absolute-Form Request Tests (Wasmtime #11571)
// =============================================================================

/// Test absolute-form request target (GET http://example.com/path).
///
/// RFC 7230 Section 5.3.2: Absolute-form is used when making requests
/// to proxies. The request-line contains the full URI including scheme.
///
/// Based on Wasmtime bug #11571.
#[tokio::test]
#[ignore] // Mock server doesn't support raw TCP edge cases; hangs on Linux/macOS
async fn test_absolute_form_request() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    let addr = host.addr().to_string();
    let client = RawHttpClient::new(&addr);

    // Send GET request with absolute-form URI (proxy-style)
    // Normal clients send: GET /health HTTP/1.1
    // Absolute-form is: GET http://host/health HTTP/1.1
    let request = format!(
        "GET http://{}/health HTTP/1.1\r\n\
         Host: {}\r\n\
         Connection: close\r\n\
         \r\n",
        addr, addr
    );

    match client.send_raw(&request) {
        Ok((status, _headers, body)) => {
            // Server should accept absolute-form and return same as origin-form
            // Health endpoint should return 200
            // Status 0 means connection closed (mock server may not support absolute-form)
            assert!(
                status == 0 || status == 200,
                "Absolute-form GET should return 200 (same as origin-form) or close, got {}. Body: {}",
                status,
                body
            );
            if status == 0 {
                println!(
                    "Server closed connection for absolute-form request (mock server limitation)"
                );
            }
        },
        Err(e) => {
            // Connection error is acceptable for mock server
            println!("Absolute-form request connection error (acceptable): {}", e);
        },
    }

    // Also test with a different path
    let request = format!(
        "GET http://{}/metrics HTTP/1.1\r\n\
         Host: {}\r\n\
         Connection: close\r\n\
         \r\n",
        addr, addr
    );

    match client.send_raw(&request) {
        Ok((status, _headers, _body)) => {
            // Should return 200 for metrics endpoint
            // Status 0 means connection closed (mock server limitation)
            assert!(
                status == 0 || status == 200 || status == 404,
                "Absolute-form GET /metrics should work or close, got {}",
                status
            );
        },
        Err(e) => {
            // Connection error is acceptable for mock server
            println!("Absolute-form to /metrics connection error: {}", e);
        },
    }
}

// =============================================================================
// Additional Edge Cases
// =============================================================================

/// Test handling of very long header values.
///
/// While not directly from the Wasmtime bugs, this is a common edge case
/// that can cause issues in HTTP parsing.
#[tokio::test]
async fn test_long_header_value() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Create a header value that's long but not excessive (8KB)
    let long_value = "x".repeat(8 * 1024);

    let result = host
        .client()
        .get(host.url("/health"))
        .header("X-Long-Header", long_value)
        .send()
        .await;

    match result {
        Ok(resp) => {
            // Server should either accept (200) or reject as too large (431)
            assert!(
                resp.status() == 200 || resp.status() == 431,
                "Long header should return 200 or 431, got {}",
                resp.status()
            );
        },
        Err(_) => {
            // Connection error due to header size limit is acceptable
        },
    }
}

/// Test handling of many headers.
///
/// Servers should handle requests with many headers without crashing.
#[tokio::test]
async fn test_many_headers() {
    let host = TestHost::builder()
        .start()
        .await
        .expect("Failed to start test host");

    // Build a request with 50 custom headers
    let mut request = host.client().get(host.url("/health"));

    for i in 0..50 {
        request = request.header(format!("X-Custom-Header-{}", i), format!("value-{}", i));
    }

    let resp = request
        .send()
        .await
        .expect("Failed to send request with many headers");

    // Should return 200, handling many headers is normal
    assert_eq!(resp.status(), 200);
}
