//! Host configuration types and validation.
//!
//! This module contains the configuration structures for the WASI HTTP runtime host.

use crate::constants;
use std::path::PathBuf;
use tracing::warn;

/// Maximum execution timeout (5 minutes).
pub const MAX_EXECUTION_TIMEOUT_SECS: u64 = 300;

/// Default memory limit per request (128MB).
pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;

/// Minimum memory limit per request (1MB).
pub const MIN_MEMORY_LIMIT_BYTES: usize = 1024 * 1024;

/// Maximum memory limit per request (4GB).
pub const MAX_MEMORY_LIMIT_BYTES: usize = 4 * 1024 * 1024 * 1024;

/// Default graceful shutdown drain timeout in seconds.
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 30;

/// Configuration validation errors.
///
/// These errors indicate invalid configuration values that would prevent
/// the host from operating correctly or safely.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("invalid execution_timeout_secs={value}: {reason}")]
    Timeout { value: u64, reason: &'static str },
    #[error("invalid memory_limit_bytes={value}: {reason}")]
    MemoryLimit { value: usize, reason: &'static str },
    #[error("invalid concurrency configuration: {reason}")]
    Concurrency { reason: &'static str },
}

/// Configuration for the host.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Directory containing .wasm modules, or single component path.
    pub modules_path: PathBuf,
    /// Maximum number of modules to keep loaded (LRU eviction).
    pub cache_size: usize,
    /// Maximum cache memory in bytes (byte-aware eviction).
    pub max_cache_bytes: usize,
    /// Static files directory (optional).
    pub static_dir: Option<PathBuf>,
    /// Port to bind to (from mik.toml).
    pub port: u16,
    /// Timeout for WASM execution (in seconds).
    pub execution_timeout_secs: u64,
    /// Memory limit per request (in bytes).
    pub memory_limit_bytes: usize,
    /// Maximum concurrent requests.
    pub max_concurrent_requests: usize,
    /// Maximum request body size (in bytes).
    pub max_body_size_bytes: usize,
    /// Maximum concurrent requests per module.
    pub max_per_module_requests: usize,
    /// Graceful shutdown drain timeout in seconds.
    pub shutdown_timeout_secs: u64,
    /// Enable wasi:logging for WASM modules.
    pub logging_enabled: bool,
    /// Allowed hosts for outgoing HTTP requests.
    pub http_allowed: Vec<String>,
    /// Scripts directory (optional, for JS orchestration).
    pub scripts_dir: Option<PathBuf>,
    /// Hot-reload mode: bypass persistent AOT cache, always recompile.
    pub hot_reload: bool,
    /// Maximum AOT cache size in MB (0 = default 1GB).
    pub aot_cache_max_mb: usize,
    /// Fuel budget per request (None = use `DEFAULT_FUEL_BUDGET`).
    /// Fuel provides deterministic CPU limiting complementing epoch-based preemption.
    pub fuel_budget: Option<u64>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            modules_path: PathBuf::from(constants::DEFAULT_MODULES_DIR),
            cache_size: constants::DEFAULT_CACHE_SIZE,
            max_cache_bytes: constants::DEFAULT_CACHE_MB * 1024 * 1024,
            static_dir: None,
            port: constants::DEFAULT_PORT,
            execution_timeout_secs: constants::MAX_WASM_TIMEOUT_SECS,
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            max_concurrent_requests: constants::DEFAULT_MAX_CONCURRENT_REQUESTS,
            max_body_size_bytes: constants::MAX_BODY_SIZE_BYTES,
            max_per_module_requests: constants::DEFAULT_MAX_PER_MODULE_REQUESTS,
            shutdown_timeout_secs: DEFAULT_SHUTDOWN_TIMEOUT_SECS,
            logging_enabled: false,
            http_allowed: Vec::new(),
            scripts_dir: None,
            hot_reload: false,
            aot_cache_max_mb: 0,
            fuel_budget: None,
        }
    }
}

impl HostConfig {
    /// Validate configuration values.
    ///
    /// Checks that all configuration values are within acceptable bounds:
    /// - `execution_timeout_secs` must be > 0 and <= 300
    /// - `memory_limit_bytes` must be >= 1MB and <= 4GB
    /// - `max_concurrent_requests` must be > 0
    /// - `max_per_module_requests` must not exceed `max_concurrent_requests`
    ///
    /// Issues a warning (but does not fail) if `modules_path` does not exist.
    ///
    /// # Errors
    ///
    /// Returns a `ConfigError` if any configuration value is out of bounds.
    ///
    /// # Examples
    ///
    /// ```
    /// use mik::runtime::HostConfig;
    ///
    /// let config = HostConfig::default();
    /// assert!(config.validate().is_ok());
    ///
    /// let mut invalid_config = HostConfig::default();
    /// invalid_config.execution_timeout_secs = 0;
    /// assert!(invalid_config.validate().is_err());
    /// ```
    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        // Validate execution timeout (must be > 0 and <= 300 seconds)
        if self.execution_timeout_secs == 0 {
            return Err(ConfigError::Timeout {
                value: 0,
                reason: "must be greater than 0",
            });
        }
        if self.execution_timeout_secs > MAX_EXECUTION_TIMEOUT_SECS {
            return Err(ConfigError::Timeout {
                value: self.execution_timeout_secs,
                reason: "must be <= 300 seconds (5 minutes)",
            });
        }

