//! Runtime error types for typed error handling.
//!
//! This module provides structured errors for the WASI HTTP runtime,
//! enabling better error handling and more informative error messages.

use std::path::PathBuf;

/// Result type for runtime operations.
#[allow(dead_code)] // Type alias for gradual migration from anyhow
pub type Result<T> = std::result::Result<T, Error>;

/// Runtime errors with structured context.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
#[allow(dead_code)] // Some variants defined for future use
#[allow(clippy::enum_variant_names)] // ScriptError is clearer than Script in error context
pub enum Error {
    /// Module not found in cache or filesystem.
    #[error("module not found: {name}")]
    ModuleNotFound { name: String },

    /// Module failed to load (compile error, invalid WASM).
    #[error("failed to load module '{name}': {reason}")]
    ModuleLoadFailed { name: String, reason: String },

    /// Module execution timed out.
    #[error("module '{name}' timed out after {timeout_secs}s")]
    ExecutionTimeout { name: String, timeout_secs: u64 },

    /// Circuit breaker is open (too many failures).
    #[error("circuit breaker open for module '{name}'")]
    CircuitBreakerOpen { name: String },

    /// Memory limit exceeded during execution.
    #[error("module '{module}' exceeded memory limit of {limit_bytes} bytes")]
    MemoryLimitExceeded { module: String, limit_bytes: usize },

    /// Rate limit exceeded.
    #[error("rate limit exceeded: {reason}")]
    RateLimitExceeded { reason: String },

    /// Path traversal attempt detected.
    #[error("path traversal blocked: {path:?}")]
    PathTraversal { path: PathBuf },

    /// Script execution error.
    #[error("script '{name}' failed: {reason}")]
    ScriptError { name: String, reason: String },

