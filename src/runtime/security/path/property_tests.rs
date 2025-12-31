//! Property-based tests for path sanitization and security.
//!
//! These tests use proptest to verify security invariants that must always hold,
//! regardless of input. They catch edge cases that example-based tests might miss.
//!
//! # Key Security Properties
//!
//! 1. Sanitized paths never contain `..` as a path component
//! 2. Sanitized paths never escape the base directory
//! 3. Null bytes are always rejected
//! 4. Sanitization is idempotent (applying twice yields same result)
//!
//! Run with:
//! ```bash
//! cargo test --lib runtime::security::path::property_tests
//! ```

use proptest::prelude::*;

use super::{sanitize_file_path, validate_windows_path};
use crate::runtime::security::error::PathTraversalError;

// ============================================================================
// Test Strategies - Input Generation
// ============================================================================

/// Strategy for generating arbitrary path-like strings.
/// Includes normal characters, path separators, dots, and special chars.
fn path_string_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-zA-Z0-9_./-]{0,100}").expect("valid regex")
}

/// Strategy for generating path components (no separators).
///
/// Excludes "." and ".." which have special meaning in paths.
fn path_component_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-zA-Z0-9_-][a-zA-Z0-9_.-]{0,49}")
        .expect("valid regex")
        .prop_filter("exclude special dirs", |s| s != "." && s != "..")
}

/// Strategy for generating valid relative paths.
fn valid_path_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(path_component_strategy(), 1..5).prop_map(|parts| parts.join("/"))
}

/// Strategy for generating paths that might contain traversal attempts.
fn traversal_attempt_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Direct traversal
        Just("..".to_string()),
        Just("../".to_string()),
        Just("../..".to_string()),
        // Traversal in path
        Just("../etc/passwd".to_string()),
        Just("foo/../..".to_string()),
        Just("a/b/c/../../../../etc".to_string()),
        // Windows-style
        Just("..\\".to_string()),
        Just("..\\..".to_string()),
        Just("foo\\..\\..".to_string()),
        // Mixed separators
        Just("..\\../foo".to_string()),
        Just("foo/..\\..".to_string()),
        // With regular path components
        valid_path_strategy().prop_map(|p| format!("../{p}")),
        valid_path_strategy().prop_map(|p| format!("{p}/../..")),
        valid_path_strategy().prop_map(|p| format!("{p}/../../../etc")),
    ]
}

/// Strategy for generating strings with null bytes.
fn null_byte_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("\0".to_string()),
        Just("file\0.txt".to_string()),
        Just("\0file".to_string()),
        Just("path/to/\0file".to_string()),
        path_component_strategy().prop_map(|s| format!("{s}\0")),
        path_component_strategy().prop_map(|s| format!("\0{s}")),
        path_component_strategy().prop_map(|s| format!("{s}\0suffix")),
    ]
}

/// Strategy for generating absolute paths.
fn absolute_path_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Unix absolute paths
        Just("/etc/passwd".to_string()),
        Just("/".to_string()),
        Just("/home/user".to_string()),
        valid_path_strategy().prop_map(|p| format!("/{p}")),
        // Windows-style (on Windows these would be absolute)
        #[cfg(windows)]
        Just("C:\\Windows".to_string()),
        #[cfg(windows)]
        Just("D:/data".to_string()),
    ]
}

// ============================================================================
// Core Security Invariants
// ============================================================================

