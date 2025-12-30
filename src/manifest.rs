//! mik.toml manifest format.
//!
//! Similar to Cargo.toml, pyproject.toml, package.json but for WASI components.

use crate::reliability::is_http_host_allowed;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;
use url::Url;

/// The main manifest structure (mik.toml).
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub tracing: TracingConfig,
    #[serde(default)]
    pub composition: CompositionConfig,
    #[serde(default)]
    pub lb: Option<LbConfig>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, Dependency>,
}

/// Tracing configuration for observability.
///
/// Controls distributed tracing and OTLP export to backends like Jaeger, Tempo, or Zipkin.
///
/// # Example
///
/// ```toml
/// [tracing]
/// enabled = true
/// otlp_endpoint = "http://localhost:4317"
/// service_name = "my-service"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Enable distributed tracing (default: true).
    ///
    /// When enabled, trace IDs are generated and propagated through requests.
    #[serde(default = "default_tracing_enabled")]
    pub enabled: bool,
    /// OTLP exporter endpoint (optional).
    ///
    /// When set, traces are exported via OTLP to backends like Jaeger or Tempo.
    /// Requires the `otlp` feature to be enabled.
    ///
    /// Example: `"http://localhost:4317"` (gRPC) or `"http://localhost:4318"` (HTTP)
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    /// Service name for traces (default: "mik").
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

fn default_tracing_enabled() -> bool {
    true
}

fn default_service_name() -> String {
    "mik".to_string()
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: default_tracing_enabled(),
            otlp_endpoint: None,
            service_name: default_service_name(),
        }
    }
}

/// Server configuration for the host runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Auto-configure based on system resources (default: true).
    ///
    /// When enabled, `cache_size`, `max_concurrent_requests`, and `max_per_module_requests`
    /// are automatically tuned based on available RAM and CPU cores.
    /// Set to false for predictable/reproducible deployments.
    #[serde(default = "default_auto")]
    pub auto: bool,
    /// Port to listen on (default: 3000)
    #[serde(default = "default_port")]
    pub port: u16,
    /// Directory containing WASM modules (default: "modules/")
    #[serde(default = "default_modules_dir")]
    pub modules: String,
    /// Directory containing JS/TS orchestration scripts (optional)
    #[serde(default)]
    pub scripts: Option<String>,
    /// Directory containing static files (default: "collected-static/")
    #[serde(default)]
    pub r#static: Option<String>,
    /// Maximum number of modules to cache (0 = auto-detect based on RAM).
    #[serde(default)]
    pub cache_size: usize,
    /// Maximum cache memory in MB (0 = auto-detect, ~10% of RAM).
    #[serde(default)]
    pub max_cache_mb: usize,
    /// Maximum request body size in MB (default: 10)
    #[serde(default = "default_max_body_size_mb")]
    pub max_body_size_mb: usize,
    /// WASM execution timeout in seconds (default: 30)
    #[serde(default = "default_execution_timeout")]
    pub execution_timeout_secs: u64,
    /// Maximum concurrent requests (0 = auto-detect based on CPU cores).
    #[serde(default)]
    pub max_concurrent_requests: usize,
    /// Maximum concurrent requests per module (0 = auto-detect).
    #[serde(default)]
    pub max_per_module_requests: usize,
    /// Graceful shutdown drain timeout in seconds (default: 30)
    #[serde(default = "default_shutdown_timeout")]
    pub shutdown_timeout_secs: u64,
    /// Enable wasi:logging for WASM modules (default: false).
    #[serde(default)]
    pub logging: bool,
    /// Allowed hosts for outgoing HTTP requests (default: empty = disabled).
    ///
    /// - `[]` or missing = disabled (no outgoing HTTP)
    /// - `["*"]` = allow all hosts
    /// - `["api.example.com", "*.supabase.co"]` = specific hosts only
    #[serde(default)]
    pub http_allowed: Vec<String>,
}

fn default_auto() -> bool {
    true
}

fn default_port() -> u16 {
    3000
}

fn default_modules_dir() -> String {
    "modules/".to_string()
}

fn default_max_body_size_mb() -> usize {
    10
}

fn default_execution_timeout() -> u64 {
    30
}

