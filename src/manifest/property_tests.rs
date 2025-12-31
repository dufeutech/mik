//! Property-based tests for manifest parsing and validation.
//!
//! These tests verify invariants for the mik.toml manifest system:
//! - Valid manifests parse and serialize correctly (roundtrip)
//! - Name validation rejects path traversal attempts
//! - Version validation follows semver
//! - Dependencies are validated correctly
//!
//! Run with:
//! ```bash
//! cargo test --lib manifest::property_tests
//! ```

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::types::{
    CompositionConfig, Dependency, DependencyDetail, Manifest, Project, ServerConfig, TracingConfig,
};

// ============================================================================
// Test Strategies - Input Generation
// ============================================================================

/// Strategy for generating valid project names.
///
/// Valid names start with letter/underscore, contain only alphanumeric + hyphen/underscore,
/// and are 1-64 characters.
fn valid_project_name() -> impl Strategy<Value = String> {
    "[a-zA-Z_][a-zA-Z0-9_-]{0,63}".prop_filter("must not be empty", |s| !s.is_empty())
}

/// Strategy for generating invalid project names (for rejection testing).
fn invalid_project_name() -> impl Strategy<Value = String> {
    prop_oneof![
        // Empty
        Just(String::new()),
        // Starts with number
        Just("123name".to_string()),
        // Starts with hyphen
        Just("-name".to_string()),
        // Contains spaces
        Just("my name".to_string()),
        // Contains special chars
        Just("my@name".to_string()),
        // Too long (65 chars)
        Just("a".repeat(65)),
        // Path traversal attempts
        Just("../etc/passwd".to_string()),
        Just("..".to_string()),
        Just("foo/bar".to_string()),
        Just("foo\\bar".to_string()),
    ]
}

/// Strategy for generating valid semver versions.
fn valid_version() -> impl Strategy<Value = String> {
    prop_oneof![
        // Basic versions
        (0u32..100, 0u32..100, 0u32..100)
            .prop_map(|(major, minor, patch)| format!("{major}.{minor}.{patch}")),
        // With prerelease
        Just("1.0.0-alpha".to_string()),
        Just("1.0.0-beta.1".to_string()),
        Just("2.3.4-rc.1".to_string()),
        // With build metadata
        Just("1.0.0+build.123".to_string()),
    ]
}

/// Strategy for generating invalid versions.
fn invalid_version() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("1".to_string()),
        Just("1.0".to_string()),
        Just("v1.0.0".to_string()), // Leading 'v'
        Just("1.0.0.0".to_string()),
        Just("abc".to_string()),
    ]
}

/// Strategy for generating valid port numbers.
fn valid_port() -> impl Strategy<Value = u16> {
    1u16..=65535u16
}

/// Strategy for generating valid git URLs.
fn valid_git_url() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("https://github.com/user/repo.git".to_string()),
        Just("git://github.com/user/repo.git".to_string()),
        Just("ssh://git@github.com/user/repo.git".to_string()),
        Just("git@github.com:user/repo.git".to_string()),
    ]
}

/// Strategy for generating invalid git URLs.
fn invalid_git_url() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("not-a-url".to_string()),
        Just("ftp://example.com/repo".to_string()),
        Just("just-text".to_string()),
    ]
}

/// Strategy for generating valid Project structs.
fn valid_project() -> impl Strategy<Value = Project> {
    (valid_project_name(), valid_version()).prop_map(|(name, version)| Project {
        name,
        version,
        description: None,
        authors: vec![],
        language: None,
    })
}

/// Strategy for generating valid simple dependencies.
fn valid_simple_dependency() -> impl Strategy<Value = (String, Dependency)> {
    (valid_project_name(), valid_version())
        .prop_map(|(name, version)| (name, Dependency::Simple(version)))
}

/// Strategy for generating valid git dependencies.
#[allow(dead_code)]
fn valid_git_dependency() -> impl Strategy<Value = (String, Dependency)> {
    (valid_project_name(), valid_git_url()).prop_map(|(name, git)| {
        (
            name,
            Dependency::Detailed(DependencyDetail {
                git: Some(git),
                ..DependencyDetail::default()
            }),
        )
    })
}

// ============================================================================
// Name Validation Invariants
// ============================================================================

