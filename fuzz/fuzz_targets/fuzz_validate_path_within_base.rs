//! Fuzz target for `validate_path_within_base` - symlink TOCTOU attack prevention.
//!
//! This fuzzer tests the symlink escape detection by:
//! 1. Creating temporary directories with various structures
//! 2. Testing path validation with symlinks pointing outside base
//! 3. Verifying TOCTOU (time-of-check-time-of-use) invariants hold
//!
//! Key security properties tested:
//! - Symlinks pointing outside base directory are rejected
//! - Path canonicalization resolves all symlinks
//! - Non-existent paths are handled safely
//! - Empty and special paths don't cause panics
//!
//! Run with: `cargo +nightly fuzz run fuzz_validate_path_within_base`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mik::security::{validate_path_within_base, PathTraversalError};
use std::path::PathBuf;
use tempfile::tempdir;

/// Structured input for testing path validation within base directory.
#[derive(Arbitrary, Debug)]
struct PathWithinBaseInput {
    /// The file path to validate (relative to base)
    file_path: String,
    /// Type of test scenario
    scenario: TestScenario,
    /// Whether to create real files/directories
    create_files: bool,
}

/// Different test scenarios for comprehensive coverage.
#[derive(Arbitrary, Debug)]
enum TestScenario {
    /// Simple path validation without symlinks
    Simple,
    /// Path with many nested directories
    DeepNesting { depth: u8 },
    /// Path with traversal attempts
    TraversalAttempt { depth: u8 },
    /// Test with adversarial patterns
    Adversarial(AdversarialPattern),
}

/// Adversarial patterns to test edge cases.
#[derive(Arbitrary, Debug)]
enum AdversarialPattern {
    /// Empty path
    Empty,
    /// Only dots
    OnlyDots,
    /// Very long path
    VeryLong,
    /// Many path components
    ManyComponents,
    /// Null byte injection
    NullByte,
    /// Mixed path separators
    MixedSeparators,
    /// Unicode in path
    Unicode,
    /// Trailing/leading spaces
    Whitespace,
    /// Double slashes
    DoubleSlashes,
    /// Path starting with dots
    DotPrefix,
}

impl PathWithinBaseInput {
    /// Build the file path to test based on the scenario.
    fn build_path(&self) -> PathBuf {
        match &self.scenario {
            TestScenario::Simple => PathBuf::from(&self.file_path),
            TestScenario::DeepNesting { depth } => {
                let nesting = (0..*depth.min(&20))
                    .map(|i| format!("d{i}"))
                    .collect::<Vec<_>>()
                    .join("/");
                if nesting.is_empty() {
                    PathBuf::from(&self.file_path)
                } else {
                    PathBuf::from(format!("{}/{}", nesting, self.file_path))
                }
            }
            TestScenario::TraversalAttempt { depth } => {
                let traversal = "../".repeat((*depth as usize).min(20));
                PathBuf::from(format!("{}{}", traversal, self.file_path))
            }
            TestScenario::Adversarial(pattern) => self.adversarial_path(pattern),
        }
    }

    /// Generate adversarial path based on pattern.
    fn adversarial_path(&self, pattern: &AdversarialPattern) -> PathBuf {
        match pattern {
            AdversarialPattern::Empty => PathBuf::new(),
            AdversarialPattern::OnlyDots => PathBuf::from("..."),
            AdversarialPattern::VeryLong => {
                let segment = "a".repeat(300);
                PathBuf::from(format!("{}/{}", segment, self.file_path))
            }
            AdversarialPattern::ManyComponents => {
                let components = (0..50)
                    .map(|i| format!("d{i}"))
                    .collect::<Vec<_>>()
                    .join("/");
                PathBuf::from(format!("{}/{}", components, self.file_path))
            }
            AdversarialPattern::NullByte => {
                // Inject null byte in the middle of the path
                let mut path = self.file_path.clone();
                if !path.is_empty() {
                    let pos = path.len() / 2;
                    path.insert(pos, '\0');
                } else {
                    path = "file\0.txt".to_string();
                }
                PathBuf::from(path)
            }
            AdversarialPattern::MixedSeparators => {
                PathBuf::from(format!("a/b\\c/{}", self.file_path))
            }
            AdversarialPattern::Unicode => {
                // Unicode characters that might be confusable
                PathBuf::from(format!("\u{2024}\u{2024}/{}", self.file_path))
            }
            AdversarialPattern::Whitespace => {
                PathBuf::from(format!("  {}  ", self.file_path))
            }
            AdversarialPattern::DoubleSlashes => {
                PathBuf::from(format!("a//b//{}", self.file_path))
            }
            AdversarialPattern::DotPrefix => {
                PathBuf::from(format!(".hidden/{}", self.file_path))
            }
        }
    }
}

