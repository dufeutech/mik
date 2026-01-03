// =============================================================================
// Lint Configuration
// =============================================================================

// Safety: deny unsafe by default, allow only where documented
// (wasmtime AOT cache in runtime/mod.rs, Unix setsid in daemon/process.rs)
#![deny(unsafe_code)]
// Correctness: Must handle all fallible operations
#![deny(unused_must_use)]
// Quality: Pedantic but pragmatic
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![warn(rust_2018_idioms)]
#![allow(missing_debug_implementations)] // Types contain wasmtime::Engine, MokaCache which lack Debug

// Allowed with documented reasons
#![allow(clippy::missing_errors_doc)] // Error returns self-documenting via type
#![allow(clippy::missing_panics_doc)] // Panics documented in main entry points
#![allow(clippy::module_name_repetitions)] // e.g., runtime::RuntimeConfig is clearer
#![allow(clippy::doc_markdown)] // Too many false positives in code docs
#![allow(clippy::must_use_candidate)] // Not all returned values need annotation
#![allow(clippy::cast_possible_truncation)] // Intentional in HTTP size calculations
#![allow(clippy::cast_sign_loss)] // Intentional in size calculations
#![allow(clippy::trivially_copy_pass_by_ref)] // Consistency in API signatures
#![allow(clippy::too_many_lines)] // Complex HTTP handlers require more lines
#![allow(clippy::redundant_pub_crate)] // Explicit pub(crate) documents intent, aids refactoring

//! Library crate for mik - exposes modules for testing and integration.
//!
//! This module exposes internal functionality for:
//! - Fuzz testing security-critical functions
//! - Integration testing with the real runtime
//!
//! # Security Functions
//!
//! The following security functions are exposed for fuzzing:
//!
//! - [`security::sanitize_file_path`] - Path traversal prevention
//! - [`security::sanitize_module_name`] - Module name validation
//! - [`security::validate_windows_path`] - Windows-specific path validation
//! - [`security::validate_path_within_base`] - Symlink escape prevention
//!
//! # Runtime
//!
//! The [`runtime`] module exposes the WASI HTTP runtime for integration testing:
//!
//! - [`runtime::Runtime`] - Core WASM runtime without network binding (library-first API)
//! - [`runtime::Server`] - HTTP server wrapper for Runtime
//! - [`runtime::RuntimeBuilder`] - Builder for creating a Runtime instance
//! - [`runtime::Request`] / [`runtime::Response`] - Framework-agnostic HTTP types
//! - [`runtime::Cluster`] / [`runtime::ClusterBuilder`] - Multi-worker orchestration
//!
//! # Example
//!
//! ```
//! use mik::security::{sanitize_file_path, sanitize_module_name};
//!
//! // Valid paths are normalized
//! assert!(sanitize_file_path("assets/style.css").is_ok());
//!
//! // Path traversal is blocked
//! assert!(sanitize_file_path("../etc/passwd").is_err());
//!
//! // Valid module names pass
//! assert!(sanitize_module_name("my-module").is_ok());
//!
//! // Path separators in module names are blocked
//! assert!(sanitize_module_name("../evil").is_err());
//! ```

/// WASI HTTP runtime for serving WASM components.
///
/// This module provides the core runtime for loading and executing WASM
/// components with the WASI HTTP interface.
///
/// # Examples
///
/// ```no_run
/// use mik::runtime::{Runtime, Request, Server};
///
/// # async fn example() -> anyhow::Result<()> {
/// // Create a runtime (no network binding)
/// let runtime = Runtime::builder()
///     .modules_dir("modules/")
///     .build()?;
///
/// // Option 1: Handle requests programmatically
/// let response = runtime.handle_request(Request::new("GET", "/run/hello/")).await?;
///
/// // Option 2: Use with the built-in server
/// let server = Server::new(runtime, "127.0.0.1:3000".parse()?);
/// server.serve().await?;
/// # Ok(())
/// # }
/// ```
#[path = "runtime/mod.rs"]
pub mod runtime;

/// Security utilities for input sanitization and path traversal prevention.
///
/// This module provides functions to validate and sanitize untrusted input
/// to prevent security vulnerabilities like path traversal attacks.
///
/// # Examples
///
/// ```
/// use mik::security::{sanitize_file_path, sanitize_module_name};
///
/// // Validate user-provided paths
/// let path = sanitize_file_path("images/logo.png").unwrap();
///
/// // Validate module names from URL parameters
/// let module = sanitize_module_name("my-handler").unwrap();
/// ```
pub use runtime::security;

/// Reliability primitives (circuit breaker, rate limiting).
///
/// This module provides patterns for building resilient services:
/// - Circuit breaker for failing fast on unhealthy dependencies
/// - Rate limiting to prevent resource exhaustion
#[path = "reliability/mod.rs"]
pub mod reliability;

/// Centralized constants for security limits and defaults.
///
/// All magic numbers in the runtime should be defined here with
/// documented rationale. Includes limits for:
/// - Request body sizes
/// - Timeout durations
/// - Cache sizes
/// - Concurrency limits
pub mod constants;

/// Manifest types for mik.toml configuration.
///
/// This module provides types for parsing and representing
/// the mik.toml manifest file, including load balancer configuration.
pub mod manifest;

/// Daemon infrastructure and embedded services.
///
/// This module provides infrastructure for the mik daemon including:
/// - Embedded services (KV, SQL, Storage)
/// - Process management
/// - State persistence
///
/// # Services
///
/// The daemon provides embedded services that WASM handlers can access:
///
/// - [`daemon::services::kv`] - Key-value store backed by redb
/// - [`daemon::services::sql`] - SQLite database for relational data
/// - [`daemon::services::storage`] - Filesystem-based object storage
#[path = "daemon/mod.rs"]
pub mod daemon;

/// Shared utility functions for formatting and common operations.
///
/// Provides helper functions used across multiple modules:
/// - [`utils::format_bytes`] - Human-readable byte sizes
/// - [`utils::format_duration`] - Human-readable durations
/// - [`utils::get_cargo_name`] - Extract project name from Cargo.toml
pub mod utils;

/// UI utilities for consistent terminal output formatting.
///
/// Provides shared formatting functions for error messages and status output:
/// - [`ui::print_error_box`] - Print formatted error boxes
/// - [`ui::print_error_box_from_output`] - Print errors from command output
/// - [`ui::print_error_box_with_hints`] - Print errors with troubleshooting hints
pub mod ui;

/// Schema caching infrastructure.
///
/// Provides content-addressed caching for JSON schemas using BLAKE3 hashes.
/// Schemas are cached in `~/.cache/mik/schemas/` and managed via `mik cache` commands.
///
/// - [`cache::SchemaCache`] - BLAKE3 content-addressed file cache
/// - [`cache::SchemaStats`] - Cache statistics (entry count, total bytes)
/// - [`cache::CacheConfig`] - Configuration for cache behavior
pub mod cache;
