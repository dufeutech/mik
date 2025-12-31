//! Type definitions for the KV store.
//!
//! Contains the internal entry structure used for storing values with
//! optional expiration metadata.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Internal structure for storing values with optional expiration.
///
/// Serialized to JSON for compatibility with debugging tools and future
/// schema evolution. The expiration timestamp is in Unix epoch seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct KvEntry {
    /// The actual value bytes (could be any binary data)
    pub value: Vec<u8>,
    /// Optional expiration timestamp (Unix epoch seconds). None = never expires.
    pub expires_at: Option<u64>,
}

impl KvEntry {
    /// Creates a new entry without expiration
    pub const fn new(value: Vec<u8>) -> Self {
        Self {
            value,
            expires_at: None,
        }
    }

    /// Creates a new entry with TTL (time-to-live) in seconds
    pub fn with_ttl(value: Vec<u8>, ttl_secs: u64) -> Result<Self> {
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
    pub fn is_expired(&self) -> Result<bool> {
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
