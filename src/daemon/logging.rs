//! Structured logging configuration for the mik daemon.
//!
//! Provides JSON-formatted logging with request correlation, compatible with
//! log aggregation systems like Loki, Elasticsearch, or `CloudWatch`.

// Allow unused - logging infrastructure for future daemon mode
#![allow(dead_code)]
#![allow(clippy::must_use_candidate)]

use std::io;
use tracing::Level;
use tracing_subscriber::{
    filter::EnvFilter,
    fmt::{self, format::FmtSpan},
    prelude::*,
};

/// Logging format options.
#[derive(Debug, Clone, Copy, Default)]
pub enum LogFormat {
    /// Pretty human-readable output (default for development)
    #[default]
    Pretty,
    /// JSON output for log aggregation
    Json,
    /// Compact single-line output
    Compact,
}

/// Logging configuration.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Output format (pretty, json, compact)
    pub format: LogFormat,
    /// Minimum log level
    pub level: Level,
    /// Include span events (enter/exit)
    pub with_spans: bool,
    /// Include target (module path)
    pub with_target: bool,
    /// Include file name and line number
    pub with_file: bool,
    /// Include thread IDs
    pub with_thread_ids: bool,
    /// Include timestamps
    pub with_timestamp: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Pretty,
            level: Level::INFO,
            with_spans: false,
            with_target: true,
            with_file: false,
            with_thread_ids: false,
            with_timestamp: true,
        }
    }
}

impl LogConfig {
    /// Create config for JSON logging (production).
    pub const fn json() -> Self {
        Self {
            format: LogFormat::Json,
            level: Level::INFO,
            with_spans: true,
            with_target: true,
            with_file: false,
            with_thread_ids: false,
            with_timestamp: true,
        }
    }

    /// Create config for development (pretty output).
    pub const fn development() -> Self {
        Self {
            format: LogFormat::Pretty,
            level: Level::DEBUG,
            with_spans: false,
            with_target: false,
            with_file: true,
            with_thread_ids: false,
            with_timestamp: true,
        }
    }

    /// Set the log level.
    #[must_use]
    pub const fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Set the log format.
    #[must_use]
    pub const fn format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }
}

/// Initialize the global tracing subscriber.
///
/// Should be called once at startup. Respects `RUST_LOG` environment
/// variable for filtering if set.
///
/// # Example
///
/// ```rust,ignore
/// use mik::daemon::logging::{init_logging, LogConfig, LogFormat};
///
/// // Development mode
/// init_logging(LogConfig::development());
///
/// // Production JSON mode
/// init_logging(LogConfig::json());
///
/// // Custom config
/// init_logging(LogConfig::default().format(LogFormat::Json));
/// ```
pub fn init_logging(config: &LogConfig) {
    // Build filter from RUST_LOG env or default level
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.level.to_string()));

    let span_events = if config.with_spans {
        FmtSpan::NEW | FmtSpan::CLOSE
    } else {
        FmtSpan::NONE
    };

    match config.format {
        LogFormat::Pretty => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                fmt::layer()
                    .with_ansi(true)
                    .with_target(config.with_target)
                    .with_file(config.with_file)
                    .with_line_number(config.with_file)
                    .with_thread_ids(config.with_thread_ids)
                    .with_span_events(span_events),
            );
            let _ = tracing::subscriber::set_global_default(subscriber);
        },
        LogFormat::Json => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                fmt::layer()
                    .json()
                    .with_target(config.with_target)
                    .with_file(config.with_file)
                    .with_line_number(config.with_file)
                    .with_thread_ids(config.with_thread_ids)
                    .with_span_events(span_events)
                    .with_writer(io::stdout),
            );
            let _ = tracing::subscriber::set_global_default(subscriber);
        },
        LogFormat::Compact => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                fmt::layer()
                    .compact()
                    .with_ansi(true)
                    .with_target(config.with_target)
                    .with_file(config.with_file)
                    .with_line_number(config.with_file)
                    .with_thread_ids(config.with_thread_ids)
                    .with_span_events(span_events),
            );
            let _ = tracing::subscriber::set_global_default(subscriber);
        },
    }
}

/// Generate a unique request ID.
///
/// Format: `req_<random_chars>` (URL-safe base64)
pub fn generate_request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}

/// Span extension for adding request context.
///
/// Use with `tracing::info_span!` to create correlated request logs.
///
/// # Example
///
/// ```rust,ignore
/// let span = tracing::info_span!(
///     "request",
///     request_id = %request_id,
///     method = %method,
///     path = %path,
/// );
/// let _guard = span.enter();
///
/// tracing::info!("Processing request");
/// ```
#[macro_export]
macro_rules! request_span {
    ($request_id:expr, $method:expr, $path:expr) => {
        tracing::info_span!(
            "request",
            request_id = %$request_id,
            method = %$method,
            path = %$path,
        )
    };
}

/// Log a completed request with timing.
///
/// # Example
///
/// ```rust,ignore
/// log_request_complete("req_abc123", "GET", "/api/health", 200, 42);
/// ```
pub fn log_request_complete(
    request_id: &str,
    method: &str,
    path: &str,
    status: u16,
    duration_ms: u64,
) {
    tracing::info!(
        request_id = %request_id,
        method = %method,
        path = %path,
        status = status,
        duration_ms = duration_ms,
        "Request completed"
    );
}

/// Log an outbound HTTP call (for debugging WASM module calls).
pub fn log_outbound_request(
    request_id: &str,
    module: &str,
    method: &str,
    host: &str,
    path: &str,
    status: Option<u16>,
    duration_ms: u64,
) {
    tracing::debug!(
        request_id = %request_id,
        module = %module,
        outbound.method = %method,
        outbound.host = %host,
        outbound.path = %path,
        outbound.status = ?status,
        outbound.duration_ms = duration_ms,
        "Outbound HTTP call"
    );
}

/// Log a WASM module execution.
pub fn log_module_execution(request_id: &str, module: &str, duration_ms: u64, success: bool) {
    if success {
        tracing::info!(
            request_id = %request_id,
            module = %module,
            duration_ms = duration_ms,
            "Module executed"
        );
    } else {
        tracing::warn!(
            request_id = %request_id,
            module = %module,
            duration_ms = duration_ms,
            "Module execution failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_request_id() {
        let id1 = generate_request_id();
        let id2 = generate_request_id();

        assert!(id1.starts_with("req_"));
        assert!(id2.starts_with("req_"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_log_config_default() {
        let config = LogConfig::default();
        assert!(matches!(config.format, LogFormat::Pretty));
        assert_eq!(config.level, Level::INFO);
    }

    #[test]
    fn test_log_config_json() {
        let config = LogConfig::json();
        assert!(matches!(config.format, LogFormat::Json));
    }

    #[test]
    fn test_log_config_builder() {
        let config = LogConfig::default()
            .level(Level::DEBUG)
            .format(LogFormat::Compact);

        assert_eq!(config.level, Level::DEBUG);
        assert!(matches!(config.format, LogFormat::Compact));
    }
}