fn default_shutdown_timeout() -> u64 {
    30
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            auto: default_auto(),
            port: default_port(),
            modules: default_modules_dir(),
            scripts: None,
            r#static: None,
            cache_size: 0,   // 0 = auto-detect
            max_cache_mb: 0, // 0 = auto-detect
            max_body_size_mb: default_max_body_size_mb(),
            execution_timeout_secs: default_execution_timeout(),
            max_concurrent_requests: 0, // 0 = auto-detect
            max_per_module_requests: 0, // 0 = auto-detect
            shutdown_timeout_secs: default_shutdown_timeout(),
            logging: false,
            http_allowed: Vec::new(),
        }
    }
}

/// Composition configuration for WASM component composition.
///
/// Controls automatic composition with mik-sdk bridge component
/// to convert handlers to WASI HTTP compatible components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionConfig {
    /// Enable automatic HTTP handler composition (default: true for "app" type).
    ///
    /// When enabled, handlers exporting `mik:core/handler` are automatically
    /// composed with bridge to export `wasi:http/incoming-handler`.
    #[serde(default = "default_http_handler")]
    pub http_handler: bool,
    /// Path to bridge component (optional, downloads from registry if not set).
    #[serde(default)]
    pub bridge: Option<String>,
}

fn default_http_handler() -> bool {
    true
}

impl Default for CompositionConfig {
    fn default() -> Self {
        Self {
            http_handler: default_http_handler(),
            bridge: None,
        }
    }
}

/// Load balancer configuration.
///
/// Controls the L7 load balancer that distributes requests across multiple backend workers.
///
/// # Example
///
/// ```toml
/// [lb]
/// enabled = true
/// algorithm = "round_robin"
/// health_check_type = "http"  # or "tcp"
/// health_check_interval_ms = 5000
/// health_check_timeout_ms = 2000
/// health_check_path = "/health"  # only used when health_check_type = "http"
/// unhealthy_threshold = 3
/// healthy_threshold = 2
/// request_timeout_secs = 30
/// max_connections_per_backend = 100
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LbConfig {
    /// Enable load balancing (default: false).
    #[serde(default = "default_lb_enabled")]
    pub enabled: bool,
    /// Load balancing algorithm (default: "round_robin").
    ///
    /// Options: "round_robin", "weighted", "consistent_hash"
    #[serde(default = "default_lb_algorithm")]
    pub algorithm: String,
    /// Type of health check to perform (default: "http").
    ///
    /// Options:
    /// - "http" - HTTP GET request to health_check_path, expects 2xx response
    /// - "tcp" - TCP connection check, just verifies port is accepting connections
    #[serde(default = "default_health_check_type")]
    pub health_check_type: String,
    /// Interval between health checks in milliseconds (default: 5000).
    #[serde(default = "default_health_check_interval_ms")]
    pub health_check_interval_ms: u64,
    /// Timeout for each health check request in milliseconds (default: 2000).
    #[serde(default = "default_health_check_timeout_ms")]
    pub health_check_timeout_ms: u64,
    /// Path to check for HTTP health checks (default: "/health").
    /// Only used when health_check_type = "http".
    #[serde(default = "default_health_check_path")]
    pub health_check_path: String,
    /// Number of consecutive failures before marking backend unhealthy (default: 3).
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,
    /// Number of consecutive successes before marking backend healthy (default: 2).
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
    /// Request timeout in seconds (default: 30).
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Maximum concurrent connections per backend (default: 100).
    #[serde(default = "default_max_connections_per_backend")]
    pub max_connections_per_backend: usize,
    /// Pool idle timeout in seconds (default: 90).
    /// Connections idle longer than this are closed.
    #[serde(default = "default_pool_idle_timeout_secs")]
    pub pool_idle_timeout_secs: u64,
    /// TCP keepalive interval in seconds (default: 60).
    #[serde(default = "default_tcp_keepalive_secs")]
    pub tcp_keepalive_secs: u64,
    /// Use HTTP/2 only (with prior knowledge) for backend connections (default: false).
    /// Enable this when all backends support HTTP/2 for better performance.
    #[serde(default = "default_http2_only")]
    pub http2_only: bool,
}

fn default_lb_enabled() -> bool {
    false
}

fn default_lb_algorithm() -> String {
    "round_robin".to_string()
}

fn default_health_check_type() -> String {
    "http".to_string()
}

fn default_health_check_interval_ms() -> u64 {
    5000
}

