//! mik.toml manifest format.
//!
//! Similar to Cargo.toml, pyproject.toml, package.json but for WASI components.

mod defaults;
mod types;
mod validation;

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

// Re-export all public types
// Note: Some re-exports may appear unused but are part of the public API
#[allow(unused_imports)]
pub use defaults::*;
pub use types::*;
#[allow(unused_imports)]
pub use validation::ValidationError;

// =============================================================================
// Manifest Implementation (Load/Save/Modify)
// =============================================================================

impl Manifest {
    /// Load manifest from mik.toml in current directory.
    pub fn load() -> Result<Self> {
        Self::load_from(Path::new("mik.toml"))
    }

    /// Load manifest from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let manifest: Self = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        // Validate the loaded manifest
        manifest.validate(path)?;

        Ok(manifest)
    }

    /// Save manifest to mik.toml in current directory.
    pub fn save(&self) -> Result<()> {
        self.save_to(Path::new("mik.toml"))
    }

    /// Save manifest to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize manifest")?;
        fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
    }

    /// Check if mik.toml exists in current directory.
    #[allow(dead_code)]
    pub fn exists() -> bool {
        Path::new("mik.toml").exists()
    }

    /// Load only the server port from mik.toml without full validation.
    ///
    /// Useful for obtaining the port before full manifest parsing is needed.
    /// Returns `None` if mik.toml doesn't exist or can't be parsed.
    pub fn load_port() -> Option<u16> {
        Self::load_server_config().map(|c| c.port).ok()
    }

    /// Load only the `[server]` section from mik.toml without full validation.
    ///
    /// Returns default `ServerConfig` if mik.toml doesn't exist or section is missing.
    pub fn load_server_config() -> Result<ServerConfig> {
        Self::load_server_config_from(Path::new("mik.toml"))
    }

    /// Load only the `[server]` section from a specific manifest path.
    pub fn load_server_config_from(path: &Path) -> Result<ServerConfig> {
        #[derive(Deserialize)]
        struct Partial {
            #[serde(default)]
            server: ServerConfig,
        }

        if !path.exists() {
            return Ok(ServerConfig::default());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let partial: Partial = toml::from_str(&content)
            .with_context(|| format!("Failed to parse server config from {}", path.display()))?;
        Ok(partial.server)
    }

    /// Load only the `[tracing]` section from mik.toml without full validation.
    ///
    /// Returns default `TracingConfig` if mik.toml doesn't exist or section is missing.
    pub fn load_tracing_config() -> Result<TracingConfig> {
        Self::load_tracing_config_from(Path::new("mik.toml"))
    }

    /// Load only the `[tracing]` section from a specific manifest path.
    pub fn load_tracing_config_from(path: &Path) -> Result<TracingConfig> {
        #[derive(Deserialize)]
        struct Partial {
            #[serde(default)]
            tracing: TracingConfig,
        }

        if !path.exists() {
            return Ok(TracingConfig::default());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let partial: Partial = toml::from_str(&content)
            .with_context(|| format!("Failed to parse tracing config from {}", path.display()))?;
        Ok(partial.tracing)
    }

    /// Add a dependency.
    #[allow(dead_code)]
    pub fn add_dependency(&mut self, name: &str, dep: Dependency) {
        self.dependencies.insert(name.to_string(), dep);
    }

    /// Remove a dependency.
    #[allow(dead_code)]
    pub fn remove_dependency(&mut self, name: &str) -> Option<Dependency> {
        self.dependencies.remove(name)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_parse_manifest() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"
description = "My WASI app"

[dependencies]
serde = "1.0"
mikrozen = { version = "0.1" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        assert_eq!(manifest.project.name, "my-app");
        assert_eq!(manifest.dependencies.len(), 2);
    }

    #[test]
    fn test_parse_server_config() {
        let toml = r#"
[project]
name = "my-server-app"

[server]
port = 8080
modules = "wasm/"
static = "public/"
cache_size = 20
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        assert_eq!(manifest.server.port, 8080);
        assert_eq!(manifest.server.modules, "wasm/");
        assert_eq!(manifest.server.r#static, Some("public/".to_string()));
        assert_eq!(manifest.server.cache_size, 20);
    }

    #[test]
    fn test_parse_composition_config() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[composition]
http_handler = true
bridge = "path/to/bridge.wasm"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        assert!(manifest.composition.http_handler);
        assert_eq!(
            manifest.composition.bridge,
            Some("path/to/bridge.wasm".to_string())
        );
    }

    #[test]
    fn test_composition_config_defaults() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        // Default: http_handler = true
        assert!(manifest.composition.http_handler);
        // Default: no paths set
        assert!(manifest.composition.bridge.is_none());
    }

    #[test]
    fn test_composition_disabled() {
        let toml = r#"
[project]
name = "my-lib"
version = "0.1.0"

[composition]
http_handler = false
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        assert!(!manifest.composition.http_handler);
    }

    #[test]
    fn test_valid_project_names() {
        assert!(Manifest::is_valid_name("my-component"));
        assert!(Manifest::is_valid_name("my_component"));
        assert!(Manifest::is_valid_name("MyComponent"));
        assert!(Manifest::is_valid_name("_private"));
        assert!(Manifest::is_valid_name("a"));
        assert!(Manifest::is_valid_name("component123"));
    }

    #[test]
    fn test_invalid_project_names() {
        assert!(!Manifest::is_valid_name(""));
        assert!(!Manifest::is_valid_name("123component")); // starts with number
        assert!(!Manifest::is_valid_name("-component")); // starts with hyphen
        assert!(!Manifest::is_valid_name("my component")); // contains space
        assert!(!Manifest::is_valid_name("my@component")); // invalid character
        assert!(!Manifest::is_valid_name("a".repeat(65).as_str())); // too long

        // Security: path traversal attacks
        assert!(!Manifest::is_valid_name("../../../etc/passwd"));
        assert!(!Manifest::is_valid_name(".."));
        assert!(!Manifest::is_valid_name("foo/bar"));
        assert!(!Manifest::is_valid_name("foo\\bar"));
        assert!(!Manifest::is_valid_name("foo/../bar"));
    }

    #[test]
    fn test_valid_versions() {
        assert!(Manifest::is_valid_version("0.1.0"));
        assert!(Manifest::is_valid_version("1.2.3"));
        assert!(Manifest::is_valid_version("10.20.30"));
        assert!(Manifest::is_valid_version("1.0.0-alpha"));
        assert!(Manifest::is_valid_version("1.0.0+build.123"));
        assert!(Manifest::is_valid_version("2.3.4-beta.1+exp.sha.5114f85"));
    }

    #[test]
    fn test_invalid_versions() {
        assert!(!Manifest::is_valid_version(""));
        assert!(!Manifest::is_valid_version("1"));
        assert!(!Manifest::is_valid_version("1.0"));
        assert!(!Manifest::is_valid_version("v1.0.0"));
        assert!(!Manifest::is_valid_version("1.0.0.0"));
        assert!(!Manifest::is_valid_version("abc"));
    }

    #[test]
    fn test_validate_empty_name() {
        let toml = r#"
[project]
name = ""
version = "0.1.0"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Project name is required"));
    }

    #[test]
    fn test_validate_invalid_name() {
        let toml = r#"
[project]
name = "123-invalid"
version = "0.1.0"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Invalid project name"));
    }

    #[test]
    fn test_validate_invalid_version() {
        let toml = r#"
[project]
name = "my-app"
version = "1.0"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Invalid version"));
        assert!(error.contains("semantic versioning"));
    }

    #[test]
    fn test_validate_invalid_port() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[server]
port = 0
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("port cannot be 0"));
    }

    #[test]
    fn test_validate_port_out_of_range() {
        // Note: u16 max is 65535, so TOML parser will reject values > 65535
        // This test verifies that TOML parsing fails for out-of-range ports
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[server]
port = 70000
"#;
        let result: Result<Manifest, _> = toml::from_str(toml);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("invalid value") || error.contains("out of range"));
    }

    #[test]
    fn test_validate_zero_cache_size_valid() {
        // cache_size = 0 means auto-detect, which is valid
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[server]
cache_size = 0
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_ok());
        assert_eq!(manifest.server.cache_size, 0);
    }

    #[test]
    fn test_validate_empty_modules_dir() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[server]
