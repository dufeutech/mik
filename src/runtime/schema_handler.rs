//! Static OpenAPI schema serving.
//!
//! This module handles `GET /openapi/<module>` requests by:
//! 1. Looking for a static `.openapi.json` schema file next to the WASM module
//! 2. If file exists: returning schema JSON with appropriate headers
//! 3. If not: returning 404 with error message
//!
//! # Response Headers
//!
//! - `Content-Type: application/json`
//! - `X-Schema-Source: static`

use std::path::{Path, PathBuf};

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::CONTENT_TYPE;
use hyper::{Response, StatusCode};
use tracing::{debug, info};

/// Header indicating schema source.
pub const SCHEMA_SOURCE_HEADER: &str = "X-Schema-Source";

/// Static source indicator value.
pub const SOURCE_STATIC: &str = "static";

/// Get the schema file path for a given module name.
///
/// Schema files are stored as `.openapi.json` files next to WASM modules.
///
/// # Arguments
///
/// * `modules_dir` - Directory containing WASM modules
/// * `module_name` - Name of the module
///
/// # Returns
///
/// The full path to the schema file (e.g., `modules/api.openapi.json`).
#[must_use]
pub fn get_schema_path(modules_dir: &Path, module_name: &str) -> PathBuf {
    modules_dir.join(format!("{module_name}.openapi.json"))
}

/// Serve a static schema file from disk.
///
/// This function reads a `.openapi.json` schema file and returns it as an HTTP response.
/// Uses synchronous file I/O since OS handles file caching efficiently.
///
/// # Arguments
///
/// * `schema_path` - Path to the schema file
///
/// # Returns
///
/// HTTP response with schema JSON (200) or error message (404).
#[must_use]
pub fn serve_static_schema(schema_path: &Path) -> Response<Full<Bytes>> {
    match std::fs::read(schema_path) {
        Ok(content) => {
            info!("Serving static schema from {:?}", schema_path);
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .header(SCHEMA_SOURCE_HEADER, SOURCE_STATIC)
                .body(Full::new(Bytes::from(content)))
                .expect("Failed to build schema response - this is a bug")
        },
        Err(e) => {
            debug!("Schema file not found at {:?}: {}", schema_path, e);
            let error_body = serde_json::json!({
                "error": "Schema not available"
            });
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(CONTENT_TYPE, "application/json")
                .header(SCHEMA_SOURCE_HEADER, SOURCE_STATIC)
                .body(Full::new(Bytes::from(error_body.to_string())))
                .expect("Failed to build error response - this is a bug")
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_get_schema_path() {
        let modules_dir = PathBuf::from("/modules");
        let path = get_schema_path(&modules_dir, "api");
        assert_eq!(path, PathBuf::from("/modules/api.openapi.json"));
    }

    #[test]
    fn test_serve_static_schema_success() {
        let dir = tempdir().unwrap();
        let schema_path = dir.path().join("test.openapi.json");
        let schema_content = r#"{"openapi":"3.0.0","info":{"title":"Test","version":"1.0"}}"#;
        std::fs::write(&schema_path, schema_content).unwrap();

        let response = serve_static_schema(&schema_path);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            response.headers().get(SCHEMA_SOURCE_HEADER).unwrap(),
            SOURCE_STATIC
        );
    }

    #[test]
    fn test_serve_static_schema_not_found() {
        let schema_path = PathBuf::from("/nonexistent/schema.openapi.json");

        let response = serve_static_schema(&schema_path);

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            response.headers().get(SCHEMA_SOURCE_HEADER).unwrap(),
            SOURCE_STATIC
        );
    }
}
