//! Security utilities for input sanitization and path traversal prevention.

use std::path::{Component, Path, PathBuf};
use tracing::warn;

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
/// - [`WindowsReservedName`](PathTraversalError::WindowsReservedName) - Uses Windows reserved name
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

/// Sanitize a module name to prevent directory traversal and injection attacks.
///
/// Valid module names:
/// - Must not be empty
/// - Must not contain path separators (`/`, `\`)
/// - Must not contain null bytes
/// - Must not be `.` or `..`
/// - Should be a valid filename
///
/// # Errors
///
/// Returns [`ModuleNameError`] if the name is invalid:
/// - [`EmptyName`](ModuleNameError::EmptyName) - Name is empty
/// - [`NullByte`](ModuleNameError::NullByte) - Name contains null bytes
/// - [`PathSeparator`](ModuleNameError::PathSeparator) - Name contains `/` or `\`
/// - [`SpecialDirectory`](ModuleNameError::SpecialDirectory) - Name is `.` or `..`
/// - [`TooLong`](ModuleNameError::TooLong) - Name exceeds 255 characters
/// - [`ControlCharacter`](ModuleNameError::ControlCharacter) - Name contains control characters
///
/// # Examples
///
/// ```
/// use mik::security::sanitize_module_name;
///
/// // Valid names
/// assert!(sanitize_module_name("api").is_ok());
/// assert!(sanitize_module_name("user-service").is_ok());
/// assert!(sanitize_module_name("my_module").is_ok());
///
/// // Invalid names
/// assert!(sanitize_module_name("../etc/passwd").is_err());
/// assert!(sanitize_module_name("api/users").is_err());
/// assert!(sanitize_module_name("..").is_err());
/// ```
pub fn sanitize_module_name(name: &str) -> Result<String, ModuleNameError> {
    // Check for empty name
    if name.is_empty() {
        return Err(ModuleNameError::EmptyName);
    }

    // Check for null bytes
    if name.contains('\0') {
        warn!(
            security_event = "module_injection_attempt",
            module = %name.replace('\0', "\\0"),
            reason = "null_byte",
            "Blocked module name with null byte"
        );
        return Err(ModuleNameError::NullByte);
    }

    // Check for path separators (Unix and Windows)
    if name.contains('/') || name.contains('\\') {
        warn!(
            security_event = "module_injection_attempt",
            module = %name,
            reason = "path_separator",
            "Blocked module name with path separator"
        );
        return Err(ModuleNameError::PathSeparator);
    }

    // Reject special directory names
    if name == "." || name == ".." {
        warn!(
            security_event = "module_injection_attempt",
            module = %name,
            reason = "special_directory",
            "Blocked special directory as module name"
        );
        return Err(ModuleNameError::SpecialDirectory);
    }

    // Check length (reasonable limit for filenames)
    if name.len() > 255 {
        warn!(
            security_event = "module_injection_attempt",
            module_len = name.len(),
            reason = "too_long",
            "Blocked excessively long module name"
        );
        return Err(ModuleNameError::TooLong);
    }

    // Check for control characters
    if name.chars().any(char::is_control) {
        warn!(
            security_event = "module_injection_attempt",
            module = %name.chars().filter(|c| !c.is_control()).collect::<String>(),
            reason = "control_character",
            "Blocked module name with control characters"
        );
        return Err(ModuleNameError::ControlCharacter);
    }

    Ok(name.to_string())
}