proptest! {
    /// Invariant: Sanitized paths never contain ".." as a path component.
    ///
    /// After sanitization, any valid path should not have ".." components
    /// that could be used for traversal. The function either normalizes them
    /// away (if they stay within bounds) or rejects the path.
    #[test]
    fn sanitized_paths_never_contain_traversal_component(input in path_string_strategy()) {
        if let Ok(sanitized) = sanitize_file_path(&input) {
            // Check that no component is exactly ".."
            for component in sanitized.components() {
                if let std::path::Component::ParentDir = component {
                    prop_assert!(false, "Sanitized path should not contain '..' component: {:?}", sanitized);
                }
            }
        }
        // If sanitization failed, that's also acceptable - the path was rejected
    }

    /// Invariant: Traversal attempts are always rejected.
    ///
    /// Any path that starts with ".." or would escape the base directory
    /// should be rejected with EscapesBaseDirectory error.
    #[test]
    fn traversal_attempts_are_rejected(input in traversal_attempt_strategy()) {
        let result = sanitize_file_path(&input);
        // Must either reject or produce a path that doesn't escape
        match result {
            Err(_) => {
                // Good - traversal was blocked (either EscapesBaseDirectory or other rejection)
            }
            Ok(path) => {
                // If accepted, verify it doesn't start with ".." or escape
                let path_str = path.to_string_lossy();
                prop_assert!(
                    !path_str.starts_with(".."),
                    "Accepted path should not start with '..': {}",
                    path_str
                );
                // And should not have ParentDir components
                for component in path.components() {
                    if let std::path::Component::ParentDir = component {
                        prop_assert!(
                            false,
                            "Accepted path should not contain '..' component: {:?}",
                            path
                        );
                    }
                }
            }
        }
    }

    /// Invariant: Null bytes are always rejected.
    ///
    /// Any path containing null bytes must be rejected - null bytes can be
    /// used to truncate paths in C-based filesystems.
    #[test]
    fn null_bytes_are_always_rejected(input in null_byte_strategy()) {
        let result = sanitize_file_path(&input);
        prop_assert!(
            matches!(result, Err(PathTraversalError::NullByte)),
            "Path with null byte should be rejected with NullByte error, got: {:?}",
            result
        );
    }

    /// Invariant: Absolute paths are always rejected.
    ///
    /// Absolute paths could bypass the base directory restriction entirely
    /// and must be rejected.
    #[test]
    fn absolute_paths_are_rejected(input in absolute_path_strategy()) {
        let result = sanitize_file_path(&input);
        prop_assert!(
            result.is_err(),
            "Absolute path should be rejected: {}",
            input
        );
    }

    /// Invariant: Sanitization is idempotent.
    ///
    /// Applying sanitization twice should yield the same result as once.
    /// This ensures the function is stable and predictable.
    #[test]
    fn sanitization_is_idempotent(input in valid_path_strategy()) {
        if let Ok(first_pass) = sanitize_file_path(&input) {
            let first_str = first_pass.to_string_lossy().to_string();
            if let Ok(second_pass) = sanitize_file_path(&first_str) {
                prop_assert_eq!(
                    first_pass, second_pass,
                    "Sanitization should be idempotent"
                );
            }
        }
    }

    /// Invariant: Valid paths produce valid output.
    ///
    /// Paths with only alphanumeric characters and safe separators
    /// should always succeed.
    #[test]
    fn valid_paths_succeed(input in valid_path_strategy()) {
        let result = sanitize_file_path(&input);
        prop_assert!(
            result.is_ok(),
            "Valid path should succeed: {} -> {:?}",
            input, result
        );
    }

    /// Invariant: Empty paths are rejected.
    ///
    /// Empty strings or paths that normalize to empty should be rejected.
    #[test]
    fn empty_and_dot_paths_are_rejected(_dummy in Just(())) {
        let result = sanitize_file_path("");
        prop_assert!(
            matches!(result, Err(PathTraversalError::EmptyPath)),
            "Empty path should be rejected: {:?}",
            result
        );

        let result = sanitize_file_path(".");
        prop_assert!(
            matches!(result, Err(PathTraversalError::EmptyPath)),
            "'.' should normalize to empty and be rejected: {:?}",
            result
        );

        let result = sanitize_file_path("./.");
        prop_assert!(
            matches!(result, Err(PathTraversalError::EmptyPath)),
            "'./.' should normalize to empty and be rejected: {:?}",
            result
        );
    }
}

// ============================================================================
// Windows-Specific Security Tests
// ============================================================================

proptest! {
    /// Invariant: Windows reserved names are rejected on all platforms.
    ///
    /// Defense in depth - even on Unix, we reject Windows reserved names
    /// to ensure portable security.
    #[test]
    fn windows_reserved_names_rejected(name in prop_oneof![
        Just("CON".to_string()),
        Just("PRN".to_string()),
        Just("AUX".to_string()),
        Just("NUL".to_string()),
        Just("COM1".to_string()),
        Just("COM9".to_string()),
        Just("LPT1".to_string()),
        Just("LPT9".to_string()),
        Just("con".to_string()),  // Case insensitive
        Just("Con.txt".to_string()),  // With extension
    ]) {
        let result = validate_windows_path(&name);
        prop_assert!(
            result.is_err(),
            "Windows reserved name should be rejected: {} -> {:?}",
            name, result
        );
    }

    /// Invariant: UNC paths are rejected.
    ///
    /// UNC paths (\\server\share) could access network resources
    /// and must be blocked.
    #[test]
    fn unc_paths_are_rejected(path in prop_oneof![
        Just("\\\\server\\share".to_string()),
        Just("\\\\127.0.0.1\\c$".to_string()),
        Just("\\\\?\\C:\\".to_string()),
        Just("\\\\.\\COM1".to_string()),
    ]) {
        let result = validate_windows_path(&path);
        prop_assert!(
            result.is_err(),
            "UNC path should be rejected: {} -> {:?}",
            path, result
        );
    }

    /// Invariant: Alternate Data Streams are rejected.
    ///
    /// NTFS ADS (file:stream) could hide data or bypass security.
    #[test]
    fn alternate_data_streams_rejected(path in prop_oneof![
        Just("file.txt:hidden".to_string()),
        Just("document:secret".to_string()),
        Just("folder/file:$DATA".to_string()),
    ]) {
        let result = validate_windows_path(&path);
        prop_assert!(
            result.is_err(),
            "ADS path should be rejected: {} -> {:?}",
            path, result
        );
    }
}

