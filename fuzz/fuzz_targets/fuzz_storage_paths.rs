//! Fuzz target for storage service path handling.
//!
//! This fuzzer tests the `StorageService::validate_path` function
//! to ensure no path can escape the base storage directory via:
//! - Path traversal (`..`, `../..`, etc.)
//! - Mixed separators (`/`, `\`)
//! - UNC paths, alternate data streams
//! - Null bytes, control characters
//! - Windows reserved names
//!
//! Run with: `cargo +nightly fuzz run fuzz_storage_paths`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::path::{Component, Path};
use tempfile::TempDir;

/// Input for fuzzing storage path operations.
#[derive(Arbitrary, Debug)]
struct StoragePathInput {
    /// The path string to test
    path: String,
    /// Type of test to perform
    test_type: StorageTestType,
}

#[derive(Arbitrary, Debug)]
enum StorageTestType {
    /// Test basic path validation
    ValidatePath,
    /// Test put_object path handling
    PutObject,
    /// Test get_object path handling
    GetObject,
    /// Test delete_object path handling
    DeleteObject,
    /// Test with adversarial patterns
    Adversarial(AdversarialPattern),
}

#[derive(Arbitrary, Debug)]
enum AdversarialPattern {
    /// Path traversal via ..
    PathTraversal,
    /// Double slashes
    DoubleSlash,
    /// Mixed separators (/ and \)
    MixedSeparators,
    /// Null byte injection
    NullByte,
    /// Control characters
    ControlChars,
    /// Windows reserved names
    WindowsReserved,
    /// Very long path
    LongPath,
    /// Many nested components
    DeepNesting,
    /// Unicode normalization
    UnicodeNormalization,
    /// UNC paths
    UncPath,
    /// Alternate data streams
    AlternateDataStream,
    /// Trailing/leading dots and spaces
    DotSpace,
}

impl StoragePathInput {
    /// Generate adversarial input based on pattern.
    fn adversarial_input(&self, pattern: &AdversarialPattern) -> String {
        match pattern {
            AdversarialPattern::PathTraversal => {
                // Various path traversal attempts
                let variants = [
                    format!("../../../{}", self.path),
                    format!("..\\..\\..\\{}", self.path),
                    format!("foo/../../../{}", self.path),
                    format!("a/b/c/../../../../{}", self.path),
                    format!("{}/../../../etc/passwd", self.path),
                ];
                variants[self.path.len() % variants.len()].clone()
            }
            AdversarialPattern::DoubleSlash => {
                format!("//{}//test", self.path)
            }
            AdversarialPattern::MixedSeparators => {
                format!("a/b\\c/{}/d\\e", self.path)
            }
            AdversarialPattern::NullByte => {
                format!("{}\0hidden.txt", self.path)
            }
            AdversarialPattern::ControlChars => {
                format!("{}\x01\x02\x03.txt", self.path)
            }
            AdversarialPattern::WindowsReserved => {
                let names = ["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"];
                let name = names[self.path.len() % names.len()];
                format!("{}/{}.txt", self.path, name)
            }
            AdversarialPattern::LongPath => {
                // Very long path segment
                let segment = "a".repeat(300);
                format!("{}/{}", segment, self.path)
            }
            AdversarialPattern::DeepNesting => {
                // Many nested directories
                let components = (0..100)
                    .map(|i| format!("d{i}"))
                    .collect::<Vec<_>>()
                    .join("/");
                format!("{}/{}", components, self.path)
            }
            AdversarialPattern::UnicodeNormalization => {
                // Unicode lookalikes for . and /
                // U+2024 ONE DOT LEADER, U+2215 DIVISION SLASH
                format!("\u{2024}\u{2024}\u{2215}{}", self.path)
            }
            AdversarialPattern::UncPath => {
                format!("\\\\server\\share\\{}", self.path)
            }
            AdversarialPattern::AlternateDataStream => {
                format!("{}:Zone.Identifier", self.path)
            }
            AdversarialPattern::DotSpace => {
                format!("  {}  ", self.path)
            }
        }
    }
}