/// Reserved Windows device names that can cause issues even with extensions.
///
/// Windows treats these as device names regardless of extension:
/// - `CON`, `PRN`, `AUX`, `NUL`
/// - `COM1` through `COM9`
/// - `LPT1` through `LPT9`
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates a path for Windows-specific security issues.
///
/// This function checks for:
/// 1. Reserved Windows device names (CON, PRN, NUL, COM1-9, LPT1-9)
/// 2. UNC paths (\\server\share)
/// 3. Alternate data streams (file.txt:stream)
///
/// This validation is performed on all platforms because:
/// - The server might run on Windows
/// - Files might be accessed from Windows clients
/// - Defense in depth against path confusion attacks
///
/// # Examples
///
/// ```
/// use mik::security::validate_windows_path;
///
/// // Valid paths
/// assert!(validate_windows_path("normal_file.txt").is_ok());
/// assert!(validate_windows_path("my_folder/file.txt").is_ok());
///
/// // Invalid: reserved device names
/// assert!(validate_windows_path("CON").is_err());
/// assert!(validate_windows_path("con.txt").is_err());
/// assert!(validate_windows_path("folder/NUL").is_err());
///
/// // Invalid: UNC paths
/// assert!(validate_windows_path("\\\\server\\share").is_err());
///
/// // Invalid: alternate data streams
/// assert!(validate_windows_path("file.txt:hidden").is_err());
/// ```
pub fn validate_windows_path(path: &str) -> Result<(), PathTraversalError> {
    // Check for UNC paths (\\server\share or //server/share)
    if path.starts_with("\\\\") || path.starts_with("//") {
        warn!(
            security_event = "windows_path_attack",
            path = %path,
            reason = "unc_path",
            "Blocked UNC path"
        );
        return Err(PathTraversalError::UncPath);
    }

    // Check for alternate data streams (colon in path, except for drive letters)
    // Drive letters like C: are handled by the absolute path check elsewhere
    // But we need to catch things like "file.txt:stream"
    if let Some(colon_pos) = path.find(':') {
        // If colon is not at position 1 (drive letter like C:), it's suspicious
        if colon_pos != 1 {
            warn!(
                security_event = "windows_path_attack",
                path = %path,
                reason = "alternate_data_stream",
                "Blocked path with alternate data stream"
            );
            return Err(PathTraversalError::AlternateDataStream);
        }
    }

    // Check each path component for reserved device names
    for component in path.split(['/', '\\']) {
        if component.is_empty() {
            continue;
        }

        // Get the stem (filename without extension)
        // "CON.txt" -> "CON", "NUL" -> "NUL"
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_uppercase();

        if WINDOWS_RESERVED_NAMES.contains(&stem.as_str()) {
            warn!(
                security_event = "windows_path_attack",
                path = %path,
                component = %component,
                reason = "reserved_device_name",
                "Blocked path with Windows reserved device name"
            );
            return Err(PathTraversalError::ReservedWindowsName);
        }
    }

    Ok(())
}

/// Validate that a path stays within a base directory after canonicalization.
///
/// This prevents symlink-based path traversal attacks (TOCTOU).
///
/// # Arguments
/// * `base_dir` - The canonical base directory
/// * `file_path` - The path to validate (relative to `base_dir`)
///
/// # Returns
/// * `Ok(canonical_path)` - The canonicalized full path if it's within `base_dir`
/// * `Err(PathTraversalError::EscapesBaseDirectory)` - If the path escapes via symlink
///
/// # Example
/// ```ignore
/// let base = std::fs::canonicalize("/var/www/static")?;
/// let path = validate_path_within_base(&base, "myproject/file.txt")?;
/// // path is guaranteed to be under /var/www/static
/// ```
pub fn validate_path_within_base(
    base_dir: &std::path::Path,
    file_path: &std::path::Path,
) -> Result<std::path::PathBuf, PathTraversalError> {
    let full_path = base_dir.join(file_path);

    // Try to canonicalize - this resolves symlinks
    // If the file doesn't exist, we check the parent directory
    let canonical = if full_path.exists() {
        full_path
            .canonicalize()
            .map_err(|_| PathTraversalError::EscapesBaseDirectory)?
    } else {
        // For non-existent files, canonicalize the parent and append the filename
        let parent = full_path.parent().ok_or(PathTraversalError::EmptyPath)?;
        let filename = full_path.file_name().ok_or(PathTraversalError::EmptyPath)?;

        if parent.exists() {
            let canonical_parent = parent
                .canonicalize()
                .map_err(|_| PathTraversalError::EscapesBaseDirectory)?;
            canonical_parent.join(filename)
        } else {
            // Parent doesn't exist - just use the joined path
            // The file read will fail anyway
            full_path
        }
    };

    // Verify the canonical path starts with the base directory
    // Also canonicalize base_dir to handle symlinks in the base path
    let canonical_base = base_dir
        .canonicalize()
        .map_err(|_| PathTraversalError::EscapesBaseDirectory)?;

    if !canonical.starts_with(&canonical_base) {
        return Err(PathTraversalError::EscapesBaseDirectory);
    }

    Ok(canonical)
}

/// Error type for path traversal attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathTraversalError {
    /// Path contains null bytes.
    NullByte,
    /// Path is empty.
    EmptyPath,
    /// Path is absolute (starts with `/` or drive letter).
    AbsolutePath,
    /// Path tries to escape the base directory using `..`.
    EscapesBaseDirectory,
    /// Path contains a reserved Windows device name (CON, PRN, NUL, etc.).
    ReservedWindowsName,
    /// Path is a Windows UNC path (\\server\share).
    UncPath,
    /// Path contains Windows alternate data stream syntax (`file:stream`).
    AlternateDataStream,
}