modules = ""
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("modules directory cannot be empty"));
    }

    #[test]
    fn test_validate_nonexistent_path_dependency() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("mik.toml");

        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
missing = { path = "../does-not-exist.wasm" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(&manifest_path);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("non-existent path"));
        assert!(error.contains("missing"));
    }

    #[test]
    fn test_validate_valid_path_dependency() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("mik.toml");

        // Create a dummy wasm file
        let wasm_path = temp_dir.path().join("component.wasm");
        std::fs::File::create(&wasm_path).unwrap();

        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
local = { path = "component.wasm" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(&manifest_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_path_extension() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("mik.toml");

        // Create a file with wrong extension
        let file_path = temp_dir.path().join("component.txt");
        std::fs::File::create(&file_path).unwrap();

        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
local = { path = "component.txt" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(&manifest_path);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("should be a .wasm file"));
    }

    #[test]
    fn test_validate_empty_dependency_version() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
empty = ""
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("empty version string"));
    }

    #[test]
    fn test_validate_dependency_multiple_sources() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
conflicting = { version = "1.0", git = "https://github.com/user/repo.git" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("multiple sources"));
    }

    #[test]
    fn test_validate_dependency_no_source() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
nosource = { }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("must specify at least one source"));
    }

    #[test]
    fn test_validate_invalid_git_url() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
