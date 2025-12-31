//! Default value functions for manifest fields.
//!
//! These functions are used by serde's `default` attribute to provide
//! sensible defaults when fields are not specified in mik.toml.

// =============================================================================
// Tracing Defaults
// =============================================================================

/// Default for tracing enabled (true).
pub const fn default_tracing_enabled() -> bool {
    true
}

/// Default service name for traces ("mik").
pub fn default_service_name() -> String {
    "mik".to_string()
}

// =============================================================================
// Server Defaults
// =============================================================================

/// Default for auto-configuration (true).
pub const fn default_auto() -> bool {
    true
}

/// Default server port (3000).
pub const fn default_port() -> u16 {
    3000
}

/// Default modules directory ("modules/").
pub fn default_modules_dir() -> String {
    "modules/".to_string()
}

/// Default max body size in MB (10).
pub const fn default_max_body_size_mb() -> usize {
    10
}

/// Default execution timeout in seconds (30).
pub const fn default_execution_timeout() -> u64 {
    30
}

/// Default shutdown timeout in seconds (30).
pub const fn default_shutdown_timeout() -> u64 {
    30
}

// =============================================================================
// Composition Defaults
// =============================================================================

/// Default for HTTP handler composition (true).
pub const fn default_http_handler() -> bool {
    true
}

// =============================================================================
// Load Balancer Defaults
// =============================================================================

/// Default for load balancer enabled (false).
pub const fn default_lb_enabled() -> bool {
    false
}

/// Default load balancing algorithm (`round_robin`).
pub fn default_lb_algorithm() -> String {
    "round_robin".to_string()
}

/// Default health check type ("http").
pub fn default_health_check_type() -> String {
    "http".to_string()
}

/// Default health check interval in milliseconds (5000).
pub const fn default_health_check_interval_ms() -> u64 {
    5000
}

/// Default health check timeout in milliseconds (2000).
pub const fn default_health_check_timeout_ms() -> u64 {
    2000
}

/// Default health check path ("/health").
pub fn default_health_check_path() -> String {
    "/health".to_string()
}

/// Default unhealthy threshold (3).
pub const fn default_unhealthy_threshold() -> u32 {
    3
}

/// Default healthy threshold (2).
pub const fn default_healthy_threshold() -> u32 {
    2
}

/// Default request timeout in seconds (30).
pub const fn default_request_timeout_secs() -> u64 {
    30
}

/// Default max connections per backend (100).
pub const fn default_max_connections_per_backend() -> usize {
    100
}

/// Default pool idle timeout in seconds (90).
pub const fn default_pool_idle_timeout_secs() -> u64 {
    90
}

/// Default TCP keepalive in seconds (60).
pub const fn default_tcp_keepalive_secs() -> u64 {
    60
}

/// Default for HTTP/2 only mode (false).
pub const fn default_http2_only() -> bool {
    false
}

// =============================================================================
// Project Defaults
// =============================================================================

/// Default project version ("0.1.0").
pub fn default_version() -> String {
    "0.1.0".to_string()
}