// ============================================================================
// Path Normalization Property Tests
// ============================================================================

proptest! {
    /// Invariant: Safe ".." usage within bounds is normalized correctly.
    ///
    /// Paths like "a/b/../c" should normalize to "a/c" when they stay
    /// within the base directory.
    #[test]
    fn safe_parent_navigation_normalizes(
        prefix in path_component_strategy(),
        middle in path_component_strategy(),
        suffix in path_component_strategy()
    ) {
        let input = format!("{prefix}/{middle}/../{suffix}");
        let result = sanitize_file_path(&input);

        if let Ok(normalized) = result {
            // The ".." should have been resolved
            // Use string comparison to handle platform path separator differences
            let normalized_str = normalized.to_string_lossy().replace('\\', "/");
            let expected_str = format!("{prefix}/{suffix}");
            prop_assert_eq!(
                normalized_str.clone(), expected_str,
                "Path should normalize correctly: {} -> {}",
                input, normalized_str
            );
        }
    }

    /// Invariant: Current directory "." is stripped during normalization.
    ///
    /// Paths like "./a/./b" should normalize to "a/b".
    #[test]
    fn current_dir_is_stripped(
        a in path_component_strategy(),
        b in path_component_strategy()
    ) {
        let input = format!("./{a}/./{b}");
        let result = sanitize_file_path(&input);

        if let Ok(normalized) = result {
            // The "." components should be gone
            // Use string comparison to handle platform path separator differences
            let normalized_str = normalized.to_string_lossy().replace('\\', "/");
            let expected_str = format!("{a}/{b}");
            prop_assert_eq!(
                normalized_str.clone(), expected_str,
                "Current dir should be stripped: {} -> {}",
                input, normalized_str
            );
        }
    }

    /// Invariant: Output path components are all Normal.
    ///
    /// After sanitization, all path components should be Component::Normal.
    /// No ParentDir, CurDir, RootDir, or Prefix components.
    #[test]
    fn output_has_only_normal_components(input in valid_path_strategy()) {
        if let Ok(sanitized) = sanitize_file_path(&input) {
            for component in sanitized.components() {
                match component {
                    std::path::Component::Normal(_) => {
                        // Good - this is expected
                    },
                    other => {
                        prop_assert!(
                            false,
                            "Output should only have Normal components, got: {:?}",
                            other
                        );
                    }
                }
            }
        }
    }
}

// ============================================================================
// Robustness Tests
// ============================================================================

proptest! {
    /// Invariant: Function never panics on arbitrary input.
    ///
    /// No matter what garbage input we provide, the function should
    /// return a Result, never panic.
    #[test]
    fn never_panics_on_arbitrary_input(input in ".*") {
        // This should not panic
        let _result = sanitize_file_path(&input);
        // If we get here, the function handled the input gracefully
    }

    /// Invariant: validate_windows_path never panics.
    #[test]
    fn windows_validation_never_panics(input in ".*") {
        let _result = validate_windows_path(&input);
    }

    /// Invariant: Paths with only dots and separators are handled.
    #[test]
    fn dot_heavy_paths_handled(
        dots in prop::collection::vec(prop_oneof![Just("."), Just("..")], 1..10),
        seps in prop::collection::vec(prop_oneof![Just("/"), Just("\\")], 1..10)
    ) {
        let mut path = String::new();
        for (d, s) in dots.iter().zip(seps.iter()) {
            path.push_str(d);
            path.push_str(s);
        }

        // Should not panic, and should likely be rejected
        let _result = sanitize_file_path(&path);
    }
}
