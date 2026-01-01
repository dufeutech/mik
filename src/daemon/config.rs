//! Global daemon configuration.
//!
//! Loads daemon-wide settings from `~/.mik/daemon.toml`.
//! These settings are separate from per-project `mik.toml` files.
//!
//! # Example Configuration
//!
//! ```toml
//! [daemon]
//! port = 9919
//!
//! [services]
//! kv_enabled = true
//! sql_enabled = true
//! storage_enabled = true
//! ```

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Global daemon configuration loaded from `~/.mik/daemon.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Daemon server settings.
    pub daemon: DaemonSettings,
    /// Embedded service settings.
    pub services: ServiceSettings,
}

/// Daemon server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Port for the daemon HTTP API.
    pub port: u16,
}

/// Embedded service enable/disable settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServiceSettings {
    /// Enable KV service.
    pub kv_enabled: bool,
    /// Enable SQL service.
    pub sql_enabled: bool,
    /// Enable Storage service.
    pub storage_enabled: bool,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self { port: 9919 }
    }
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            kv_enabled: true,
            sql_enabled: true,
            storage_enabled: true,
        }
    }
}

impl DaemonConfig {
    /// Load daemon configuration from `~/.mik/daemon.toml`.
    ///
    /// If the file doesn't exist, returns default configuration.
    /// If the file exists but is invalid, returns an error.
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            tracing::debug!(
                path = %config_path.display(),
                "Daemon config not found, using defaults"
            );
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&config_path).with_context(|| {
            format!(
                "Failed to read daemon config from {}",
                config_path.display()
            )
        })?;

        let config: DaemonConfig = toml::from_str(&content).with_context(|| {
            format!(
                "Failed to parse daemon config from {}",
                config_path.display()
            )
        })?;

        tracing::info!(
            path = %config_path.display(),
            port = config.daemon.port,
            kv = config.services.kv_enabled,
            sql = config.services.sql_enabled,
            storage = config.services.storage_enabled,
            "Loaded daemon configuration"
        );

        Ok(config)
    }

    /// Get the path to the daemon configuration file.
    pub fn config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Failed to get home directory")?;
        Ok(home.join(".mik").join("daemon.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = DaemonConfig::default();
        assert_eq!(config.daemon.port, 9919);
        assert!(config.services.kv_enabled);
        assert!(config.services.sql_enabled);
        assert!(config.services.storage_enabled);
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r"
[daemon]
port = 9090

[services]
kv_enabled = true
sql_enabled = false
storage_enabled = true
";
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.daemon.port, 9090);
        assert!(config.services.kv_enabled);
        assert!(!config.services.sql_enabled);
        assert!(config.services.storage_enabled);
    }

    #[test]
    fn test_parse_partial_config() {
        // Only daemon section
        let toml = r"
[daemon]
port = 8080
";
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.daemon.port, 8080);
        // Services should use defaults
        assert!(config.services.kv_enabled);
        assert!(config.services.sql_enabled);
        assert!(config.services.storage_enabled);
    }

    #[test]
    fn test_parse_empty_config() {
        let toml = "";
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        // All defaults
        assert_eq!(config.daemon.port, 9919);
        assert!(config.services.kv_enabled);
        assert!(config.services.sql_enabled);
        assert!(config.services.storage_enabled);
    }

    #[test]
    fn test_config_path() {
        let path = DaemonConfig::config_path().unwrap();
        assert!(path.ends_with("daemon.toml"));
        assert!(path.to_string_lossy().contains(".mik"));
    }
}