badgit = { git = "not-a-url" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("invalid git URL"));
    }

    #[test]
    fn test_validate_valid_git_urls() {
        let urls = vec![
            "https://github.com/user/repo.git",
            "http://example.com/repo.git",
            "git://github.com/user/repo.git",
            "ssh://git@github.com/user/repo.git",
            "git@github.com:user/repo.git",
        ];

        for url in urls {
            let toml = format!(
                r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
gitdep = {{ git = "{url}" }}
"#
            );
            let manifest: Manifest = toml::from_str(&toml).unwrap();
            let result = manifest.validate(Path::new("mik.toml"));
            assert!(result.is_ok(), "Failed for URL: {url}");
        }
    }

    #[test]
    fn test_validate_conflicting_git_refs() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
gitdep = { git = "https://github.com/user/repo.git", branch = "main", tag = "v1.0" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("multiple git refs"));
    }

    #[test]
    fn test_validate_registry_without_version() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
registry = { registry = "ghcr.io/user/component" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("must specify a version"));
    }

    #[test]
    fn test_validate_empty_registry() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[dependencies]
registry = { registry = "", version = "1.0" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("empty registry URL"));
    }

    #[test]
    fn test_validate_valid_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("mik.toml");

        // Create a dummy wasm file for path dependency
        let wasm_path = temp_dir.path().join("component.wasm");
        std::fs::File::create(&wasm_path).unwrap();

        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[server]
port = 3000
modules = "modules/"
cache_size = 10

