// Allow dead code for library-first API - these types are for external consumers
#![allow(dead_code)]

//! Request and Response types for library-first API.
//!
//! This module provides framework-agnostic types for handling HTTP requests
//! programmatically, enabling mik to be embedded in applications like Tauri,
//! Electron, or custom HTTP servers.
//!
//! # Examples
//!
//! ```
//! use mik::runtime::{Request, Response};
//!
//! // Create a request programmatically
//! let request = Request {
//!     method: "GET".to_string(),
//!     path: "/run/hello/greet".to_string(),
//!     headers: vec![("Content-Type".to_string(), "application/json".to_string())],
//!     body: vec![],
//! };
//!
//! // Responses can be constructed directly
//! let response = Response {
//!     status: 200,
//!     headers: vec![("Content-Type".to_string(), "application/json".to_string())],
//!     body: b"{\"message\": \"Hello, World!\"}".to_vec(),
//! };
//! ```

use http_body_util::Full;
use hyper::body::Bytes;

/// A framework-agnostic HTTP request.
///
/// This type allows mik to be used without binding to a specific HTTP framework.
/// It can be constructed from any HTTP request format (axum, actix, hyper, etc.)
/// or created directly for programmatic use.
///
/// # Fields
///
/// * `method` - HTTP method (GET, POST, PUT, DELETE, etc.)
/// * `path` - Request path including query string (e.g., `/run/hello/greet?name=world`)
/// * `headers` - HTTP headers as name-value pairs
/// * `body` - Request body as raw bytes
///
/// # Example
///
/// ```
/// use mik::runtime::Request;
///
/// let request = Request::new("POST", "/run/api/users")
///     .with_header("Content-Type", "application/json")
///     .with_body(b"{\"name\": \"Alice\"}".to_vec());
/// ```
#[derive(Debug, Clone)]
pub struct Request {
    /// HTTP method (e.g., "GET", "POST", "PUT", "DELETE").
    pub method: String,
    /// Request path including query string.
    pub path: String,
    /// HTTP headers as (name, value) pairs.
    pub headers: Vec<(String, String)>,
    /// Request body as raw bytes.
    pub body: Vec<u8>,
}

impl Request {
    /// Create a new request with the given method and path.
    ///
    /// Headers and body are initialized to empty.
    #[must_use]
    pub fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Add a header to the request.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Add multiple headers to the request.
    #[must_use]
    pub fn with_headers(
        mut self,
        headers: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        for (name, value) in headers {
            self.headers.push((name.into(), value.into()));
        }
        self
    }

    /// Set the request body.
    #[must_use]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Set the request body from a string.
    #[must_use]
    pub fn with_body_str(mut self, body: impl Into<String>) -> Self {
        self.body = body.into().into_bytes();
        self
    }

    /// Get a header value by name (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| n.to_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
    }

    /// Get the Content-Type header value.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    /// Check if the request accepts gzip encoding.
    #[must_use]
    pub fn accepts_gzip(&self) -> bool {
        self.header("accept-encoding")
            .is_some_and(|v| v.contains("gzip"))
    }

    /// Get the Content-Length header value.
    #[must_use]
    pub fn content_length(&self) -> Option<usize> {
        self.header("content-length").and_then(|v| v.parse().ok())
    }
}

impl Default for Request {
    fn default() -> Self {
        Self::new("GET", "/")
    }
}