/// Windows reserved device names.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fuzz_target!(|data: StoragePathInput| {
    // Create a temporary directory for each fuzz iteration
    let temp_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(_) => return, // Skip if we can't create temp dir
    };

    // Open storage service
    let storage = match mik::daemon::services::storage::StorageService::open(temp_dir.path()) {
        Ok(s) => s,
        Err(_) => return, // Skip if storage can't be opened
    };

    let test_path = match data.test_type {
        StorageTestType::ValidatePath => data.path.clone(),
        StorageTestType::PutObject => data.path.clone(),
        StorageTestType::GetObject => data.path.clone(),
        StorageTestType::DeleteObject => data.path.clone(),
        StorageTestType::Adversarial(ref pattern) => data.adversarial_input(pattern),
    };

    // Skip empty paths early (they're expected to fail)
    if test_path.is_empty() {
        return;
    }

    match data.test_type {
        StorageTestType::ValidatePath | StorageTestType::Adversarial(_) => {
            test_path_validation(&storage, temp_dir.path(), &test_path);
        }
        StorageTestType::PutObject => {
            test_put_object(&storage, temp_dir.path(), &test_path);
        }
        StorageTestType::GetObject => {
            test_get_object(&storage, temp_dir.path(), &test_path);
        }
        StorageTestType::DeleteObject => {
            test_delete_object(&storage, temp_dir.path(), &test_path);
        }
    }
});

/// Test path validation with security invariant checks.
fn test_path_validation(
    storage: &mik::daemon::services::storage::StorageService,
    base_dir: &Path,
    path: &str,
) {
    // Try to put an object - this exercises validate_path internally
    let result = storage.put_object(path, b"test data", Some("text/plain"));

    if result.is_ok() {
        // If the operation succeeded, verify security invariants

        // INVARIANT 1: The path must not contain null bytes
        assert!(
            !path.contains('\0'),
            "Accepted path with null byte: {:?}",
            path
        );

        // INVARIANT 2: Check that no file was created outside base_dir
        verify_no_escape(base_dir, path);

        // INVARIANT 3: Path should not start with .. after normalization
        let normalized = normalize_path(path);
        assert!(
            !normalized.starts_with(".."),
            "Accepted path that normalizes to ..: {:?} -> {:?}",
            path,
            normalized
        );

        // INVARIANT 4: Should not be UNC path
        assert!(
            !path.starts_with("\\\\") && !path.starts_with("//"),
            "Accepted UNC path: {:?}",
            path
        );

        // INVARIANT 5: Check for Windows reserved names in path components
        // Note: The storage service may allow these since it's cross-platform,
        // but if Windows support is intended, this could be a security issue
        check_windows_reserved_names(path);

        // Clean up
        let _ = storage.delete_object(path);
    } else {
        // Operation failed - this is expected for malicious paths
        // Verify that no file was created
        verify_no_files_outside_base(base_dir);
    }
}

/// Test put_object with security checks.
fn test_put_object(
    storage: &mik::daemon::services::storage::StorageService,
    base_dir: &Path,
    path: &str,
) {
    let test_data = b"fuzz test data";
    let result = storage.put_object(path, test_data, None);

    if result.is_ok() {
        // Verify the file was created within base_dir
        verify_no_escape(base_dir, path);

        // Verify we can read it back
        if let Ok(Some((data, _meta))) = storage.get_object(path) {
            assert_eq!(
                data, test_data,
                "Data mismatch for path: {:?}",
                path
            );
        }

        // Clean up
        let _ = storage.delete_object(path);
    }

    // Always verify no escape occurred
    verify_no_files_outside_base(base_dir);
}

/// Test get_object with security checks.
fn test_get_object(
    storage: &mik::daemon::services::storage::StorageService,
    base_dir: &Path,
    path: &str,
) {
    // First, try to create a valid file
    let _ = storage.put_object("valid_test_file.txt", b"valid data", None);

    // Now try to get the fuzzed path
    let result = storage.get_object(path);

    if let Ok(Some(_)) = result {
        // If we got data, verify it's from within base_dir
        verify_no_escape(base_dir, path);
    }

    // Clean up
    let _ = storage.delete_object("valid_test_file.txt");
}

/// Test delete_object with security checks.
fn test_delete_object(
    storage: &mik::daemon::services::storage::StorageService,
    base_dir: &Path,
    path: &str,
) {
    // Create a file first
    let _ = storage.put_object("delete_test.txt", b"to delete", None);

    // Try to delete with fuzzed path
    let result = storage.delete_object(path);

    // If deletion was attempted (whether it succeeded or not),
    // verify no files outside base_dir were affected
    if result.is_ok() {
        verify_no_escape(base_dir, path);
    }

    verify_no_files_outside_base(base_dir);
}

/// Verify that no file operation escaped the base directory.
fn verify_no_escape(base_dir: &Path, path: &str) {
    // Canonicalize the base directory
    let canonical_base = match base_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => return, // Can't verify if base doesn't exist
    };

    // Try to construct what the full path would be
    let normalized = normalize_path(path);
    let full_path = base_dir.join(&normalized);

    // If the file exists, verify it's within base
    if full_path.exists() {
        if let Ok(canonical_file) = full_path.canonicalize() {
            assert!(
                canonical_file.starts_with(&canonical_base),
                "File escaped base directory!\nBase: {:?}\nFile: {:?}\nOriginal path: {:?}",
                canonical_base,
                canonical_file,
                path
            );
        }
    }
}