[dependencies]
serde = "1.0"
local = { path = "component.wasm" }
gitdep = { git = "https://github.com/user/repo.git", branch = "main" }
registry = { registry = "ghcr.io/user/component", version = "1.0" }
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(&manifest_path);
        if let Err(e) = &result {
            println!("Validation error: {e}");
        }
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_multiple_errors() {
        let toml = r#"
[project]
name = "123-invalid"
version = "1.0"

[server]
port = 0
cache_size = 0
modules = ""
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let result = manifest.validate(Path::new("mik.toml"));
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();

        // Should contain multiple errors
        assert!(error.contains("1."));
        assert!(error.contains("2."));
        assert!(error.contains("3."));
        assert!(error.contains("Invalid project name"));
        assert!(error.contains("Invalid version"));
        assert!(error.contains("port cannot be 0"));
    }

    #[test]
    fn test_load_validates_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("mik.toml");

        // Write an invalid manifest
        let mut file = std::fs::File::create(&manifest_path).unwrap();
        write!(
            file,
            r#"
[project]
name = ""
version = "invalid"
"#
        )
        .unwrap();

        // Loading should fail validation
        let result = Manifest::load_from(&manifest_path);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Validation failed"));
    }

    // NOTE: Tests for is_http_host_allowed are in reliability/src/security.rs
    // which is the single source of truth for this function.
    // ServerConfig::is_host_allowed delegates to that implementation.

    #[test]
    fn test_server_config_is_host_allowed_delegates() {
        // Just verify the delegation works - full tests are in reliability crate
        let config = ServerConfig {
            http_allowed: vec!["api.example.com".to_string()],
            ..Default::default()
        };
        assert!(config.is_host_allowed("api.example.com"));
        assert!(!config.is_host_allowed("other.example.com"));
    }

    #[test]
    fn test_lb_config_defaults() {
        let config = LbConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.health_check_type, "http");
        assert_eq!(config.health_check_interval_ms, 5000);
        assert_eq!(config.health_check_timeout_ms, 2000);
        assert_eq!(config.health_check_path, "/health");
        assert_eq!(config.unhealthy_threshold, 3);
        assert_eq!(config.healthy_threshold, 2);
        assert_eq!(config.request_timeout_secs, 30);
        assert_eq!(config.max_connections_per_backend, 100);
        assert_eq!(config.pool_idle_timeout_secs, 90);
        assert_eq!(config.tcp_keepalive_secs, 60);
        assert!(!config.http2_only);
    }

    #[test]
    fn test_parse_lb_config() {
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[lb]
enabled = true
health_check_interval_ms = 10000
health_check_timeout_ms = 3000
health_check_path = "/healthz"
unhealthy_threshold = 5
healthy_threshold = 3
request_timeout_secs = 60
max_connections_per_backend = 200
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let lb = manifest.lb.expect("lb config should be present");
        assert!(lb.enabled);
        assert_eq!(lb.health_check_interval_ms, 10000);
        assert_eq!(lb.health_check_timeout_ms, 3000);
        assert_eq!(lb.health_check_path, "/healthz");
        assert_eq!(lb.unhealthy_threshold, 5);
        assert_eq!(lb.healthy_threshold, 3);
        assert_eq!(lb.request_timeout_secs, 60);
        assert_eq!(lb.max_connections_per_backend, 200);
    }

    #[test]
    fn test_parse_lb_config_partial() {
        // Test that defaults are applied for missing fields
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"

[lb]
enabled = true
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let lb = manifest.lb.expect("lb config should be present");
        assert!(lb.enabled);
        // Check defaults are applied
        assert_eq!(lb.health_check_type, "http");
        assert_eq!(lb.health_check_interval_ms, 5000);
        assert_eq!(lb.health_check_timeout_ms, 2000);
        assert_eq!(lb.health_check_path, "/health");
        assert_eq!(lb.unhealthy_threshold, 3);
        assert_eq!(lb.healthy_threshold, 2);
        assert_eq!(lb.request_timeout_secs, 30);
        assert_eq!(lb.max_connections_per_backend, 100);
    }

    #[test]
    fn test_parse_manifest_without_lb() {
        // Test that lb is None when not specified
        let toml = r#"
[project]
name = "my-app"
version = "0.1.0"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        assert!(manifest.lb.is_none());
    }
}
