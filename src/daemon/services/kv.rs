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

// Allow unused - KV service for future sidecar integration
#![allow(dead_code)]

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Table name for key-value pairs with expiration metadata
const KV_TABLE: TableDefinition<'static, &'static str, &'static [u8]> = TableDefinition::new("kv");

/// Internal structure for storing values with optional expiration.
///
/// Serialized to JSON for compatibility with debugging tools and future
/// schema evolution. The expiration timestamp is in Unix epoch seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KvEntry {
    /// The actual value bytes (could be any binary data)
    value: Vec<u8>,
    /// Optional expiration timestamp (Unix epoch seconds). None = never expires.
    expires_at: Option<u64>,
}

impl KvEntry {
    /// Creates a new entry without expiration
    fn new(value: Vec<u8>) -> Self {
        Self {
            value,
            expires_at: None,
        }
    }

    /// Creates a new entry with TTL (time-to-live) in seconds
    fn with_ttl(value: Vec<u8>, ttl_secs: u64) -> Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs();

        Ok(Self {
            value,
            expires_at: Some(now + ttl_secs),
        })
    }

    /// Checks if this entry has expired
    fn is_expired(&self) -> Result<bool> {
        match self.expires_at {
            None => Ok(false), // Never expires
            Some(expires_at) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("System time before UNIX epoch")?
                    .as_secs();
                Ok(now >= expires_at)
            },
        }
    }
}

/// Key-value store interface wrapping redb database.
///
/// Provides simple KV operations with optional TTL support and automatic
/// expiration cleanup. All operations serialize to JSON for human-readable
/// debugging in redb.
///
/// # Thread Safety
///
/// `KvStore` is `Clone` and can be shared across threads. The underlying
/// database handles concurrent access safely.
#[derive(Clone)]
pub struct KvStore {
    db: Arc<Database>,
}

impl KvStore {
    /// Opens the default KV store at `~/.mik/kv.redb`.
    ///
    /// This is a convenience method for the standard location.
    /// Creates the directory and database if they don't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The data directory cannot be determined or created
    /// - The database file cannot be opened or created
    /// - The KV table cannot be initialized
    pub fn open_default() -> Result<Self> {
        let data_dir = super::get_data_dir()?;
        let kv_path = data_dir.join("kv.redb");
        Self::open(kv_path)
    }

