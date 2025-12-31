//! Log rotation functionality for mik daemon instances.
//!
//! This module handles automatic rotation of log files when they exceed
//! a configured size. Rotated logs are renamed with a timestamp suffix
//! (e.g., `default.log.20250101-120000`).

use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

use super::types::LogRotationConfig;
use super::utils::get_log_dir;

/// Rotates a log file if it exceeds the configured size.
///
/// When rotation occurs:
/// 1. The current log file is renamed with a timestamp suffix
/// 2. Old rotated files exceeding `max_files` are deleted
/// 3. A new empty log file is created
///
/// # Arguments
///
/// * `log_path` - Path to the log file to check/rotate
/// * `config` - Rotation configuration
///
/// # Returns
///
/// `Ok(true)` if rotation occurred, `Ok(false)` if no rotation was needed.
pub fn rotate_log_if_needed(log_path: &Path, config: &LogRotationConfig) -> Result<bool> {
    // Check if file exists and get its size
    let metadata = match fs::metadata(log_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("Failed to get log file metadata"),
    };

    if metadata.len() < config.max_size {
        return Ok(false);
    }

    // Generate rotated filename with timestamp
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let rotated_name = format!(
        "{}.{}",
        log_path.file_name().unwrap_or_default().to_string_lossy(),
        timestamp
    );
    let rotated_path = log_path.with_file_name(rotated_name);

    // Rename current log to rotated name
    fs::rename(log_path, &rotated_path)
        .with_context(|| format!("Failed to rotate log file to {}", rotated_path.display()))?;

    tracing::info!(
        log = %log_path.display(),
        rotated_to = %rotated_path.display(),
        size_mb = metadata.len() / (1024 * 1024),
        "Rotated log file"
    );

    // Clean up old rotated files
    cleanup_old_logs(log_path, config.max_files);

    Ok(true)
}

/// Cleans up old rotated log files, keeping only the most recent ones.
pub(crate) fn cleanup_old_logs(log_path: &Path, max_files: usize) {
    let log_dir = log_path.parent().unwrap_or_else(|| Path::new("."));
    let log_name = log_path.file_name().unwrap_or_default().to_string_lossy();

    // Find all rotated log files (matching pattern: {name}.{timestamp})
    let mut rotated_files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

    if let Ok(entries) = fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path.file_name().unwrap_or_default().to_string_lossy();

            // Check if this is a rotated version of our log file
            if filename.starts_with(&format!("{log_name}."))
                && path != log_path
                && let Ok(metadata) = fs::metadata(&path)
                && let Ok(modified) = metadata.modified()
            {
                rotated_files.push((path, modified));
            }
        }
    }

    // Sort by modification time (newest first)
    rotated_files.sort_by(|a, b| b.1.cmp(&a.1));

    // Delete files beyond the limit
    for (path, _) in rotated_files.iter().skip(max_files) {
        if let Err(e) = fs::remove_file(path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to delete old rotated log"
            );
        } else {
            tracing::debug!(path = %path.display(), "Deleted old rotated log");
        }
    }
}

/// Rotates all log files in the log directory.
///
/// Useful for periodic maintenance or before starting new instances.
///
/// # Note
/// Provided for API completeness. Use for maintenance tasks.
#[allow(dead_code)]
pub fn rotate_all_logs(config: &LogRotationConfig) -> Result<usize> {
    let log_dir = get_log_dir()?;

    if !log_dir.exists() {
        return Ok(0);
    }

    let mut rotated_count = 0;

    if let Ok(entries) = fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Only rotate .log files (not already rotated ones)
            if path.extension().is_some_and(|ext| ext == "log")
                && rotate_log_if_needed(&path, config)?
            {
                rotated_count += 1;
            }
        }
    }

    Ok(rotated_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_log_rotation_config_default() {
        let config = LogRotationConfig::default();
        assert_eq!(config.max_size, 10 * 1024 * 1024); // 10 MB
        assert_eq!(config.max_files, 5);
    }

    #[test]
    fn test_log_rotation_config_with_size_mb() {
        let config = LogRotationConfig::with_size_mb(5);
        assert_eq!(config.max_size, 5 * 1024 * 1024);
    }

    #[test]
    fn test_rotate_log_if_needed_no_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("test.log");

        // Create a small file
        let mut file = File::create(&log_file).unwrap();
        writeln!(file, "Small log content").unwrap();

        let config = LogRotationConfig::with_size_mb(1);
        let rotated = rotate_log_if_needed(&log_file, &config).unwrap();

        assert!(!rotated);
        assert!(log_file.exists());
    }

    #[test]
    fn test_rotate_log_if_needed_triggers_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("test.log");

        // Create a file larger than the threshold
        let mut file = File::create(&log_file).unwrap();
        let large_content = "x".repeat(1024); // 1KB line
        for _ in 0..200 {
            writeln!(file, "{large_content}").unwrap();
        }
        drop(file);

        // Use a small threshold to trigger rotation
        let config = LogRotationConfig {
            max_size: 100 * 1024, // 100 KB
            max_files: 3,
        };

        let rotated = rotate_log_if_needed(&log_file, &config).unwrap();
        assert!(rotated);

        // Original file should be gone (renamed)
        assert!(!log_file.exists());

        // There should be a rotated file
        let entries: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .file_name()
                .to_string_lossy()
                .starts_with("test.log.")
        );
    }

    #[test]
    fn test_rotate_log_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("nonexistent.log");

        let config = LogRotationConfig::default();
        let rotated = rotate_log_if_needed(&log_file, &config).unwrap();

        assert!(!rotated);
    }

    #[test]
    fn test_cleanup_old_logs() {
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("test.log");

        // Create several "rotated" log files
        for i in 0..5 {
            let rotated_name = format!("test.log.2024010{i}-120000");
            let path = temp_dir.path().join(rotated_name);
            File::create(&path).unwrap();
            // Small delay to ensure different modification times
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Keep only 2 files
        cleanup_old_logs(&log_file, 2);

        // Count remaining rotated files
        let remaining: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("test.log."))
            .collect();

        assert_eq!(remaining.len(), 2);
    }
}
