//! Async wrappers for `KvStore` operations.
//!
//! These methods wrap the synchronous operations in `spawn_blocking` to
//! avoid blocking the async runtime. Use these when calling from async
//! contexts (HTTP handlers, etc.).

use super::store::KvStore;
use anyhow::{Context, Result};

impl KvStore {
    /// Retrieves a value by key asynchronously.
    ///
    /// Async version of `get` that uses `spawn_blocking`.
    pub async fn get_async(&self, key: String) -> Result<Option<Vec<u8>>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.get(&key))
            .await
            .context("Task join error")?
    }

    /// Stores a key-value pair without expiration asynchronously.
    ///
    /// Async version of `set` that uses `spawn_blocking`.
    pub async fn set_async(&self, key: String, value: Vec<u8>) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.set(&key, &value))
            .await
            .context("Task join error")?
    }

    /// Stores a key-value pair with a TTL asynchronously.
    ///
    /// Async version of `set_with_ttl` that uses `spawn_blocking`.
    pub async fn set_with_ttl_async(
        &self,
        key: String,
        value: Vec<u8>,
        ttl_secs: u64,
    ) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.set_with_ttl(&key, &value, ttl_secs))
            .await
            .context("Task join error")?
    }

    /// Deletes a key-value pair asynchronously.
    ///
    /// Async version of `delete` that uses `spawn_blocking`.
    pub async fn delete_async(&self, key: String) -> Result<bool> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.delete(&key))
            .await
            .context("Task join error")?
    }

    /// Lists all keys matching an optional prefix asynchronously.
    ///
    /// Async version of `list_keys` that uses `spawn_blocking`.
    pub async fn list_keys_async(&self, prefix: Option<String>) -> Result<Vec<String>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list_keys(prefix.as_deref()))
            .await
            .context("Task join error")?
    }
}