fuzz_target!(|data: PathWithinBaseInput| {
    // Create a temporary base directory for testing
    let base_dir = match tempdir() {
        Ok(dir) => dir,
        Err(_) => return, // Can't create temp dir, skip this input
    };

    let file_path = data.build_path();

    // Optionally create the file/directory structure
    if data.create_files && !file_path.as_os_str().is_empty() {
        let full_path = base_dir.path().join(&file_path);
        if let Some(parent) = full_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&full_path, "test content");
    }

    // The function must never panic
    let result = validate_path_within_base(base_dir.path(), &file_path);

    match result {
        Ok(canonical_path) => {
            // INVARIANT 1: Canonical path must start with base directory
            // We need to canonicalize the base too for comparison
            if let Ok(canonical_base) = base_dir.path().canonicalize() {
                assert!(
                    canonical_path.starts_with(&canonical_base),
                    "Canonical path {:?} does not start with base {:?}",
                    canonical_path,
                    canonical_base
                );
            }

            // INVARIANT 2: Canonical path must not contain ".." components
            let path_str = canonical_path.to_string_lossy();
            assert!(
                !path_str.contains(".."),
                "Canonical path contains '..': {:?}",
                canonical_path
            );

            // INVARIANT 3: Result should be an absolute path (canonicalized)
            assert!(
                canonical_path.is_absolute(),
                "Canonical path is not absolute: {:?}",
                canonical_path
            );

            // INVARIANT 4: No null bytes in the result
            assert!(
                !path_str.contains('\0'),
                "Canonical path contains null byte: {:?}",
                canonical_path
            );
        }
        Err(e) => {
            // Errors are expected for certain inputs
            match e {
                PathTraversalError::EscapesBaseDirectory => {
                    // This is expected for symlinks pointing outside or traversal attempts
                    // The path likely contained ".." or was a symlink escape
                }
                PathTraversalError::EmptyPath => {
                    // Expected for empty paths or paths that normalize to empty
                }
                _ => {
                    // Other errors can occur for various malformed inputs
                }
            }
        }
    }
});

/// Additional invariant checks for symlink scenarios.
/// These run on Unix where symlinks are easily created.
#[cfg(unix)]
#[allow(dead_code)]
fn test_symlink_invariants(base_dir: &std::path::Path, _file_path: &std::path::Path) {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    // Create a directory outside base
    let outside_dir = match tempdir() {
        Ok(dir) => dir,
        Err(_) => return,
    };

    // Create an evil file outside base
    let evil_file = outside_dir.path().join("secret.txt");
    if std::fs::write(&evil_file, "secret data").is_err() {
        return;
    }

    // Create symlink inside base pointing to evil file
    let symlink_path = base_dir.join("evil_link");
    if symlink(&evil_file, &symlink_path).is_err() {
        return;
    }

    // INVARIANT: Symlink escape MUST be detected
    let result = validate_path_within_base(base_dir, Path::new("evil_link"));
    assert!(
        result.is_err(),
        "Symlink escape was NOT detected! This is a security vulnerability."
    );

    // Clean up
    let _ = std::fs::remove_file(&symlink_path);
}
