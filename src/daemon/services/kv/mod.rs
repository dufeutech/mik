//! Key-value store service backed by redb.
//!
//! Provides a simple, embedded KV store with TTL support for WASM instances.
//! Stores data at `~/.mik/kv.redb` with ACID guarantees from redb.
//!
//! Features:
//! - Get/set/delete operations
//! - TTL support with automatic expiration
//! - Prefix-based key listing
//! - Existence checks
//! - Automatic cleanup of expired keys
//!
//! # Async Usage
//!
//! All database operations are blocking. When using from async contexts,
//! use the async methods (`get_async`, `set_async`, etc.) which automatically
//! wrap operations in `spawn_blocking` to avoid blocking the async runtime.

mod async_ops;
mod store;
mod types;

#[cfg(test)]
mod tests;

// Re-export the public API
pub use store::KvStore;