    /// Opens or creates the KV database at the given path.
    ///
    /// Creates parent directories if needed. Uses redb's ACID guarantees
    /// to prevent corruption on crashes or unclean shutdowns.
    /// Initializes the KV table on first open.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Parent directory cannot be created
    /// - Database file cannot be opened or created (permissions, disk full, etc.)
    /// - Initialization transaction fails to begin or commit
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists before opening database
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create KV directory: {}", parent.display()))?;
        }

        let db = Database::create(path)
            .with_context(|| format!("Failed to open KV database: {}", path.display()))?;

        // Initialize table on first open to ensure it exists for reads
        let write_txn = db
            .begin_write()
            .context("Failed to begin initialization transaction")?;
        {
            let _table = write_txn
                .open_table(KV_TABLE)
                .context("Failed to initialize KV table")?;
        }
        write_txn
            .commit()
            .context("Failed to commit initialization transaction")?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Retrieves a value by key.
    ///
    /// Returns None if the key doesn't exist or has expired. Automatically
    /// removes expired entries on access.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Database read transaction cannot begin
    /// - Key lookup fails due to database corruption
    /// - Entry deserialization fails (corrupted data)
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(KV_TABLE)
            .context("Failed to open KV table")?;

        let result = table
            .get(key)
            .with_context(|| format!("Failed to read key '{key}'"))?;

        match result {
            Some(guard) => {
                let json = guard.value();
                let entry: KvEntry = serde_json::from_slice(json)
                    .with_context(|| format!("Failed to deserialize entry for key '{key}'"))?;

                // Check expiration
                if entry.is_expired()? {
                    // Drop read transaction before starting write
                    drop(table);
                    drop(read_txn);

                    // Remove expired entry
                    self.delete(key)?;
                    Ok(None)
                } else {
                    Ok(Some(entry.value))
                }
            },
            None => Ok(None),
        }
    }

    /// Stores a key-value pair without expiration.
    ///
    /// Overwrites existing value if key already exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the write transaction cannot begin, the entry
    /// cannot be serialized, or the transaction fails to commit.
    pub fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        let entry = KvEntry::new(value.to_vec());
        self.save_entry(key, &entry)
    }

    /// Stores a key-value pair with a TTL (time-to-live) in seconds.
    ///
    /// The entry will be automatically removed when accessed after expiration.
    /// Overwrites existing value if key already exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the system time is before UNIX epoch, the write
    /// transaction cannot begin, or the transaction fails to commit.
    pub fn set_with_ttl(&self, key: &str, value: &[u8], ttl_secs: u64) -> Result<()> {
        let entry = KvEntry::with_ttl(value.to_vec(), ttl_secs)?;
        self.save_entry(key, &entry)
    }

    /// Internal helper to save an entry to the database.
    fn save_entry(&self, key: &str, entry: &KvEntry) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(KV_TABLE)
                .context("Failed to open KV table")?;

            let json = serde_json::to_vec(entry).context("Failed to serialize entry to JSON")?;

            table
                .insert(key, json.as_slice())
                .with_context(|| format!("Failed to insert key '{key}'"))?;
        }

        write_txn
            .commit()
            .context("Failed to commit set transaction")?;

        Ok(())
    }

    /// Deletes a key-value pair.
    ///
    /// Returns Ok(true) if the key existed and was removed, Ok(false) if
    /// it didn't exist. Idempotent - safe to call multiple times.
    ///
    /// # Errors
    ///
    /// Returns an error if the write transaction cannot begin or commit.
    pub fn delete(&self, key: &str) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        let removed = {
            let mut table = write_txn
                .open_table(KV_TABLE)
                .context("Failed to open KV table")?;

            table
                .remove(key)
                .with_context(|| format!("Failed to remove key '{key}'"))?
                .is_some()
        };

        write_txn
            .commit()
            .context("Failed to commit delete transaction")?;

        Ok(removed)
    }

    /// Checks if a key exists and has not expired.
    ///
    /// Returns true if the key exists and is valid, false otherwise.
    /// Automatically removes expired entries.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `get` operation fails.
    pub fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// Lists all keys matching an optional prefix.
    ///
    /// Returns empty vec if no keys match. Automatically skips expired entries
    /// and removes them. Pass None for prefix to list all keys.
    ///
    /// # Errors
    ///
    /// Returns an error if the read transaction cannot begin or iteration fails.
    pub fn list_keys(&self, prefix: Option<&str>) -> Result<Vec<String>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(KV_TABLE)
            .context("Failed to open KV table")?;

        let mut keys = Vec::new();
        let mut expired_keys = Vec::new();

        for item in table.iter().context("Failed to iterate KV table")? {
            let (key, value) = item.context("Failed to read KV entry")?;
            let key_str = key.value();

            // Filter by prefix if provided
            if let Some(prefix) = prefix
                && !key_str.starts_with(prefix)
            {
                continue;
            }

            // Check expiration
            if let Ok(entry) = serde_json::from_slice::<KvEntry>(value.value()) {
                match entry.is_expired() {
                    Ok(true) => {
                        // Mark for deletion
                        expired_keys.push(key_str.to_string());
                    },
                    Ok(false) => {
                        // Valid entry
                        keys.push(key_str.to_string());
                    },
                    Err(_) => {
                        // Skip entries with time errors
                    },
                }
            }
        }

        // Clean up expired keys (drop read transaction first)
        drop(table);
        drop(read_txn);

        for key in expired_keys {
            let _ = self.delete(&key); // Ignore errors during cleanup
        }

        Ok(keys)
    }

    /// Clears all key-value pairs from the store.
    ///
    /// This is primarily useful for testing. Use with caution in production.
    #[cfg(test)]
    pub fn clear(&self) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(KV_TABLE)
                .context("Failed to open KV table")?;

            // Collect all keys first to avoid iterator invalidation
            let keys: Vec<String> = table
                .iter()
                .context("Failed to iterate KV table")?
                .filter_map(std::result::Result::ok)
                .map(|(key, _)| key.value().to_string())
                .collect();

            // Remove all keys
            for key in keys {
                table
                    .remove(key.as_str())
                    .context("Failed to remove key during clear")?;
            }
        }

        write_txn
            .commit()
            .context("Failed to commit clear transaction")?;

        Ok(())
    }

    // ========================================================================
    // Async Methods
    //
    // These methods wrap the synchronous operations in `spawn_blocking` to
    // avoid blocking the async runtime. Use these when calling from async
    // contexts (HTTP handlers, etc.).
    // ========================================================================

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_set_and_get() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("key1", b"value1").unwrap();
        let value = store.get("key1").unwrap().unwrap();
        assert_eq!(value, b"value1");
    }

    #[test]
    fn test_get_nonexistent_key() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        let result = store.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_delete() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("key1", b"value1").unwrap();
        let deleted = store.delete("key1").unwrap();
        assert!(deleted);

        let result = store.get("key1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_delete_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        let deleted = store.delete("nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_exists() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        assert!(!store.exists("key1").unwrap());

        store.set("key1", b"value1").unwrap();
        assert!(store.exists("key1").unwrap());

        store.delete("key1").unwrap();
        assert!(!store.exists("key1").unwrap());
    }

    #[test]
    fn test_list_keys_all() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("key1", b"value1").unwrap();
        store.set("key2", b"value2").unwrap();
        store.set("other", b"value3").unwrap();

        let keys = store.list_keys(None).unwrap();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&"key1".to_string()));
        assert!(keys.contains(&"key2".to_string()));
        assert!(keys.contains(&"other".to_string()));
    }

    #[test]
    fn test_list_keys_with_prefix() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("user:1", b"alice").unwrap();
        store.set("user:2", b"bob").unwrap();
        store.set("session:abc", b"xyz").unwrap();

        let user_keys = store.list_keys(Some("user:")).unwrap();
        assert_eq!(user_keys.len(), 2);
        assert!(user_keys.contains(&"user:1".to_string()));
        assert!(user_keys.contains(&"user:2".to_string()));

        let session_keys = store.list_keys(Some("session:")).unwrap();
        assert_eq!(session_keys.len(), 1);
        assert!(session_keys.contains(&"session:abc".to_string()));
    }

    #[test]
    fn test_overwrite_value() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("key1", b"value1").unwrap();
        store.set("key1", b"value2").unwrap();

        let value = store.get("key1").unwrap().unwrap();
        assert_eq!(value, b"value2");
    }

    #[test]
    fn test_binary_data() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        let binary_data = vec![0u8, 1, 2, 3, 255, 128, 64];
        store.set("binary", &binary_data).unwrap();

        let value = store.get("binary").unwrap().unwrap();
        assert_eq!(value, binary_data);
    }

    #[test]
    fn test_ttl_expiration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        // Set with 1 second TTL
        store.set_with_ttl("temp", b"expires soon", 1).unwrap();

        // Should exist immediately
        assert!(store.exists("temp").unwrap());
        let value = store.get("temp").unwrap().unwrap();
        assert_eq!(value, b"expires soon");

        // Wait for expiration
        thread::sleep(Duration::from_secs(2));

        // Should be expired and automatically removed
        assert!(!store.exists("temp").unwrap());
        assert!(store.get("temp").unwrap().is_none());
    }

    #[test]
    fn test_ttl_no_expiration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        // Set with long TTL
        store.set_with_ttl("key", b"value", 3600).unwrap();

        // Should still exist
        assert!(store.exists("key").unwrap());
        let value = store.get("key").unwrap().unwrap();
        assert_eq!(value, b"value");
    }

    #[test]
    fn test_list_keys_filters_expired() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        // Add permanent key
        store.set("permanent", b"forever").unwrap();

        // Add temporary key with 1 second TTL
        store.set_with_ttl("temporary", b"short-lived", 1).unwrap();

        // Both should appear initially
        let keys = store.list_keys(None).unwrap();
        assert_eq!(keys.len(), 2);

        // Wait for expiration
        thread::sleep(Duration::from_secs(2));

        // Only permanent key should remain
        let keys = store.list_keys(None).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&"permanent".to_string()));
    }

    #[test]
    fn test_persistence_across_reopens() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");

        {
            let store = KvStore::open(&db_path).unwrap();
            store.set("persistent", b"value").unwrap();
        }

        // Reopen database and verify data persists
        {
            let store = KvStore::open(&db_path).unwrap();
            let value = store.get("persistent").unwrap().unwrap();
            assert_eq!(value, b"value");
        }
    }

    #[test]
    fn test_clear() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("key1", b"value1").unwrap();
        store.set("key2", b"value2").unwrap();
        store.set("key3", b"value3").unwrap();

        assert_eq!(store.list_keys(None).unwrap().len(), 3);

        store.clear().unwrap();

        assert_eq!(store.list_keys(None).unwrap().len(), 0);
        assert!(!store.exists("key1").unwrap());
    }

    #[test]
    fn test_update_ttl() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        // Set with short TTL
        store.set_with_ttl("key", b"value1", 1).unwrap();

        // Overwrite with longer TTL before expiration
        store.set_with_ttl("key", b"value2", 3600).unwrap();

        // Wait for original TTL to pass
        thread::sleep(Duration::from_secs(2));

        // Should still exist with new value
        assert!(store.exists("key").unwrap());
        let value = store.get("key").unwrap().unwrap();
        assert_eq!(value, b"value2");
    }

    #[test]
    fn test_empty_value() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = KvStore::open(&db_path).unwrap();

        store.set("empty", b"").unwrap();
        let value = store.get("empty").unwrap().unwrap();
        assert_eq!(value, b"");
    }
}
