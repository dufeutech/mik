//! Path sanitization and validation utilities.
//!
//! This module provides functions for validating and sanitizing file paths
//! to prevent directory traversal attacks, null byte injection, and
//! Windows-specific path exploits.
//!
//! # Module Structure
//!
//! - [`windows`] - Windows-specific validation (reserved names, UNC, ADS)
//! - [`base`] - Base directory validation with symlink protection

mod base;
mod windows;

use std::path::{Component, Path, PathBuf};
use tracing::warn;

use super::error::PathTraversalError;

// Re-export submodule items
pub use base::validate_path_within_base;
pub use windows::{WINDOWS_RESERVED_NAMES, validate_windows_path};

/// Sanitize a file path to prevent directory traversal attacks.
///
/// This function:
/// 1. Normalizes the path by resolving `.` and `..` components
/// 2. Rejects paths that try to escape the base directory
/// 3. Rejects absolute paths
/// 4. Rejects paths with null bytes
///
/// # Errors
///
/// Returns [`PathTraversalError`] if the path is unsafe:
/// - [`NullByte`](PathTraversalError::NullByte) - Path contains null bytes
/// - [`EmptyPath`](PathTraversalError::EmptyPath) - Path is empty
/// - [`AbsolutePath`](PathTraversalError::AbsolutePath) - Path is absolute
/// - [`EscapesBaseDirectory`](PathTraversalError::EscapesBaseDirectory) - Path escapes base via `..`
/// - [`ReservedWindowsName`](PathTraversalError::ReservedWindowsName) - Uses Windows reserved name
/// - [`UncPath`](PathTraversalError::UncPath) - Uses UNC path format
/// - [`AlternateDataStream`](PathTraversalError::AlternateDataStream) - Uses NTFS ADS
///
/// # Examples
///
/// ```
/// use mik::security::sanitize_file_path;
///
/// // Valid paths
/// assert!(sanitize_file_path("project/index.html").is_ok());
/// assert!(sanitize_file_path("assets/style.css").is_ok());
///
/// // Invalid paths (directory traversal)
/// assert!(sanitize_file_path("../etc/passwd").is_err());
/// assert!(sanitize_file_path("project/../../etc/passwd").is_err());
/// assert!(sanitize_file_path("/etc/passwd").is_err());
/// ```
pub fn sanitize_file_path(path: &str) -> Result<PathBuf, PathTraversalError> {
    // Check for null bytes
    if path.contains('\0') {
        warn!(
            security_event = "path_traversal_attempt",
            path = %path.replace('\0', "\\0"),
            reason = "null_byte",
            "Blocked path with null byte"
        );
        return Err(PathTraversalError::NullByte);
    }

    // Check for empty path
    if path.is_empty() {
        return Err(PathTraversalError::EmptyPath);
    }

    // Validate Windows-specific security issues (run on all platforms for defense in depth)
    validate_windows_path(path)?;

    let path_obj = Path::new(path);

    // Reject absolute paths
    if path_obj.is_absolute() {
        warn!(
            security_event = "path_traversal_attempt",
            path = %path,
            reason = "absolute_path",
            "Blocked absolute path"
        );
        return Err(PathTraversalError::AbsolutePath);
    }

    // Normalize the path and check for directory traversal
    let mut normalized = PathBuf::new();
    for component in path_obj.components() {
        match component {
            Component::Normal(part) => {
                // Check for null bytes in path components
                if part.to_string_lossy().contains('\0') {
                    warn!(
                        security_event = "path_traversal_attempt",
                        path = %path,
                        reason = "null_byte_in_component",
                        "Blocked path with null byte in component"
                    );
                    return Err(PathTraversalError::NullByte);
                }
                normalized.push(part);
            },
            Component::CurDir => {
                // Skip "." - it's safe but unnecessary
            },
            Component::ParentDir => {
                // ".." is only allowed if we can pop from normalized path
                // If normalized is empty, this would escape the base directory
                if !normalized.pop() {
                    warn!(
                        security_event = "path_traversal_attempt",
                        path = %path,
                        reason = "directory_escape",
                        "Blocked path escaping base directory"
                    );
                    return Err(PathTraversalError::EscapesBaseDirectory);
                }
            },
            Component::RootDir | Component::Prefix(_) => {
                // Absolute paths and Windows prefixes are not allowed
                warn!(
                    security_event = "path_traversal_attempt",
                    path = %path,
                    reason = "absolute_path",
                    "Blocked absolute path"
                );
                return Err(PathTraversalError::AbsolutePath);
            },
        }
    }

    // Ensure the normalized path is not empty
    if normalized.as_os_str().is_empty() {
        return Err(PathTraversalError::EmptyPath);
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // BASIC SANITIZATION TESTS
    // =========================================================================

    #[test]
    fn test_sanitize_file_path_valid() {
        assert!(sanitize_file_path("index.html").is_ok());
        assert!(sanitize_file_path("project/index.html").is_ok());
        assert!(sanitize_file_path("assets/css/style.css").is_ok());
        assert!(sanitize_file_path("./file.txt").is_ok());
    }

    #[test]
    fn test_sanitize_file_path_traversal() {
        assert_eq!(
            sanitize_file_path("../etc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("project/../../etc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("../../secrets"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
    }

    #[test]
    fn test_sanitize_file_path_absolute() {
        assert_eq!(
            sanitize_file_path("/etc/passwd"),
            Err(PathTraversalError::AbsolutePath)
        );
        #[cfg(windows)]
        assert_eq!(
            sanitize_file_path("C:\\Windows\\System32"),
            Err(PathTraversalError::AbsolutePath)
        );
    }

    #[test]
    fn test_sanitize_file_path_null_byte() {
        assert_eq!(
            sanitize_file_path("file\0.txt"),
            Err(PathTraversalError::NullByte)
        );
    }

    #[test]
    fn test_sanitize_file_path_empty() {
        assert_eq!(sanitize_file_path(""), Err(PathTraversalError::EmptyPath));
    }

    #[test]
    fn test_sanitize_file_path_normalization() {
        // "a/b/../c" -> "a/c"
        let result = sanitize_file_path("a/b/../c").unwrap();
        assert_eq!(result, PathBuf::from("a/c"));

        // "a/./b" -> "a/b"
        let result = sanitize_file_path("a/./b").unwrap();
        assert_eq!(result, PathBuf::from("a/b"));
    }

    // =========================================================================
    // PATH TRAVERSAL EDGE CASES
    // =========================================================================

    #[test]
    fn test_path_traversal_basic() {
        assert_eq!(
            sanitize_file_path("../etc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("foo/../../../etc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path(".."),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("../.."),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        // Deeply nested traversal
        assert_eq!(
            sanitize_file_path("a/b/c/../../../../etc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
    }

    #[test]
    fn test_path_traversal_double_encoded() {
        // URL-encoded variants - these are literal strings, not decoded
        // The function receives the raw string, URL decoding happens at HTTP layer
        // But we test what happens if encoded chars slip through

        // These should be treated as literal filenames (weird but not traversal)
        // because the function doesn't do URL decoding
        let result = sanitize_file_path("%2e%2e/etc/passwd");
        // This is a valid path component since %2e is just a literal filename char
        assert!(result.is_ok());

        // Mixed encoding: `..%2Fetc/passwd` has components `..%2Fetc` + `passwd`
        // The `..%2Fetc` is a Normal component (weird filename), not ParentDir
        // because `%2F` is a literal string, not a path separator
        let result = sanitize_file_path("..%2Fetc/passwd");
        assert!(result.is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn test_path_traversal_backslash_variants() {
        // Windows-style path traversal with backslashes
        // Note: On Unix, backslashes are literal filename characters, not path separators
        assert_eq!(
            sanitize_file_path("..\\etc\\passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("..\\..\\windows\\system32"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("foo\\..\\..\\..\\etc"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_path_traversal_mixed_separators() {
        // Mix of forward and back slashes
        // Note: On Unix, backslashes are literal filename characters, not path separators
        assert_eq!(
            sanitize_file_path("..\\../etc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
        assert_eq!(
            sanitize_file_path("foo/..\\..\\etc"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );
    }

    // =========================================================================
    // NULL BYTE INJECTION TESTS
    // =========================================================================

    #[test]
    fn test_null_byte_injection() {
        // Null byte at various positions
        assert_eq!(
            sanitize_file_path("valid.txt\0.exe"),
            Err(PathTraversalError::NullByte)
        );
        assert_eq!(
            sanitize_file_path("module\0evil"),
            Err(PathTraversalError::NullByte)
        );
        assert_eq!(
            sanitize_file_path("\0hidden"),
            Err(PathTraversalError::NullByte)
        );
        assert_eq!(
            sanitize_file_path("path/to/\0file"),
            Err(PathTraversalError::NullByte)
        );
        // Null byte at the end
        assert_eq!(
            sanitize_file_path("file.txt\0"),
            Err(PathTraversalError::NullByte)
        );
    }

    // =========================================================================
    // VALID INPUT TESTS
    // =========================================================================

    #[test]
    fn test_valid_paths() {
        // Valid nested paths
        let result = sanitize_file_path("assets/style.css");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("assets/style.css"));

        let result = sanitize_file_path("deep/nested/path/file.js");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("deep/nested/path/file.js"));

        // Valid path with safe .. usage (within bounds)
        let result = sanitize_file_path("a/b/../c/d");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("a/c/d"));
    }

    #[test]
    fn test_valid_simple_filenames() {
        // Simple filenames
        assert_eq!(
            sanitize_file_path("file.txt").unwrap(),
            PathBuf::from("file.txt")
        );
        assert_eq!(
            sanitize_file_path("image.png").unwrap(),
            PathBuf::from("image.png")
        );
        assert_eq!(
            sanitize_file_path("data.json").unwrap(),
            PathBuf::from("data.json")
        );
        assert_eq!(
            sanitize_file_path("noextension").unwrap(),
            PathBuf::from("noextension")
        );
        assert_eq!(
            sanitize_file_path(".hidden").unwrap(),
            PathBuf::from(".hidden")
        );
    }

    #[test]
    fn test_valid_paths_with_dots_in_name() {
        // Files with multiple dots (not traversal)
        assert_eq!(
            sanitize_file_path("file.min.js").unwrap(),
            PathBuf::from("file.min.js")
        );
        assert_eq!(
            sanitize_file_path("archive.tar.gz").unwrap(),
            PathBuf::from("archive.tar.gz")
        );
        assert_eq!(
            sanitize_file_path("version.1.2.3.txt").unwrap(),
            PathBuf::from("version.1.2.3.txt")
        );
    }

    // =========================================================================
    // PATH NORMALIZATION EDGE CASES
    // =========================================================================

    #[test]
    fn test_path_normalization_safe_usage() {
        // Safe .. that doesn't escape
        assert_eq!(
            sanitize_file_path("a/b/c/../../d").unwrap(),
            PathBuf::from("a/d")
        );

        // Multiple safe .. operations
        assert_eq!(
            sanitize_file_path("a/b/c/../d/../e").unwrap(),
            PathBuf::from("a/b/e")
        );

        // Current dir (.) is stripped
        assert_eq!(
            sanitize_file_path("./a/./b/./c").unwrap(),
            PathBuf::from("a/b/c")
        );

        // Mix of . and safe ..
        assert_eq!(
            sanitize_file_path("./a/b/../c/.").unwrap(),
            PathBuf::from("a/c")
        );
    }

    #[test]
    fn test_path_only_current_dir() {
        // Just "." should result in empty path error after normalization
        assert_eq!(sanitize_file_path("."), Err(PathTraversalError::EmptyPath));
        assert_eq!(
            sanitize_file_path("././."),
            Err(PathTraversalError::EmptyPath)
        );
    }

    #[test]
    fn test_absolute_paths_various_formats() {
        // Unix absolute paths
        assert_eq!(
            sanitize_file_path("/etc/passwd"),
            Err(PathTraversalError::AbsolutePath)
        );
        assert_eq!(
            sanitize_file_path("/"),
            Err(PathTraversalError::AbsolutePath)
        );
        assert_eq!(
            sanitize_file_path("/home/user/file"),
            Err(PathTraversalError::AbsolutePath)
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_absolute_paths() {
        // Windows drive letters
        assert_eq!(
            sanitize_file_path("C:\\Windows\\System32"),
            Err(PathTraversalError::AbsolutePath)
        );
        assert_eq!(
            sanitize_file_path("D:/data/file.txt"),
            Err(PathTraversalError::AbsolutePath)
        );
        assert_eq!(
            sanitize_file_path("C:file.txt"),
            Err(PathTraversalError::AbsolutePath)
        );
    }
}