    /// Script not found.
    #[error("script not found: {name}")]
    ScriptNotFound { name: String },

    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// IO error with context.
    #[error("IO error in {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// Wasmtime error wrapper.
    #[error("wasmtime error: {0}")]
    Wasmtime(#[from] wasmtime::Error),

    /// HTTP error.
    #[error("HTTP error: {0}")]
    Http(String),

    /// Invalid request.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

#[allow(dead_code)] // Helper constructors for ergonomic error creation
impl Error {
    /// Create an IO error with context.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Create a module not found error.
    pub fn module_not_found(name: impl Into<String>) -> Self {
        Self::ModuleNotFound { name: name.into() }
    }

    /// Create a module load failed error.
    pub fn module_load_failed(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ModuleLoadFailed {
            name: name.into(),
            reason: reason.into(),
        }
    }

    /// Create a script error.
    pub fn script_error(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ScriptError {
            name: name.into(),
            reason: reason.into(),
        }
    }

    /// Create a circuit breaker open error.
    pub fn circuit_breaker_open(name: impl Into<String>) -> Self {
        Self::CircuitBreakerOpen { name: name.into() }
    }

    /// Create a rate limit exceeded error.
    pub fn rate_limit_exceeded(reason: impl Into<String>) -> Self {
        Self::RateLimitExceeded {
            reason: reason.into(),
        }
    }

    /// Create a path traversal error.
    pub fn path_traversal(path: impl Into<PathBuf>) -> Self {
        Self::PathTraversal { path: path.into() }
    }

    /// Create an execution timeout error.
    pub fn execution_timeout(name: impl Into<String>, timeout_secs: u64) -> Self {
        Self::ExecutionTimeout {
            name: name.into(),
            timeout_secs,
        }
    }

    /// Create a memory limit exceeded error.
    pub fn memory_limit_exceeded(module: impl Into<String>, limit_bytes: usize) -> Self {
        Self::MemoryLimitExceeded {
            module: module.into(),
            limit_bytes,
        }
    }
}

/// Convert runtime error to HTTP status code.
impl Error {
    /// Get the appropriate HTTP status code for this error.
    pub const fn status_code(&self) -> u16 {
        match self {
            Self::ModuleNotFound { .. } | Self::ScriptNotFound { .. } => 404,
            Self::PathTraversal { .. } | Self::InvalidRequest(_) => 400,
            Self::CircuitBreakerOpen { .. } => 503,
            Self::RateLimitExceeded { .. } => 429,
            Self::ExecutionTimeout { .. } => 504,
            Self::MemoryLimitExceeded { .. } => 507,
            Self::ModuleLoadFailed { .. }
            | Self::ScriptError { .. }
            | Self::Config(_)
            | Self::Io { .. }
            | Self::Wasmtime(_)
            | Self::Http(_) => 500,
        }
    }
}

impl Error {
    /// Convert to `anyhow::Error` for gradual migration.
    pub fn into_anyhow(self) -> anyhow::Error {
        anyhow::Error::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_not_found_display() {
        let err = Error::ModuleNotFound {
            name: "api".to_string(),
        };
        assert_eq!(err.to_string(), "module not found: api");
        assert!(err.to_string().contains("api"));
    }

    #[test]
    fn test_module_not_found_helper() {
        let err = Error::module_not_found("my-service");
        assert_eq!(err.to_string(), "module not found: my-service");
    }

    #[test]
    fn test_module_load_failed_display() {
        let err = Error::ModuleLoadFailed {
            name: "broken".to_string(),
            reason: "invalid wasm magic".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "failed to load module 'broken': invalid wasm magic"
        );
        assert!(err.to_string().contains("broken"));
        assert!(err.to_string().contains("invalid wasm magic"));
    }

    #[test]
    fn test_module_load_failed_helper() {
        let err = Error::module_load_failed("test", "compile error");
        assert_eq!(
            err.to_string(),
            "failed to load module 'test': compile error"
        );
    }

    #[test]
    fn test_execution_timeout_display() {
        let err = Error::ExecutionTimeout {
            name: "slow-handler".to_string(),
            timeout_secs: 30,
        };
        assert_eq!(err.to_string(), "module 'slow-handler' timed out after 30s");
        assert!(err.to_string().contains("slow-handler"));
        assert!(err.to_string().contains("30"));
    }

    #[test]
    fn test_execution_timeout_helper() {
        let err = Error::execution_timeout("handler", 60);
        assert_eq!(err.to_string(), "module 'handler' timed out after 60s");
    }

    #[test]
    fn test_memory_limit_exceeded_display() {
        let err = Error::MemoryLimitExceeded {
            module: "greedy".to_string(),
            limit_bytes: 268_435_456, // 256 MB
        };
        assert_eq!(
            err.to_string(),
            "module 'greedy' exceeded memory limit of 268435456 bytes"
        );
        assert!(err.to_string().contains("greedy"));
        assert!(err.to_string().contains("268435456"));
    }

    #[test]
    fn test_memory_limit_exceeded_helper() {
        let err = Error::memory_limit_exceeded("test-module", 1024);
        assert_eq!(
            err.to_string(),
            "module 'test-module' exceeded memory limit of 1024 bytes"
        );
    }

    #[test]
    fn test_circuit_breaker_open_display() {
        let err = Error::CircuitBreakerOpen {
            name: "flaky-service".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "circuit breaker open for module 'flaky-service'"
        );
        assert!(err.to_string().contains("flaky-service"));
    }

    #[test]
    fn test_circuit_breaker_open_helper() {
        let err = Error::circuit_breaker_open("failing");
        assert_eq!(err.to_string(), "circuit breaker open for module 'failing'");
    }

    #[test]
    fn test_rate_limit_exceeded_display() {
        let err = Error::RateLimitExceeded {
            reason: "too many requests".to_string(),
        };
        assert_eq!(err.to_string(), "rate limit exceeded: too many requests");
    }

    #[test]
    fn test_rate_limit_exceeded_helper() {
        let err = Error::rate_limit_exceeded("burst limit");
        assert_eq!(err.to_string(), "rate limit exceeded: burst limit");
    }

    #[test]
    fn test_path_traversal_display() {
        let err = Error::PathTraversal {
            path: PathBuf::from("../../../etc/passwd"),
        };
        assert!(err.to_string().contains("path traversal blocked"));
        assert!(err.to_string().contains("etc/passwd"));
    }

    #[test]
    fn test_path_traversal_helper() {
        let err = Error::path_traversal("/etc/shadow");
        assert!(err.to_string().contains("path traversal blocked"));
    }

    #[test]
    fn test_script_error_display() {
        let err = Error::ScriptError {
            name: "router.js".to_string(),
            reason: "undefined is not a function".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "script 'router.js' failed: undefined is not a function"
        );
    }

    #[test]
    fn test_script_error_helper() {
        let err = Error::script_error("test.js", "syntax error");
        assert_eq!(err.to_string(), "script 'test.js' failed: syntax error");
    }

    #[test]
    fn test_config_error_display() {
        let err = Error::Config("invalid port number".to_string());
        assert_eq!(err.to_string(), "configuration error: invalid port number");
    }

    #[test]
    fn test_http_error_display() {
        let err = Error::Http("connection refused".to_string());
        assert_eq!(err.to_string(), "HTTP error: connection refused");
    }

    #[test]
    fn test_invalid_request_display() {
        let err = Error::InvalidRequest("missing Content-Type".to_string());
        assert_eq!(err.to_string(), "invalid request: missing Content-Type");
    }

    #[test]
    fn test_io_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = Error::Io {
            context: "reading config".to_string(),
            source: io_err,
        };
        assert!(err.to_string().contains("IO error in reading config"));
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn test_io_error_helper() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = Error::io("opening file", io_err);
        assert!(err.to_string().contains("IO error in opening file"));
    }

    // Test HTTP status codes
    #[test]
    fn test_status_code_module_not_found() {
        let err = Error::module_not_found("test");
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn test_status_code_script_not_found() {
        let err = Error::ScriptNotFound {
            name: "test.js".to_string(),
        };
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn test_status_code_path_traversal() {
        let err = Error::path_traversal("../secret");
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn test_status_code_invalid_request() {
        let err = Error::InvalidRequest("bad input".to_string());
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn test_status_code_circuit_breaker_open() {
        let err = Error::circuit_breaker_open("test");
        assert_eq!(err.status_code(), 503);
    }

    #[test]
    fn test_status_code_rate_limit_exceeded() {
        let err = Error::rate_limit_exceeded("quota exhausted");
        assert_eq!(err.status_code(), 429);
    }

    #[test]
    fn test_status_code_execution_timeout() {
        let err = Error::execution_timeout("test", 30);
        assert_eq!(err.status_code(), 504);
    }

    #[test]
    fn test_status_code_memory_limit_exceeded() {
        let err = Error::memory_limit_exceeded("test", 1024);
        assert_eq!(err.status_code(), 507);
    }

    #[test]
    fn test_status_code_module_load_failed() {
        let err = Error::module_load_failed("test", "error");
        assert_eq!(err.status_code(), 500);
    }

    #[test]
    fn test_status_code_script_error() {
        let err = Error::script_error("test.js", "error");
        assert_eq!(err.status_code(), 500);
    }

    #[test]
    fn test_status_code_config_error() {
        let err = Error::Config("bad config".to_string());
        assert_eq!(err.status_code(), 500);
    }

    #[test]
    fn test_status_code_http_error() {
        let err = Error::Http("network error".to_string());
        assert_eq!(err.status_code(), 500);
    }

    // Test error trait implementation
    #[test]
    fn test_error_trait_source() {
        use std::error::Error as StdError;

        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = Error::io("test", io_err);

        // Verify source() returns the underlying IO error
        assert!(err.source().is_some());
    }

    #[test]
    fn test_error_debug() {
        let err = Error::module_not_found("test");
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("ModuleNotFound"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_into_anyhow() {
        let err = Error::module_not_found("test");
        let anyhow_err: anyhow::Error = err.into_anyhow();
        assert!(anyhow_err.to_string().contains("test"));
    }
}