proptest! {
    /// Invariant: Valid project names pass validation.
    #[test]
    fn valid_names_pass(name in valid_project_name()) {
        prop_assert!(
            Manifest::is_valid_name(&name),
            "Valid name should pass: {}",
            name
        );
    }

    /// Invariant: Invalid project names are rejected.
    #[test]
    fn invalid_names_rejected(name in invalid_project_name()) {
        prop_assert!(
            !Manifest::is_valid_name(&name),
            "Invalid name should be rejected: {}",
            name
        );
    }

    /// Invariant: Names with path separators are always rejected.
    ///
    /// This is critical for security - path separators could enable traversal.
    #[test]
    fn names_with_path_separators_rejected(
        prefix in "[a-zA-Z]{1,10}",
        suffix in "[a-zA-Z]{1,10}"
    ) {
        let with_forward_slash = format!("{prefix}/{suffix}");
        let with_backslash = format!("{prefix}\\{suffix}");
        let with_dotdot = format!("{prefix}/../{suffix}");

        prop_assert!(
            !Manifest::is_valid_name(&with_forward_slash),
            "Name with / should be rejected"
        );
        prop_assert!(
            !Manifest::is_valid_name(&with_backslash),
            "Name with \\ should be rejected"
        );
        prop_assert!(
            !Manifest::is_valid_name(&with_dotdot),
            "Name with .. should be rejected"
        );
    }

    /// Invariant: Names longer than 64 characters are rejected.
    #[test]
    fn names_over_64_chars_rejected(extra_len in 1usize..100) {
        let long_name = "a".repeat(64 + extra_len);
        prop_assert!(
            !Manifest::is_valid_name(&long_name),
            "Name over 64 chars should be rejected: len={}",
            long_name.len()
        );
    }

    /// Invariant: Names at exactly 64 characters are valid.
    #[test]
    fn names_at_64_chars_valid(_dummy in Just(())) {
        let name_64 = format!("a{}", "b".repeat(63));
        prop_assert!(
            Manifest::is_valid_name(&name_64),
            "Name at 64 chars should be valid"
        );
    }
}

// ============================================================================
// Version Validation Invariants
// ============================================================================

proptest! {
    /// Invariant: Valid semver versions pass validation.
    #[test]
    fn valid_versions_pass(version in valid_version()) {
        prop_assert!(
            Manifest::is_valid_version(&version),
            "Valid version should pass: {}",
            version
        );
    }

    /// Invariant: Invalid versions are rejected.
    #[test]
    fn invalid_versions_rejected(version in invalid_version()) {
        prop_assert!(
            !Manifest::is_valid_version(&version),
            "Invalid version should be rejected: {}",
            version
        );
    }

    /// Invariant: Semver with valid prerelease tags passes.
    #[test]
    fn prerelease_versions_valid(
        major in 0u32..10,
        minor in 0u32..10,
        patch in 0u32..10,
        tag in prop_oneof![Just("alpha"), Just("beta"), Just("rc")]
    ) {
        let version = format!("{major}.{minor}.{patch}-{tag}");
        prop_assert!(
            Manifest::is_valid_version(&version),
            "Prerelease version should be valid: {}",
            version
        );
    }
}

// ============================================================================
// Manifest Serialization Roundtrip
// ============================================================================

proptest! {
    /// Invariant: Manifest serialization roundtrips.
    ///
    /// Serialize to TOML and parse back should yield equivalent manifest.
    #[test]
    fn manifest_serialization_roundtrip(project in valid_project()) {
        let manifest = Manifest {
            project: project.clone(),
            server: ServerConfig::default(),
            tracing: TracingConfig::default(),
            composition: CompositionConfig::default(),
            lb: None,
            dependencies: BTreeMap::default(),
            dev_dependencies: BTreeMap::default(),
        };

        // Serialize to TOML
        let toml_str = toml::to_string(&manifest);
        prop_assert!(toml_str.is_ok(), "Serialization should succeed");

        // Parse back
        let parsed: Result<Manifest, _> = toml::from_str(&toml_str.unwrap());
        prop_assert!(parsed.is_ok(), "Deserialization should succeed");

        // Verify key fields match
        let parsed = parsed.unwrap();
        prop_assert_eq!(parsed.project.name, project.name);
        prop_assert_eq!(parsed.project.version, project.version);
    }

    /// Invariant: Default manifest is serializable.
    #[test]
    fn default_manifest_serializable(_dummy in Just(())) {
        let manifest = Manifest::default();
        let toml_str = toml::to_string(&manifest);
        prop_assert!(toml_str.is_ok(), "Default manifest should serialize");
    }
}

// ============================================================================
// Git URL Validation
// ============================================================================

proptest! {
    /// Invariant: Valid git URLs pass validation.
    #[test]
    fn valid_git_urls_pass(url in valid_git_url()) {
        prop_assert!(
            Manifest::is_valid_git_url(&url),
            "Valid git URL should pass: {}",
            url
        );
    }

    /// Invariant: Invalid git URLs are rejected.
    #[test]
    fn invalid_git_urls_rejected(url in invalid_git_url()) {
        prop_assert!(
            !Manifest::is_valid_git_url(&url),
            "Invalid git URL should be rejected: {}",
            url
        );
    }

    /// Invariant: SCP-style git URLs are valid.
    #[test]
    fn scp_style_urls_valid(user in "[a-z]+", host in "[a-z]+\\.[a-z]+", path in "[a-z/]+") {
        let url = format!("{user}@{host}:{path}");
        if url.starts_with("git@") {
            prop_assert!(
                Manifest::is_valid_git_url(&url),
                "SCP-style URL should be valid: {}",
                url
            );
        }
    }
}

