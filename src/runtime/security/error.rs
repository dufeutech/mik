//! Error types for security validation.
//!
//! This module defines error types for path traversal and module name validation
//! failures. These errors are used to communicate specific security violations
//! detected during input sanitization.

use thiserror::Error;

/// Error type for path traversal attempts.
///
/// This error is returned when a path fails security validation due to
/// potential directory traversal attacks, reserved names, or other
/// path-based security issues.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathTraversalError {
    #[error("Path contains null bytes")]
    NullByte,
    #[error("Path is empty")]
    EmptyPath,
    #[error("Absolute paths are not allowed")]
    AbsolutePath,
    #[error("Path attempts to escape base directory using '..'")]
    EscapesBaseDirectory,
    #[error("Path contains reserved Windows device name")]
    ReservedWindowsName,
    #[error("UNC paths are not allowed")]
    UncPath,
    #[error("Alternate data streams are not allowed")]
    AlternateDataStream,
}

/// Error type for invalid module names.
///
/// This error is returned when a module name fails validation due to
/// security concerns such as path separators, control characters, or
/// special directory names.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModuleNameError {
    #[error("Module name is empty")]
    EmptyName,
    #[error("Module name contains null bytes")]
    NullByte,
    #[error("Module name contains path separators")]
    PathSeparator,
    #[error("Module name cannot be '.' or '..'")]
    SpecialDirectory,
    #[error("Module name is too long (max 255 characters)")]
    TooLong,
    #[error("Module name contains control characters")]
    ControlCharacter,
}