impl std::fmt::Display for PathTraversalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NullByte => write!(f, "Path contains null bytes"),
            Self::EmptyPath => write!(f, "Path is empty"),
            Self::AbsolutePath => write!(f, "Absolute paths are not allowed"),
            Self::EscapesBaseDirectory => {
                write!(f, "Path attempts to escape base directory using '..'")
            },
            Self::ReservedWindowsName => {
                write!(f, "Path contains reserved Windows device name")
            },
            Self::UncPath => write!(f, "UNC paths are not allowed"),
            Self::AlternateDataStream => {
                write!(f, "Alternate data streams are not allowed")
            },
        }
    }
}

impl std::error::Error for PathTraversalError {}

/// Error type for invalid module names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleNameError {
    /// Module name is empty.
    EmptyName,
    /// Module name contains null bytes.
    NullByte,
    /// Module name contains path separators.
    PathSeparator,
    /// Module name is a special directory (`.` or `..`).
    SpecialDirectory,
    /// Module name is too long (> 255 characters).
    TooLong,
    /// Module name contains control characters.
    ControlCharacter,
}

impl std::fmt::Display for ModuleNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => write!(f, "Module name is empty"),
            Self::NullByte => write!(f, "Module name contains null bytes"),
            Self::PathSeparator => write!(f, "Module name contains path separators"),
            Self::SpecialDirectory => write!(f, "Module name cannot be '.' or '..'"),
            Self::TooLong => write!(f, "Module name is too long (max 255 characters)"),
            Self::ControlCharacter => write!(f, "Module name contains control characters"),
        }
    }
}