// ============================================================================
// ServerConfig Validation
// ============================================================================

proptest! {
    /// Invariant: Port 0 is invalid.
    #[test]
    fn port_zero_invalid(_dummy in Just(())) {
        // Port 0 should cause validation to fail
        let manifest = Manifest {
            project: Project {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                authors: vec![],
                language: None,
            },
            server: ServerConfig {
                port: 0,
                ..Default::default()
            },
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        prop_assert!(result.is_err(), "Port 0 should fail validation");
    }

    /// Invariant: Valid ports pass validation.
    #[test]
    fn valid_ports_pass(port in valid_port()) {
        let manifest = Manifest {
            project: Project {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                authors: vec![],
                language: None,
            },
            server: ServerConfig {
                port,
                ..Default::default()
            },
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        // May still fail for other reasons (modules dir), but port should be valid
        if let Err(e) = &result {
            let msg = e.to_string();
            prop_assert!(
                !msg.contains("port cannot be 0"),
                "Valid port {} should not trigger port error",
                port
            );
        }
    }
}

// ============================================================================
// Dependency Validation
// ============================================================================

proptest! {
    /// Invariant: Simple version dependency is valid.
    #[test]
    fn simple_dependency_valid((name, dep) in valid_simple_dependency()) {
        let mut deps = std::collections::BTreeMap::new();
        deps.insert(name.clone(), dep);

        let manifest = Manifest {
            project: Project {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                authors: vec![],
                language: None,
            },
            dependencies: deps,
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        // Should not have dependency-related errors for simple valid deps
        if let Err(e) = &result {
            let msg = e.to_string();
            prop_assert!(
                !msg.contains(&format!("'{name}'")),
                "Valid simple dependency {} should not trigger error",
                name
            );
        }
    }

    /// Invariant: Empty version string is rejected.
    #[test]
    fn empty_version_dependency_rejected(name in valid_project_name()) {
        let mut deps = std::collections::BTreeMap::new();
        deps.insert(name.clone(), Dependency::Simple(String::new()));

        let manifest = Manifest {
            project: Project {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                authors: vec![],
                language: None,
            },
            dependencies: deps,
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        prop_assert!(result.is_err(), "Empty version should fail");
    }

    /// Invariant: Dependency with multiple sources is rejected.
    #[test]
    fn multiple_sources_rejected(name in valid_project_name()) {
        let mut deps = std::collections::BTreeMap::new();
        deps.insert(
            name.clone(),
            Dependency::Detailed(DependencyDetail {
                version: Some("1.0".to_string()),
                git: Some("https://github.com/user/repo.git".to_string()),
                ..Default::default()
            }),
        );

        let manifest = Manifest {
            project: Project {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                authors: vec![],
                language: None,
            },
            dependencies: deps,
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        prop_assert!(result.is_err(), "Multiple sources should fail");
        if let Err(e) = result {
            prop_assert!(
                e.to_string().contains("multiple sources"),
                "Error should mention multiple sources"
            );
        }
    }

    /// Invariant: Dependency with no sources is rejected.
    #[test]
    fn no_sources_rejected(name in valid_project_name()) {
        let mut deps = std::collections::BTreeMap::new();
        deps.insert(
            name.clone(),
            Dependency::Detailed(DependencyDetail::default()),
        );

        let manifest = Manifest {
            project: Project {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                authors: vec![],
                language: None,
            },
            dependencies: deps,
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        prop_assert!(result.is_err(), "No sources should fail");
    }
}

// ============================================================================
// Error Message Quality Tests
// ============================================================================

proptest! {
    /// Invariant: Multiple errors are collected and reported together.
    #[test]
    fn multiple_errors_collected(_dummy in Just(())) {
        let manifest = Manifest {
            project: Project {
                name: "123-invalid".to_string(),  // Invalid name
                version: "1.0".to_string(),        // Invalid version
                description: None,
                authors: vec![],
                language: None,
            },
            server: ServerConfig {
                port: 0,                           // Invalid port
                modules: String::new(),            // Empty modules
                ..Default::default()
            },
            ..Default::default()
        };

        let result = manifest.validate(std::path::Path::new("mik.toml"));
        prop_assert!(result.is_err());

        if let Err(e) = result {
            let msg = e.to_string();
            // Should contain numbered errors
            prop_assert!(msg.contains("1."), "Should have first error numbered");
            prop_assert!(msg.contains("2."), "Should have second error numbered");
        }
    }
}