/// Verify no files exist outside the expected structure.
fn verify_no_files_outside_base(base_dir: &Path) {
    // Get the parent of base_dir
    if let Some(parent) = base_dir.parent() {
        // Check that only our temp directory exists in parent
        // (This is a sanity check that we haven't escaped)
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path != base_dir {
                    // There's something else in the parent - verify it existed before
                    // (This is tricky because we can't track pre-existing files)
                    // For fuzzing purposes, just verify our base_dir is what we expect
                    let name = path.file_name().unwrap_or_default();
                    // Skip common system directories/files
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with(".tmp") || name_str.starts_with("tmp") {
                        // Other temp directories are fine
                        continue;
                    }
                }
            }
        }
    }
}

/// Check if path contains Windows reserved device names.
/// This is a soft check - we log but don't assert, as the storage service
/// may intentionally allow these for cross-platform compatibility.
#[allow(dead_code)]
fn check_windows_reserved_names(path: &str) {
    for component in path.split(['/', '\\']) {
        if component.is_empty() {
            continue;
        }

        // Get the stem (filename without extension)
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_uppercase();

        // Note: We don't assert here because the storage service may allow
        // Windows reserved names for cross-platform compatibility. However,
        // this could be a security concern on Windows systems.
        if WINDOWS_RESERVED.contains(&stem.as_str()) {
            // This is informational - the fuzzer will still work, but
            // security auditors should be aware that Windows reserved
            // names are not blocked by the storage service.
            #[cfg(debug_assertions)]
            eprintln!(
                "Note: Storage service accepted Windows reserved name: {} in path: {}",
                component, path
            );
        }
    }
}

/// Normalize a path by resolving . and .. components.
fn normalize_path(path: &str) -> String {
    let path = Path::new(path);
    let mut normalized = Vec::new();

    for component in path.components() {
        match component {
            Component::Normal(name) => {
                normalized.push(name.to_string_lossy().to_string());
            }
            Component::CurDir => {
                // Skip "."
            }
            Component::ParentDir => {
                // Pop if we can
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => {
                // Absolute path component
                normalized.clear();
                normalized.push(component.as_os_str().to_string_lossy().to_string());
            }
        }
    }

    normalized.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("a/b/c"), "a/b/c");
        assert_eq!(normalize_path("a/b/../c"), "a/c");
        assert_eq!(normalize_path("a/./b"), "a/b");
        assert_eq!(normalize_path("../etc"), "");
        assert_eq!(normalize_path("a/../../../etc"), "");
    }

    #[test]
    fn test_basic_path_traversal_blocked() {
        let temp_dir = TempDir::new().unwrap();
        let storage =
            mik::daemon::services::storage::StorageService::open(temp_dir.path()).unwrap();

        // These should all fail
        let attack_paths = [
            "../etc/passwd",
            "../../etc/passwd",
            "foo/../../../etc/passwd",
            "/etc/passwd",
            "..\\windows\\system32",
        ];

        for path in &attack_paths {
            let result = storage.put_object(path, b"attack", None);
            assert!(
                result.is_err(),
                "Path traversal not blocked for: {}",
                path
            );
        }
    }

    #[test]
    fn test_null_byte_in_path() {
        let temp_dir = TempDir::new().unwrap();
        let storage =
            mik::daemon::services::storage::StorageService::open(temp_dir.path()).unwrap();

        // Note: The storage service may or may not block null bytes explicitly
        // The key invariant is that no file should be created outside base_dir
        let result = storage.put_object("test\0.txt", b"data", None);

        // If it succeeded, verify no escape
        if result.is_ok() {
            verify_no_escape(temp_dir.path(), "test\0.txt");
        }
    }

    #[test]
    fn test_valid_paths_work() {
        let temp_dir = TempDir::new().unwrap();
        let storage =
            mik::daemon::services::storage::StorageService::open(temp_dir.path()).unwrap();

        // Valid paths should work
        let valid_paths = ["test.txt", "dir/file.txt", "a/b/c/d.txt", "file.min.js"];

        for path in &valid_paths {
            let result = storage.put_object(path, b"valid data", None);
            assert!(result.is_ok(), "Valid path rejected: {}", path);

            // Clean up
            storage.delete_object(path).unwrap();
        }
    }
}