fn default_health_check_timeout_ms() -> u64 {
    2000
}

fn default_health_check_path() -> String {
    "/health".to_string()
}

fn default_unhealthy_threshold() -> u32 {
    3
}

fn default_healthy_threshold() -> u32 {
    2
}

fn default_request_timeout_secs() -> u64 {
    30
}

fn default_max_connections_per_backend() -> usize {
    100
}

fn default_pool_idle_timeout_secs() -> u64 {
    90
}

fn default_tcp_keepalive_secs() -> u64 {
    60
}

fn default_http2_only() -> bool {
    false
}

impl Default for LbConfig {
    fn default() -> Self {
        Self {
            enabled: default_lb_enabled(),
            algorithm: default_lb_algorithm(),
            health_check_type: default_health_check_type(),
            health_check_interval_ms: default_health_check_interval_ms(),
            health_check_timeout_ms: default_health_check_timeout_ms(),
            health_check_path: default_health_check_path(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
            request_timeout_secs: default_request_timeout_secs(),
            max_connections_per_backend: default_max_connections_per_backend(),
            pool_idle_timeout_secs: default_pool_idle_timeout_secs(),
            tcp_keepalive_secs: default_tcp_keepalive_secs(),
            http2_only: default_http2_only(),
        }
    }
}

impl ServerConfig {
    /// Check if outgoing HTTP is enabled.
    #[allow(dead_code)] // Public API for host runtime
    pub fn http_enabled(&self) -> bool {
        !self.http_allowed.is_empty()
    }

    /// Check if all hosts are allowed for outgoing HTTP.
    #[allow(dead_code)] // Public API for host runtime
    pub fn http_allows_all(&self) -> bool {
        self.http_allowed.iter().any(|h| h == "*")
    }

    /// Check if a specific host is allowed for outgoing HTTP.
    ///
    /// Delegates to `mikrozen_reliability::is_http_host_allowed` which is the
    /// single source of truth for this logic.
    #[allow(dead_code)] // Public API for host runtime
    pub fn is_host_allowed(&self, host: &str) -> bool {
        is_http_host_allowed(host, &self.http_allowed)
    }
}

/// Project metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<Author>,
    /// Project language: rust (default), typescript, python, c
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// Author information.
#[derive(Debug, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// Dependency specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    /// Simple version string: "1.0"
    Simple(String),
    /// Detailed specification
    Detailed(DependencyDetail),
}

/// Detailed dependency specification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyDetail {
    /// Version requirement
    #[serde(default)]
    pub version: Option<String>,
    /// Git repository URL
    #[serde(default)]
    pub git: Option<String>,
    /// Git branch
    #[serde(default)]
    pub branch: Option<String>,
    /// Git tag
    #[serde(default)]
    pub tag: Option<String>,
    /// Git revision
    #[serde(default)]
    pub rev: Option<String>,
    /// Local path
    #[serde(default)]
    pub path: Option<String>,
    /// Registry URL (ghcr.io, custom registry)
    #[serde(default)]
    pub registry: Option<String>,
}

