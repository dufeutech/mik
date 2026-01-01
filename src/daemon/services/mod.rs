// Allow dead code and unused imports for library-first API
// These types are for external consumers, not yet integrated with CLI
#![allow(dead_code, unused_imports)]

//! Embedded services for the mik daemon.
//!
//! Provides built-in infrastructure services that WASM handlers can access
//! via HTTP, matching the same API as external sidecars:
//!
//! - **KV**: Key-value store with pluggable backends (redb, memory, custom)
//! - **SQL**: SQLite database with pluggable backends (SQLite, memory, custom)
//! - **Storage**: Object storage with pluggable backends (filesystem, memory, custom)
//!
//! # Quick Start
//!
//! ```ignore
//! use mik::daemon::services::{Services, ServicesBuilder};
//!
//! // File-based services (production)
//! let services = Services::file("~/.mik")?;
//!
//! // In-memory services (testing)
//! let services = Services::memory()?;
//!
//! // Custom configuration
//! let services = ServicesBuilder::new()
//!     .kv_memory()
//!     .sql_file("./db.sqlite")?
//!     .storage_disabled()
//!     .build()?;
//! ```
//!
//! # Backend Traits
//!
//! Each service defines a backend trait for custom implementations:
//!
//! - `KvBackend` - Custom KV storage (e.g., Redis)
//! - `SqlBackend` - Custom SQL storage (e.g., PostgreSQL)
//! - `StorageBackend` - Custom object storage (e.g., S3)
//!
//! # Example: Custom Backend
//!
//! ```ignore
//! use mik::daemon::services::kv::{KvBackend, KvStore};
//!
//! struct RedisBackend { /* ... */ }
//!
//! #[async_trait]
//! impl KvBackend for RedisBackend {
//!     async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> { /* ... */ }
//!     async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<()> { /* ... */ }
//!     async fn delete(&self, key: &str) -> Result<bool> { /* ... */ }
//!     async fn list(&self, prefix: Option<&str>) -> Result<Vec<String>> { /* ... */ }
//! }
//!
//! let kv = KvStore::custom(RedisBackend::new());
//! ```

mod builder;
pub mod kv;
pub mod sql;
pub mod storage;

use std::path::PathBuf;

// Re-export the builder and services container
pub use builder::{Services, ServicesBuilder};

// Re-export commonly used types for convenience
pub use kv::{KvBackend, KvStore, MemoryBackend as KvMemoryBackend, RedbBackend};
pub use sql::{MemorySqlBackend, Row, SqlBackend, SqlService, SqliteBackend, Value};
pub use storage::{
    FilesystemBackend, MemoryStorageBackend, ObjectMeta, StorageBackend, StorageService,
};

/// Get the base directory for mik data (~/.mik).
pub fn get_data_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let data_dir = home.join(".mik");
    std::fs::create_dir_all(&data_dir)?;
    Ok(data_dir)
}
