//! Core `KvStore` implementation with synchronous operations.
//!
//! Provides the main key-value store interface backed by redb.

use super::types::KvEntry;
use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::Path;
use std::sync::Arc;

/// Table name for key-value pairs with expiration metadata
pub(super) const KV_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("kv");

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
    pub(super) db: Arc<Database>,
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
        let data_dir = crate::daemon::services::get_data_dir()?;
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
    pub(super) fn save_entry(&self, key: &str, entry: &KvEntry) -> Result<()> {
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
}
