//! Daemon infrastructure for managing WASM instances.
//!
//! Provides persistent state storage, process management, and HTTP API
//! for Docker-like instance lifecycle management.

pub mod cron;
pub mod http;
pub mod metrics;
#[cfg(feature = "otlp")]
pub mod otlp;
pub mod process;
pub mod services;
pub mod state;
pub mod watch;
