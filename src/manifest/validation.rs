//! Validation logic for manifest fields.
//!
//! This module contains all validation-related types and functions for
//! ensuring manifest correctness.

use std::fmt;
use std::path::Path;
use url::Url;

use super::types::{Dependency, DependencyDetail, Manifest};

// =============================================================================
// Validation Error Types
// =============================================================================

/// Validation errors for manifest fields.
#[derive(Debug)]
pub enum ValidationError {
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

// =============================================================================
// Manifest Validation Implementation
// =============================================================================

impl Manifest {
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
    pub fn validate(&self, manifest_path: &Path) -> anyhow::Result<()> {
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
            if let Err(e) = validate_dependency(name, dep, manifest_dir, false) {
                errors.push(ValidationError::DependencyError(e));
            }
        }

        // 5. Validate dev-dependencies
        for (name, dep) in &self.dev_dependencies {
            if let Err(e) = validate_dependency(name, dep, manifest_dir, true) {
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

    /// Check if a project name is valid.
    ///
    /// Valid names:
    /// - Start with a letter (a-z, A-Z) or underscore (_)
    /// - Contain only letters, numbers, hyphens (-), and underscores (_)
    /// - Between 1 and 64 characters
    pub fn is_valid_name(name: &str) -> bool {
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
    pub fn is_valid_version(version: &str) -> bool {
        semver::Version::parse(version).is_ok()
    }

    /// Validate a git URL using the `url` crate.
    ///
    /// Accepts:
    /// - `https://github.com/user/repo.git`
    /// - `git://github.com/user/repo.git`
    /// - `ssh://git@github.com/user/repo.git`
    /// - `git@github.com:user/repo.git` (SCP-style)
    #[allow(dead_code)] // Public API method
    pub fn is_valid_git_url(git: &str) -> bool {
        is_valid_git_url(git)
    }
}

// =============================================================================
// Dependency Validation Functions
// =============================================================================

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

    if !is_valid_git_url(git) {
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
            validate_detail_empty_fields(name, detail, dep_type)?;
            validate_dependency_sources(name, detail, dep_type)?;

            if let Some(path) = &detail.path {
                validate_path_dependency(name, path, manifest_dir, dep_type)?;
            }

            validate_git_dependency(name, detail, dep_type)?;

            if detail.registry.is_some() && detail.version.as_ref().is_none_or(String::is_empty) {
                return Err(format!(
                    "{dep_type} '{name}' with registry must specify a version\n  \
                     Fix: Add version = \"1.0\" alongside registry"
                ));
            }
        },
    }

    Ok(())
}

/// Validate a git URL.
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