impl std::error::Error for ModuleNameError {}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_sanitize_module_name_valid() {
        assert!(sanitize_module_name("api").is_ok());
        assert!(sanitize_module_name("user-service").is_ok());
        assert!(sanitize_module_name("my_module").is_ok());
        assert!(sanitize_module_name("service123").is_ok());
    }

    #[test]
    fn test_sanitize_module_name_path_separator() {
        assert_eq!(
            sanitize_module_name("api/users"),
            Err(ModuleNameError::PathSeparator)
        );
        assert_eq!(
            sanitize_module_name("..\\..\\etc\\passwd"),
            Err(ModuleNameError::PathSeparator)
        );
    }

    #[test]
    fn test_sanitize_module_name_special_directory() {
        assert_eq!(
            sanitize_module_name(".."),
            Err(ModuleNameError::SpecialDirectory)
        );
        assert_eq!(
            sanitize_module_name("."),
            Err(ModuleNameError::SpecialDirectory)
        );
    }

    #[test]
    fn test_sanitize_module_name_empty() {
        assert_eq!(sanitize_module_name(""), Err(ModuleNameError::EmptyName));
    }

    #[test]
    fn test_sanitize_module_name_null_byte() {
        assert_eq!(
            sanitize_module_name("api\0"),
            Err(ModuleNameError::NullByte)
        );
    }

    #[test]
    fn test_sanitize_module_name_too_long() {
        let long_name = "a".repeat(256);
        assert_eq!(
            sanitize_module_name(&long_name),
            Err(ModuleNameError::TooLong)
        );
    }

    #[test]
    fn test_sanitize_module_name_control_chars() {
        assert_eq!(
            sanitize_module_name("api\x01"),
            Err(ModuleNameError::ControlCharacter)
        );
        assert_eq!(
            sanitize_module_name("api\n"),
            Err(ModuleNameError::ControlCharacter)
        );
    }

    #[test]
    fn test_validate_path_within_base_valid() {
        use std::fs;
        use tempfile::tempdir;

        let base = tempdir().unwrap();
        let file_path = base.path().join("test.txt");
        fs::write(&file_path, "test").unwrap();

        // Valid file within base
        let result = validate_path_within_base(base.path(), Path::new("test.txt"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_path_within_base_nonexistent() {
        use tempfile::tempdir;

        let base = tempdir().unwrap();

        // Non-existent file should still return Ok (let file read handle 404)
        let result = validate_path_within_base(base.path(), Path::new("nonexistent.txt"));
        // Result depends on whether parent exists - base exists so this should work
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_windows_path_reserved_names() {
        // Reserved device names should be rejected
        assert_eq!(
            validate_windows_path("CON"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("con"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("PRN"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("NUL"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("COM1"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("LPT9"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        // Reserved names with extensions should also be rejected
        assert_eq!(
            validate_windows_path("CON.txt"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("nul.anything"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        // Reserved names in subdirectories
        assert_eq!(
            validate_windows_path("folder/CON"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("folder\\NUL.txt"),
            Err(PathTraversalError::ReservedWindowsName)
        );
    }

    #[test]
    fn test_validate_windows_path_unc() {
        // UNC paths should be rejected
        assert_eq!(
            validate_windows_path("\\\\server\\share"),
            Err(PathTraversalError::UncPath)
        );
        assert_eq!(
            validate_windows_path("//server/share"),
            Err(PathTraversalError::UncPath)
        );
        assert_eq!(
            validate_windows_path("\\\\192.168.1.1\\c$"),
            Err(PathTraversalError::UncPath)
        );
    }

    #[test]
    fn test_validate_windows_path_alternate_data_stream() {
        // Alternate data streams should be rejected
        assert_eq!(
            validate_windows_path("file.txt:hidden"),
            Err(PathTraversalError::AlternateDataStream)
        );
        assert_eq!(
            validate_windows_path("file.txt:$DATA"),
            Err(PathTraversalError::AlternateDataStream)
        );
        assert_eq!(
            validate_windows_path("folder/file:stream"),
            Err(PathTraversalError::AlternateDataStream)
        );
    }

    #[test]
    fn test_validate_windows_path_valid() {
        // Normal paths should be allowed
        assert!(validate_windows_path("normal_file.txt").is_ok());
        assert!(validate_windows_path("my_folder/file.txt").is_ok());
        assert!(validate_windows_path("assets\\images\\logo.png").is_ok());
        // Names that look like reserved but aren't
        assert!(validate_windows_path("CONSOLE.txt").is_ok());
        assert!(validate_windows_path("PRNT.txt").is_ok());
        assert!(validate_windows_path("COM10.txt").is_ok()); // Only COM1-9 are reserved
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_path_within_base_symlink_escape() {
        use std::fs;
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let base = tempdir().unwrap();
        let evil_target = tempdir().unwrap();

        // Create evil file outside base
        let evil_file = evil_target.path().join("secret.txt");
        fs::write(&evil_file, "secret data").unwrap();

        // Create symlink inside base pointing to evil file
        let symlink_path = base.path().join("evil_link");
        symlink(&evil_file, &symlink_path).unwrap();

        // Should reject the symlink that escapes base
        let result = validate_path_within_base(base.path(), Path::new("evil_link"));
        assert_eq!(result, Err(PathTraversalError::EscapesBaseDirectory));
    }

    // =========================================================================
    // COMPREHENSIVE PATH TRAVERSAL EDGE CASE TESTS
    // =========================================================================

    #[test]
    fn test_path_traversal_basic() {
        // Basic path traversal attempts
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

        // Mixed encoding - behavior differs by platform:
        // - On Unix: `..%2Fetc/passwd` is parsed as `..` + `%2Fetc` + `passwd`, so ".." is caught
        // - On Windows: `..%2Fetc` may be parsed as a single component (weird folder name)
        #[cfg(unix)]
        assert_eq!(
            sanitize_file_path("..%2Fetc/passwd"),
            Err(PathTraversalError::EscapesBaseDirectory)
        );

        #[cfg(windows)]
        {
            // On Windows, this is parsed as a literal folder name "..%2Fetc" + "passwd"
            // which is weird but not a traversal attack (the ".." isn't a component)
            let result = sanitize_file_path("..%2Fetc/passwd");
            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_path_traversal_backslash_variants() {
        // Windows-style path traversal with backslashes
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

    #[test]
    fn test_path_traversal_mixed_separators() {
        // Mix of forward and back slashes
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

    #[test]
    fn test_null_byte_in_module_name() {
        assert_eq!(
            sanitize_module_name("valid\0evil"),
            Err(ModuleNameError::NullByte)
        );
        assert_eq!(sanitize_module_name("\0"), Err(ModuleNameError::NullByte));
        assert_eq!(
            sanitize_module_name("module\0"),
            Err(ModuleNameError::NullByte)
        );
    }

    // =========================================================================
    // MODULE NAME VALIDATION TESTS
    // =========================================================================

    #[test]
    fn test_module_name_valid() {
        // Various valid module name formats
        assert!(sanitize_module_name("my-module").is_ok());
        assert!(sanitize_module_name("module_v2").is_ok());
        assert!(sanitize_module_name("CamelCase").is_ok());
        assert!(sanitize_module_name("UPPERCASE").is_ok());
        assert!(sanitize_module_name("lowercase").is_ok());
        assert!(sanitize_module_name("mix3d-numb3rs_123").is_ok());
        assert!(sanitize_module_name("a").is_ok()); // Single char
        assert!(sanitize_module_name("module.wasm").is_ok()); // With extension
        assert!(sanitize_module_name("module.service.v2").is_ok()); // Multiple dots
    }

    #[test]
    fn test_module_name_invalid_chars() {
        // Path separators
        assert_eq!(
            sanitize_module_name("../bad"),
            Err(ModuleNameError::PathSeparator)
        );
        assert_eq!(
            sanitize_module_name("bad/module"),
            Err(ModuleNameError::PathSeparator)
        );
        assert_eq!(
            sanitize_module_name("bad\\module"),
            Err(ModuleNameError::PathSeparator)
        );
        assert_eq!(
            sanitize_module_name("/absolute"),
            Err(ModuleNameError::PathSeparator)
        );
        assert_eq!(
            sanitize_module_name("\\backslash"),
            Err(ModuleNameError::PathSeparator)
        );
    }

    #[test]
    fn test_module_name_too_long() {
        // Exactly at limit (255 chars) - should pass
        let at_limit = "a".repeat(255);
        assert!(sanitize_module_name(&at_limit).is_ok());

        // One over limit (256 chars) - should fail
        let over_limit = "a".repeat(256);
        assert_eq!(
            sanitize_module_name(&over_limit),
            Err(ModuleNameError::TooLong)
        );

        // Way over limit
        let way_over = "a".repeat(1000);
        assert_eq!(
            sanitize_module_name(&way_over),
            Err(ModuleNameError::TooLong)
        );
    }

    #[test]
    fn test_module_name_control_characters() {
        // Various control characters
        assert_eq!(
            sanitize_module_name("mod\x00ule"), // Null (also caught by null check)
            Err(ModuleNameError::NullByte)
        );
        assert_eq!(
            sanitize_module_name("mod\x01ule"), // SOH
            Err(ModuleNameError::ControlCharacter)
        );
        assert_eq!(
            sanitize_module_name("mod\x1Bule"), // Escape
            Err(ModuleNameError::ControlCharacter)
        );
        assert_eq!(
            sanitize_module_name("module\r"), // Carriage return
            Err(ModuleNameError::ControlCharacter)
        );
        assert_eq!(
            sanitize_module_name("module\n"), // Newline
            Err(ModuleNameError::ControlCharacter)
        );
        assert_eq!(
            sanitize_module_name("module\t"), // Tab
            Err(ModuleNameError::ControlCharacter)
        );
    }

    #[test]
    fn test_module_name_special_directories() {
        assert_eq!(
            sanitize_module_name("."),
            Err(ModuleNameError::SpecialDirectory)
        );
        assert_eq!(
            sanitize_module_name(".."),
            Err(ModuleNameError::SpecialDirectory)
        );
        // But these should be valid (contain dots but aren't special)
        assert!(sanitize_module_name("...").is_ok());
        assert!(sanitize_module_name(".hidden").is_ok());
        assert!(sanitize_module_name("file.txt").is_ok());
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
    // WINDOWS-SPECIFIC SECURITY TESTS
    // =========================================================================

    #[test]
    fn test_windows_reserved_names_comprehensive() {
        // All reserved device names
        for name in &["CON", "PRN", "AUX", "NUL"] {
            assert_eq!(
                validate_windows_path(name),
                Err(PathTraversalError::ReservedWindowsName),
                "Should reject {}",
                name
            );
        }

        // COM ports 1-9
        for i in 1..=9 {
            let name = format!("COM{}", i);
            assert_eq!(
                validate_windows_path(&name),
                Err(PathTraversalError::ReservedWindowsName),
                "Should reject {}",
                name
            );
        }

        // LPT ports 1-9
        for i in 1..=9 {
            let name = format!("LPT{}", i);
            assert_eq!(
                validate_windows_path(&name),
                Err(PathTraversalError::ReservedWindowsName),
                "Should reject {}",
                name
            );
        }
    }

    #[test]
    fn test_windows_reserved_names_case_insensitive() {
        // Case variations
        assert_eq!(
            validate_windows_path("con"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("CoN"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("cON"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("nUl"),
            Err(PathTraversalError::ReservedWindowsName)
        );
    }

    #[test]
    fn test_windows_reserved_with_extensions() {
        // Reserved names with extensions are still dangerous
        assert_eq!(
            validate_windows_path("CON.txt"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("NUL.exe"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("COM1.log"),
            Err(PathTraversalError::ReservedWindowsName)
        );
        assert_eq!(
            validate_windows_path("aux.anything.here"),
            Err(PathTraversalError::ReservedWindowsName)
        );
    }

    #[test]
    fn test_windows_alternate_data_streams() {
        // ADS syntax
        assert_eq!(
            validate_windows_path("file.txt:Zone.Identifier"),
            Err(PathTraversalError::AlternateDataStream)
        );
        assert_eq!(
            validate_windows_path("file:$DATA"),
            Err(PathTraversalError::AlternateDataStream)
        );
        assert_eq!(
            validate_windows_path("file.txt::$DATA"),
            Err(PathTraversalError::AlternateDataStream)
        );
    }

    #[test]
    fn test_windows_unc_paths() {
        // UNC path variations
        assert_eq!(
            validate_windows_path("\\\\server\\share\\file.txt"),
            Err(PathTraversalError::UncPath)
        );
        assert_eq!(
            validate_windows_path("//server/share/file.txt"),
            Err(PathTraversalError::UncPath)
        );
        assert_eq!(
            validate_windows_path("\\\\?\\C:\\path"),
            Err(PathTraversalError::UncPath)
        );
        assert_eq!(
            validate_windows_path("\\\\localhost\\c$"),
            Err(PathTraversalError::UncPath)
        );
    }

    #[test]
    fn test_windows_safe_names_similar_to_reserved() {
        // Names that look similar but aren't reserved
        assert!(validate_windows_path("CONN.txt").is_ok()); // Extra N
        assert!(validate_windows_path("PRNT.txt").is_ok()); // Extra T
        assert!(validate_windows_path("AUXILLARY.txt").is_ok()); // Extra chars
        assert!(validate_windows_path("NULLIFY.txt").is_ok()); // Contains NUL
        assert!(validate_windows_path("COM10.txt").is_ok()); // COM10+ not reserved
        assert!(validate_windows_path("COM0.txt").is_ok()); // COM0 not reserved
        assert!(validate_windows_path("LPT0.txt").is_ok()); // LPT0 not reserved
        assert!(validate_windows_path("LPT10.txt").is_ok()); // LPT10+ not reserved
    }

    // =========================================================================
    // ERROR TYPE TESTS
    // =========================================================================

    #[test]
    fn test_path_traversal_error_display() {
        assert_eq!(
            PathTraversalError::NullByte.to_string(),
            "Path contains null bytes"
        );
        assert_eq!(PathTraversalError::EmptyPath.to_string(), "Path is empty");
        assert_eq!(
            PathTraversalError::AbsolutePath.to_string(),
            "Absolute paths are not allowed"
        );
        assert_eq!(
            PathTraversalError::EscapesBaseDirectory.to_string(),
            "Path attempts to escape base directory using '..'"
        );
        assert_eq!(
            PathTraversalError::ReservedWindowsName.to_string(),
            "Path contains reserved Windows device name"
        );
        assert_eq!(
            PathTraversalError::UncPath.to_string(),
            "UNC paths are not allowed"
        );
        assert_eq!(
            PathTraversalError::AlternateDataStream.to_string(),
            "Alternate data streams are not allowed"
        );
    }

    #[test]
    fn test_module_name_error_display() {
        assert_eq!(
            ModuleNameError::EmptyName.to_string(),
            "Module name is empty"
        );
        assert_eq!(
            ModuleNameError::NullByte.to_string(),
            "Module name contains null bytes"
        );
        assert_eq!(
            ModuleNameError::PathSeparator.to_string(),
            "Module name contains path separators"
        );
        assert_eq!(
            ModuleNameError::SpecialDirectory.to_string(),
            "Module name cannot be '.' or '..'"
        );
        assert_eq!(
            ModuleNameError::TooLong.to_string(),
            "Module name is too long (max 255 characters)"
        );
        assert_eq!(
            ModuleNameError::ControlCharacter.to_string(),
            "Module name contains control characters"
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