/// Validation errors for manifest fields.
#[derive(Debug)]
enum ValidationError {
    EmptyProjectName,
    InvalidProjectName(String),
    EmptyVersion,
    InvalidVersion(String),
    InvalidPort,
    EmptyModulesDir,
    DependencyError(String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProjectName => {
                write!(f, "Project name is required in [project] section")
            },
            Self::InvalidProjectName(name) => {
                write!(
                    f,
                    "Invalid project name '{name}'. Names must:\n  \
                     - Start with a letter or underscore\n  \
                     - Contain only letters, numbers, hyphens, and underscores\n  \
                     - Be between 1 and 64 characters\n  \
                     Example: 'my-component' or 'my_component'"
                )
            },
            Self::EmptyVersion => {
                write!(f, "Project version is required in [project] section")
            },
            Self::InvalidVersion(version) => {
                write!(
                    f,
                    "Invalid version '{version}'. Version must follow semantic versioning (e.g., '0.1.0', '1.2.3')\n  \
                     Format: MAJOR.MINOR.PATCH\n  \
                     See: https://semver.org/"
                )
            },
            Self::InvalidPort => {
                write!(
                    f,
                    "Server port cannot be 0. Use a valid port number (1-65535)\n  \
                     Common ports: 3000 (default), 8080, 8000"
                )
            },
            Self::EmptyModulesDir => {
                write!(
                    f,
                    "Server modules directory cannot be empty\n  \
                     Fix: Set modules = \"modules/\" in [server] section"
                )
            },
            Self::DependencyError(msg) => write!(f, "{msg}"),
        }
    }
}

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

    /// Validate the manifest for common errors.
    ///
    /// This checks:
    /// - Required fields are non-empty
    /// - Project name follows naming conventions
    /// - Version follows semantic versioning
    /// - Server configuration is sensible
    /// - Component references exist (for path dependencies)
    /// - Dependencies have valid specifications
    ///
    /// # Errors
    ///
    /// Returns detailed error messages that tell the user exactly what's wrong
    /// and how to fix it.
    pub fn validate(&self, manifest_path: &Path) -> Result<()> {
        let mut errors = Vec::new();

        // 1. Validate project name
        if self.project.name.is_empty() {
            errors.push(ValidationError::EmptyProjectName);
        } else if !Self::is_valid_name(&self.project.name) {
            errors.push(ValidationError::InvalidProjectName(
                self.project.name.clone(),
            ));
        }

        // 2. Validate version
        if self.project.version.is_empty() {
            errors.push(ValidationError::EmptyVersion);
        } else if !Self::is_valid_version(&self.project.version) {
            errors.push(ValidationError::InvalidVersion(
                self.project.version.clone(),
            ));
        }

        // 3. Validate server configuration
        if self.server.port == 0 {
            errors.push(ValidationError::InvalidPort);
        }

        // Note: cache_size = 0 is valid (means auto-detect)

        if self.server.modules.is_empty() {
            errors.push(ValidationError::EmptyModulesDir);
        }

        // 4. Validate dependencies
        let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));

        for (name, dep) in &self.dependencies {
            if let Err(e) = Self::validate_dependency(name, dep, manifest_dir, false) {
                errors.push(ValidationError::DependencyError(e));
            }
        }

        // 5. Validate dev-dependencies
        for (name, dep) in &self.dev_dependencies {
            if let Err(e) = Self::validate_dependency(name, dep, manifest_dir, true) {
                errors.push(ValidationError::DependencyError(e));
            }
        }

        // If there are errors, format them nicely and return
        if !errors.is_empty() {
            let error_list = errors
                .iter()
                .enumerate()
                .map(|(i, e)| format!("  {}. {}", i + 1, e))
                .collect::<Vec<_>>()
                .join("\n\n");

            anyhow::bail!(
                "Validation failed for {}:\n\n{}\n\n\
                 Run 'mik new --help' to see valid configuration options.",
                manifest_path.display(),
                error_list
            );
        }

        Ok(())
    }

    /// Validate empty fields in detailed dependency.
    fn validate_detail_empty_fields(
        name: &str,
        detail: &DependencyDetail,
        dep_type: &str,
    ) -> Result<(), String> {
        if let Some(version) = &detail.version
            && version.is_empty()
        {
            return Err(format!(
                "{dep_type} '{name}' has empty version string\n  \
                 Fix: Specify a version like: {name} = \"1.0\""
            ));
        }
        if let Some(git) = &detail.git
            && git.is_empty()
        {
            return Err(format!(
                "{dep_type} '{name}' has empty git URL\n  \
                 Fix: Specify a git URL like: git = \"https://github.com/user/repo.git\""
            ));
        }
        if let Some(path) = &detail.path
            && path.is_empty()
        {
            return Err(format!(
                "{dep_type} '{name}' has empty path\n  \
                 Fix: Specify a path like: path = \"../component.wasm\""
            ));
        }
        if let Some(registry) = &detail.registry
            && registry.is_empty()
        {
            return Err(format!(
                "{dep_type} '{name}' has empty registry URL\n  \
                 Fix: Specify a registry like: registry = \"ghcr.io/user/component\""
            ));
        }
        Ok(())
    }

    /// Validate dependency sources (version, git, path, registry).
    fn validate_dependency_sources(
        name: &str,
        detail: &DependencyDetail,
        dep_type: &str,
    ) -> Result<(), String> {
        let has_version = detail.version.as_ref().is_some_and(|v| !v.is_empty());
        let has_git = detail.git.as_ref().is_some_and(|v| !v.is_empty());
        let has_path = detail.path.as_ref().is_some_and(|v| !v.is_empty());
        let has_registry = detail.registry.as_ref().is_some_and(|v| !v.is_empty());

        let sources = match (has_version, has_git, has_path, has_registry) {
            // Valid single-source combinations
            (true, false, false, true | false)
            | (false, true, false, false)
            | (false, false, true, false)
            | (false, false, false, true) => return Ok(()),
            // No sources specified
            (false, false, false, false) => {
                return Err(format!(
                    "{dep_type} '{name}' must specify at least one source:\n  \
                     - version = \"1.0\" (from default registry)\n  \
                     - git = \"https://github.com/...\"\n  \
                     - path = \"../local/path\"\n  \
                     - registry = \"ghcr.io/user/component\" (requires version)"
                ));
            },
            // Multiple sources - build error list
            _ => {
                let mut sources_list = vec![];
                if has_version {
                    sources_list.push("version");
                }
                if has_git {
                    sources_list.push("git");
                }
                if has_path {
                    sources_list.push("path");
                }
                if has_registry {
                    sources_list.push("registry");
                }
                if has_registry && has_version && !has_git && !has_path {
                    return Ok(());
                }
                sources_list
            },
        };

        Err(format!(
            "{dep_type} '{name}' specifies multiple sources: {}\n  \
             Only one source type is allowed per dependency\n  \
             Note: registry requires both 'registry' and 'version' fields",
            sources.join(", ")
        ))
    }

    /// Validate path dependency exists and has correct extension.
    fn validate_path_dependency(
        name: &str,
        path: &str,
        manifest_dir: &Path,
        dep_type: &str,
    ) -> Result<(), String> {
        let dep_path = manifest_dir.join(path);
        if !dep_path.exists() {
            return Err(format!(
                "{dep_type} '{name}' references non-existent path: {path}\n  \
                 Current directory: {}\n  Resolved path: {}\n  \
                 Fix: Ensure the component file exists or update the path",
                manifest_dir.display(),
                dep_path.display()
            ));
        }

        let has_wasm_ext = std::path::Path::new(path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wasm"));
        if !has_wasm_ext && !dep_path.is_dir() {
            return Err(format!(
                "{dep_type} '{name}' path '{path}' should be a .wasm file or directory\n  \
                 Current: {}\n  Expected: {name}.wasm or a directory containing mik.toml",
                dep_path.display()
            ));
        }
        Ok(())
    }

    /// Validate git dependency URL and refs.
    fn validate_git_dependency(
        name: &str,
        detail: &DependencyDetail,
        dep_type: &str,
    ) -> Result<(), String> {
        let Some(git) = &detail.git else {
            return Ok(());
        };

        if !Self::is_valid_git_url(git) {
            return Err(format!(
                "{dep_type} '{name}' has invalid git URL: {git}\n  \
                 Valid formats:\n  \
                 - https://github.com/user/repo.git\n  \
                 - git://github.com/user/repo.git\n  \
                 - ssh://git@github.com/user/repo.git\n  \
                 - git@github.com:user/repo.git"
            ));
        }

        let git_refs: Vec<_> = [
            detail.branch.as_ref().map(|_| "branch"),
            detail.tag.as_ref().map(|_| "tag"),
            detail.rev.as_ref().map(|_| "rev"),
        ]
        .iter()
        .filter_map(|&s| s)
        .collect();

        if git_refs.len() > 1 {
            return Err(format!(
                "{dep_type} '{name}' specifies multiple git refs: {}\n  \
                 Only one of branch, tag, or rev is allowed",
                git_refs.join(", ")
            ));
        }
        Ok(())
    }

    /// Validate a single dependency.
    fn validate_dependency(
        name: &str,
        dep: &Dependency,
        manifest_dir: &Path,
        is_dev: bool,
    ) -> Result<(), String> {
        let dep_type = if is_dev {
            "dev-dependency"
        } else {
            "dependency"
        };

        if name.is_empty() {
            return Err(format!(
                "Empty {dep_type} name found. Dependency names must be non-empty"
            ));
        }

        match dep {
            Dependency::Simple(version) => {
                if version.is_empty() {
                    return Err(format!(
                        "{dep_type} '{name}' has empty version string\n  \
                         Fix: Specify a version like: {name} = \"1.0\""
                    ));
                }
            },
            Dependency::Detailed(detail) => {
                Self::validate_detail_empty_fields(name, detail, dep_type)?;
                Self::validate_dependency_sources(name, detail, dep_type)?;

                if let Some(path) = &detail.path {
                    Self::validate_path_dependency(name, path, manifest_dir, dep_type)?;
                }

                Self::validate_git_dependency(name, detail, dep_type)?;

                if detail.registry.is_some() && detail.version.as_ref().is_none_or(String::is_empty)
                {
                    return Err(format!(
                        "{dep_type} '{name}' with registry must specify a version\n  \
                         Fix: Add version = \"1.0\" alongside registry"
                    ));
                }
            },
        }

        Ok(())
    }

    /// Check if a project name is valid.
    ///
    /// Valid names:
    /// - Start with a letter (a-z, A-Z) or underscore (_)
    /// - Contain only letters, numbers, hyphens (-), and underscores (_)
    /// - Between 1 and 64 characters
    fn is_valid_name(name: &str) -> bool {
        if name.is_empty() || name.len() > 64 {
            return false;
        }

        // Security: reject path traversal attempts (defense in depth)
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return false;
        }

        let mut chars = name.chars();

        // First character must be letter or underscore
        if let Some(first) = chars.next()
            && !first.is_ascii_alphabetic()
            && first != '_'
        {
            return false;
        }

        // Rest can be letters, numbers, hyphens, or underscores
        chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    /// Check if a version string follows semantic versioning.
    ///
    /// Uses the `semver` crate for proper validation of:
    /// - "1.0.0", "0.1.0" (basic versions)
    /// - "2.3.4-alpha", "1.0.0-rc.1" (prerelease)
    /// - "1.0.0+build.123" (build metadata)
    fn is_valid_version(version: &str) -> bool {
        semver::Version::parse(version).is_ok()
    }

    /// Validate a git URL using the `url` crate.
    ///
    /// Accepts:
    /// - `https://github.com/user/repo.git`
    /// - `git://github.com/user/repo.git`
    /// - `ssh://git@github.com/user/repo.git`
    /// - `git@github.com:user/repo.git` (SCP-style)
    fn is_valid_git_url(git: &str) -> bool {
        // Handle SCP-style URLs (git@host:path)
        if git.starts_with("git@") {
            // Must have : after host and some path
            return git.find(':').is_some_and(|pos| git.len() > pos + 1);
        }

        // Standard URL parsing for http/https/git/ssh schemes
        match Url::parse(git) {
            Ok(url) => {
                let scheme = url.scheme();
                matches!(scheme, "http" | "https" | "git" | "ssh") && url.host().is_some()
            },
            Err(_) => false,
        }
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            project: Project {
                name: "my-component".to_string(),
                version: default_version(),
                description: None,
                authors: vec![],
                language: None,
            },
            server: ServerConfig::default(),
            tracing: TracingConfig::default(),
            composition: CompositionConfig::default(),
            lb: None,
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
        }
    }
}