        // Validate memory limit (must be >= 1MB and <= 4GB)
        if self.memory_limit_bytes < MIN_MEMORY_LIMIT_BYTES {
            return Err(ConfigError::MemoryLimit {
                value: self.memory_limit_bytes,
                reason: "must be >= 1MB (1048576 bytes)",
            });
        }
        if self.memory_limit_bytes > MAX_MEMORY_LIMIT_BYTES {
            return Err(ConfigError::MemoryLimit {
                value: self.memory_limit_bytes,
                reason: "must be <= 4GB (4294967296 bytes)",
            });
        }

        // Validate max_concurrent_requests (must be > 0)
        if self.max_concurrent_requests == 0 {
            return Err(ConfigError::Concurrency {
                reason: "max_concurrent_requests must be greater than 0",
            });
        }

        // Validate max_per_module_requests (must not exceed max_concurrent_requests)
        if self.max_per_module_requests > self.max_concurrent_requests {
            return Err(ConfigError::Concurrency {
                reason: "max_per_module_requests cannot exceed max_concurrent_requests",
            });
        }

        // Warn if modules_path doesn't exist (non-fatal)
        if !self.modules_path.exists() {
            warn!(
                "modules_path does not exist: {}",
                self.modules_path.display()
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid() {
        let config = HostConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_zero_timeout_is_invalid() {
        let config = HostConfig {
            execution_timeout_secs: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::Timeout { value: 0, .. }));
        assert!(err.to_string().contains("must be greater than 0"));
    }

    #[test]
    fn test_excessive_timeout_is_invalid() {
        let config = HostConfig {
            execution_timeout_secs: 301,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::Timeout { value: 301, .. }
        ));
    }

    #[test]
    fn test_memory_limit_too_small() {
        let config = HostConfig {
            memory_limit_bytes: 100,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::MemoryLimit { .. }
        ));
    }

    #[test]
    fn test_memory_limit_too_large() {
        let config = HostConfig {
            memory_limit_bytes: 5 * 1024 * 1024 * 1024,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_concurrent_requests_is_invalid() {
        let config = HostConfig {
            max_concurrent_requests: 0,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConfigError::Concurrency { .. }
        ));
    }

    #[test]
    fn test_per_module_exceeds_total_concurrent() {
        let config = HostConfig {
            max_concurrent_requests: 10,
            max_per_module_requests: 20,
            ..Default::default()
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_per_module_equal_to_total_is_valid() {
        let config = HostConfig {
            max_concurrent_requests: 10,
            max_per_module_requests: 10,
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_valid_memory_limit_at_boundaries() {
        let min_config = HostConfig {
            memory_limit_bytes: MIN_MEMORY_LIMIT_BYTES,
            ..Default::default()
        };
        assert!(min_config.validate().is_ok());

        let max_config = HostConfig {
            memory_limit_bytes: MAX_MEMORY_LIMIT_BYTES,
            ..Default::default()
        };
        assert!(max_config.validate().is_ok());
    }

    #[test]
    fn test_valid_timeout_at_boundary() {
        let config = HostConfig {
            execution_timeout_secs: MAX_EXECUTION_TIMEOUT_SECS,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_error_display() {
        let timeout_err = ConfigError::Timeout {
            value: 0,
            reason: "must be greater than 0",
        };
        assert!(timeout_err.to_string().contains("execution_timeout_secs"));

        let memory_err = ConfigError::MemoryLimit {
            value: 100,
            reason: "too small",
        };
        assert!(memory_err.to_string().contains("memory_limit_bytes"));

        let concurrency_err = ConfigError::Concurrency {
            reason: "invalid config",
        };
        assert!(concurrency_err.to_string().contains("concurrency"));
    }

    #[test]
    fn test_config_error_is_error_trait() {
        let err: Box<dyn std::error::Error> = Box::new(ConfigError::Timeout {
            value: 0,
            reason: "test",
        });
        assert!(err.to_string().contains("test"));
    }
}