/// A framework-agnostic HTTP response.
///
/// This type allows mik to return responses without being tied to a specific
/// HTTP framework. It can be converted to any HTTP response format.
///
/// # Fields
///
/// * `status` - HTTP status code (e.g., 200, 404, 500)
/// * `headers` - HTTP headers as name-value pairs
/// * `body` - Response body as raw bytes
///
/// # Example
///
/// ```
/// use mik::runtime::Response;
///
/// let response = Response::ok()
///     .with_header("Content-Type", "application/json")
///     .with_body(b"{\"success\": true}".to_vec());
/// ```
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// HTTP headers as (name, value) pairs.
    pub headers: Vec<(String, String)>,
    /// Response body as raw bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// Create a new response with the given status code.
    ///
    /// Headers and body are initialized to empty.
    #[must_use]
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Create a 200 OK response.
    #[must_use]
    pub fn ok() -> Self {
        Self::new(200)
    }

    /// Create a 201 Created response.
    #[must_use]
    pub fn created() -> Self {
        Self::new(201)
    }

    /// Create a 204 No Content response.
    #[must_use]
    pub fn no_content() -> Self {
        Self::new(204)
    }

    /// Create a 400 Bad Request response with an error message.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400)
            .with_header("Content-Type", "application/json")
            .with_body_str(format!(r#"{{"error": "{}"}}"#, message.into()))
    }

    /// Create a 404 Not Found response with an error message.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404)
            .with_header("Content-Type", "application/json")
            .with_body_str(format!(r#"{{"error": "{}"}}"#, message.into()))
    }

    /// Create a 500 Internal Server Error response with an error message.
    #[must_use]
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new(500)
            .with_header("Content-Type", "application/json")
            .with_body_str(format!(r#"{{"error": "{}"}}"#, message.into()))
    }

    /// Create a 502 Bad Gateway response with an error message.
    #[must_use]
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(502)
            .with_header("Content-Type", "application/json")
            .with_body_str(format!(r#"{{"error": "{}"}}"#, message.into()))
    }

    /// Create a 503 Service Unavailable response.
    #[must_use]
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(503)
            .with_header("Content-Type", "application/json")
            .with_body_str(format!(r#"{{"error": "{}"}}"#, message.into()))
    }

    /// Add a header to the response.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Add multiple headers to the response.
    #[must_use]
    pub fn with_headers(
        mut self,
        headers: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        for (name, value) in headers {
            self.headers.push((name.into(), value.into()));
        }
        self
    }

    /// Set the response body.
    #[must_use]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Set the response body from a string.
    #[must_use]
    pub fn with_body_str(mut self, body: impl Into<String>) -> Self {
        self.body = body.into().into_bytes();
        self
    }

    /// Set JSON body with Content-Type header.
    #[must_use]
    pub fn with_json(self, body: impl Into<String>) -> Self {
        self.with_header("Content-Type", "application/json")
            .with_body_str(body)
    }

    /// Get a header value by name (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| n.to_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
    }

    /// Check if the response is successful (2xx status).
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Check if the response is a redirect (3xx status).
    #[must_use]
    pub const fn is_redirect(&self) -> bool {
        self.status >= 300 && self.status < 400
    }

    /// Check if the response is a client error (4xx status).
    #[must_use]
    pub const fn is_client_error(&self) -> bool {
        self.status >= 400 && self.status < 500
    }

    /// Check if the response is a server error (5xx status).
    #[must_use]
    pub const fn is_server_error(&self) -> bool {
        self.status >= 500 && self.status < 600
    }

    /// Get the body as a UTF-8 string.
    pub fn body_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.body)
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::ok()
    }
}

// =============================================================================
// Conversions to/from hyper types
// =============================================================================

impl TryFrom<Response> for hyper::Response<Full<Bytes>> {
    type Error = hyper::http::Error;

    fn try_from(response: Response) -> Result<Self, Self::Error> {
        let mut builder = hyper::Response::builder().status(response.status);

        for (name, value) in response.headers {
            builder = builder.header(name, value);
        }

        builder.body(Full::new(Bytes::from(response.body)))
    }
}

impl From<hyper::Response<Full<Bytes>>> for Response {
    fn from(response: hyper::Response<Full<Bytes>>) -> Self {
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.to_string(), v.to_string()))
            })
            .collect();

        // Get body bytes from the Full<Bytes> body
        // Note: This requires the body to be collected first in async context.
        // For sync conversion, we use an empty body as a fallback.
        // In practice, responses are converted after the body is already collected.
        let body = Vec::new();

        Self {
            status,
            headers,
            body,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_builder() {
        let request = Request::new("POST", "/api/users")
            .with_header("Content-Type", "application/json")
            .with_header("Authorization", "Bearer token123")
            .with_body_str(r#"{"name": "Alice"}"#);

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/users");
        assert_eq!(request.headers.len(), 2);
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(request.header("authorization"), Some("Bearer token123"));
        assert_eq!(request.body, br#"{"name": "Alice"}"#);
    }

    #[test]
    fn test_request_header_case_insensitive() {
        let request = Request::new("GET", "/").with_header("Content-Type", "text/plain");

        assert_eq!(request.header("content-type"), Some("text/plain"));
        assert_eq!(request.header("Content-Type"), Some("text/plain"));
        assert_eq!(request.header("CONTENT-TYPE"), Some("text/plain"));
    }

    #[test]
    fn test_request_accepts_gzip() {
        let request = Request::new("GET", "/").with_header("Accept-Encoding", "gzip, deflate");
        assert!(request.accepts_gzip());

        let request_no_gzip = Request::new("GET", "/").with_header("Accept-Encoding", "deflate");
        assert!(!request_no_gzip.accepts_gzip());

        let request_no_header = Request::new("GET", "/");
        assert!(!request_no_header.accepts_gzip());
    }

    #[test]
    fn test_response_builder() {
        let response = Response::ok()
            .with_header("Content-Type", "application/json")
            .with_body_str(r#"{"success": true}"#);

        assert_eq!(response.status, 200);
        assert!(response.is_success());
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.body, br#"{"success": true}"#);
    }

    #[test]
    fn test_response_status_helpers() {
        let ok = Response::ok();
        assert!(ok.is_success());
        assert!(!ok.is_redirect());
        assert!(!ok.is_client_error());
        assert!(!ok.is_server_error());

        let redirect = Response::new(301);
        assert!(!redirect.is_success());
        assert!(redirect.is_redirect());

        let not_found = Response::not_found("resource not found");
        assert!(not_found.is_client_error());
        assert_eq!(not_found.status, 404);

        let error = Response::internal_error("something went wrong");
        assert!(error.is_server_error());
        assert_eq!(error.status, 500);
    }

    #[test]
    fn test_response_json() {
        let response = Response::ok().with_json(r#"{"key": "value"}"#);

        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.body_str().unwrap(), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_response_to_hyper() {
        let response = Response::ok()
            .with_header("X-Custom", "value")
            .with_body_str("Hello");

        let hyper_response: hyper::Response<Full<Bytes>> = response.try_into().unwrap();

        assert_eq!(hyper_response.status().as_u16(), 200);
        assert_eq!(
            hyper_response
                .headers()
                .get("X-Custom")
                .unwrap()
                .to_str()
                .unwrap(),
            "value"
        );
    }

    #[test]
    fn test_request_default() {
        let request = Request::default();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/");
        assert!(request.headers.is_empty());
        assert!(request.body.is_empty());
    }

    #[test]
    fn test_response_default() {
        let response = Response::default();
        assert_eq!(response.status, 200);
        assert!(response.headers.is_empty());
        assert!(response.body.is_empty());
    }

    #[test]
    fn test_request_with_headers() {
        let headers = vec![
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
        ];
        let request = Request::new("POST", "/api").with_headers(headers);

        assert_eq!(request.headers.len(), 2);
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(request.header("accept"), Some("application/json"));
    }

    #[test]
    fn test_response_body_str() {
        let response = Response::ok().with_body_str("Hello, World!");
        assert_eq!(response.body_str().unwrap(), "Hello, World!");

        // Invalid UTF-8
        let response_invalid = Response::ok().with_body(vec![0xff, 0xfe]);
        assert!(response_invalid.body_str().is_err());
    }
}
