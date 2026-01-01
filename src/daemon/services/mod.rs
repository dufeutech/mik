//! Embedded services for the mik daemon.
//!
//! Provides built-in infrastructure services that WASM handlers can access
//! via HTTP, matching the same API as external sidecars:
//!
//! - **KV**: Key-value store backed by redb
//! - **SQL**: `SQLite` database for relational data
//! - **Storage**: Filesystem-based object storage

pub mod kv;
pub mod sql;
pub mod storage;

use std::path::PathBuf;

/// Get the base directory for mik data (~/.mik).
pub fn get_data_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let data_dir = home.join(".mik");
    std::fs::create_dir_all(&data_dir)?;
    Ok(data_dir)
}