#[allow(dead_code)]
impl Dependency {
    /// Create a simple version dependency.
    pub fn version(version: &str) -> Self {
        Dependency::Simple(version.to_string())
    }

    /// Create a git dependency.
    pub fn git(url: &str) -> Self {
        Dependency::Detailed(DependencyDetail {
            git: Some(url.to_string()),
            ..Default::default()
        })
    }

    /// Create a registry dependency (ghcr.io, etc).
    pub fn registry(registry: &str, version: &str) -> Self {
        Dependency::Detailed(DependencyDetail {
            registry: Some(registry.to_string()),
            version: Some(version.to_string()),
            ..Default::default()
        })
    }

    /// Create a path dependency.
    pub fn path(path: &str) -> Self {
        Dependency::Detailed(DependencyDetail {
            path: Some(path.to_string()),
            ..Default::default()
        })
    }
}

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
        assert_eq!(config.algorithm, "round_robin");
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
algorithm = "weighted"
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
        assert_eq!(lb.algorithm, "weighted");
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
algorithm = "consistent_hash"
"#;
        let manifest: Manifest = toml::from_str(toml).unwrap();
        let lb = manifest.lb.expect("lb config should be present");
        assert!(lb.enabled);
        assert_eq!(lb.algorithm, "consistent_hash");
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
