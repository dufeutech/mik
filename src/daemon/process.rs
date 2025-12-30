//! Process management for mik daemon instances.
//!
//! This module handles spawning and managing mik server instances as background
//! processes. Each instance runs independently with its own port, logs, and
//! configuration.
//!
//! ## Log Rotation
//!
//! Log files are automatically rotated when they exceed a configured size.
//! Rotated logs are renamed with a timestamp suffix (e.g., `default.log.20250101-120000`).

#![allow(dead_code)] // Daemon infrastructure for future background mode
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_wraps)]

use anyhow::{Context, Result};
use chrono::Utc;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Default maximum log file size before rotation (10 MB).
const DEFAULT_MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Default number of rotated log files to keep.
const DEFAULT_MAX_LOG_FILES: usize = 5;

/// Configuration for log rotation.
#[derive(Debug, Clone)]
pub struct LogRotationConfig {
    /// Maximum log file size in bytes before rotation.
    pub max_size: u64,
    /// Maximum number of rotated log files to keep.
    pub max_files: usize,
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_LOG_SIZE,
            max_files: DEFAULT_MAX_LOG_FILES,
        }
    }
}

impl LogRotationConfig {
    /// Create a config with custom size limit in megabytes.
    ///
    /// # Note
    /// Provided for API completeness. Currently `spawn_instance` uses defaults.
    #[allow(dead_code)]
    pub fn with_size_mb(mb: u64) -> Self {
        Self {
            max_size: mb * 1024 * 1024,
            ..Default::default()
        }
    }

    /// Set the maximum number of rotated files to keep.
    ///
    /// # Note
    /// Provided for API completeness. Currently `spawn_instance` uses defaults.
    #[allow(dead_code)]
    #[must_use]
    pub fn max_files(mut self, count: usize) -> Self {
        self.max_files = count;
        self
    }
}

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
    cleanup_old_logs(log_path, config.max_files)?;

    Ok(true)
}

/// Cleans up old rotated log files, keeping only the most recent ones.
fn cleanup_old_logs(log_path: &Path, max_files: usize) -> Result<()> {
    let log_dir = log_path.parent().unwrap_or(Path::new("."));
    let log_name = log_path.file_name().unwrap_or_default().to_string_lossy();

    // Find all rotated log files (matching pattern: {name}.{timestamp})
    let mut rotated_files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

    if let Ok(entries) = fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path.file_name().unwrap_or_default().to_string_lossy();

            // Check if this is a rotated version of our log file
            if filename.starts_with(&format!("{log_name}.")) && path != log_path {
                if let Ok(metadata) = fs::metadata(&path) {
                    if let Ok(modified) = metadata.modified() {
                        rotated_files.push((path, modified));
                    }
                }
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

    Ok(())
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
            if path.extension().is_some_and(|ext| ext == "log") {
                if rotate_log_if_needed(&path, config)? {
                    rotated_count += 1;
                }
            }
        }
    }

    Ok(rotated_count)
}

/// Configuration for spawning a new mik instance.
#[derive(Debug, Clone, Default)]
pub struct SpawnConfig {
    /// Name of the instance (used for logging and tracking).
    pub name: String,
    /// Port to bind the HTTP server to.
    pub port: u16,
    /// Path to the mik.toml configuration file.
    pub config_path: PathBuf,
    /// Working directory for the process.
    pub working_dir: PathBuf,
    /// Enable hot-reload mode (bypasses AOT cache).
    pub hot_reload: bool,
}

/// Information about a running instance.
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    /// Process ID of the running instance.
    pub pid: u32,
    /// Path to the log file.
    pub log_path: PathBuf,
}

impl SpawnConfig {
    /// Set hot-reload mode (bypasses AOT cache for faster restarts during development).
    #[must_use]
    pub fn with_hot_reload(mut self, enabled: bool) -> Self {
        self.hot_reload = enabled;
        self
    }
}

/// Spawns a new mik server instance as a background process.
///
/// The instance runs independently with stdout/stderr redirected to a log file
/// at `~/.mik/logs/{name}.log`. The process is detached from the parent and
/// continues running even after this function returns.
///
/// Log files are automatically rotated when they exceed the default size limit
/// (10 MB). Use `spawn_instance_with_rotation` for custom rotation settings.
///
/// # Arguments
///
/// * `config` - Configuration for the instance to spawn
///
/// # Returns
///
/// Information about the spawned instance including PID and log path.
///
/// # Errors
///
/// Returns an error if:
/// - The log directory cannot be created
/// - The log file cannot be opened
/// - The process fails to spawn
/// - The current executable path cannot be determined
pub fn spawn_instance(config: &SpawnConfig) -> Result<InstanceInfo> {
    spawn_instance_with_rotation(config, &LogRotationConfig::default())
}

/// Spawns a new mik server instance with custom log rotation settings.
///
/// Same as `spawn_instance` but allows configuring log rotation behavior.
///
/// # Arguments
///
/// * `config` - Configuration for the instance to spawn
/// * `rotation` - Log rotation configuration
///
/// # Errors
///
/// Returns an error if:
/// - The log directory cannot be created
/// - The log file cannot be opened
/// - The current executable path cannot be determined
/// - The child process fails to spawn
///
/// # Safety
///
/// On Unix systems, this function uses `pre_exec` to call `setsid()` which
/// creates a new process group. This is safe because:
/// - `setsid()` is async-signal-safe according to POSIX
/// - No memory allocation or locking occurs in the pre_exec closure
/// - The closure only calls libc functions that are safe in this context
#[allow(unsafe_code)] // SAFETY: Unix pre_exec/setsid for process group detachment
pub fn spawn_instance_with_rotation(
    config: &SpawnConfig,
    rotation: &LogRotationConfig,
) -> Result<InstanceInfo> {
    // Ensure log directory exists
    let log_dir = get_log_dir()?;
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    // Create log file path
    let log_path = log_dir.join(format!("{}.log", config.name));

    // Rotate log if needed before opening
    rotate_log_if_needed(&log_path, rotation)?;

    // Open log file for writing (append mode)
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;

    // Get the current executable path to spawn mik with the same binary
    let current_exe = std::env::current_exe().context("Failed to get current executable path")?;

    // Build the command to spawn mik server
    let mut cmd = Command::new(current_exe);

    cmd.arg("run")
        .arg("--port")
        .arg(config.port.to_string())
        .arg("--config")
        .arg(&config.config_path)
        .current_dir(&config.working_dir)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file);

    // Set hot-reload mode env var if enabled (bypasses AOT cache)
    if config.hot_reload {
        cmd.env("MIK_HOT_RELOAD", "1");
    }

    // Platform-specific process detachment
    #[cfg(unix)]
    {
        use nix::libc;
        use std::os::unix::process::CommandExt;

        // On Unix, use process groups to detach from parent
        unsafe {
            cmd.pre_exec(|| {
                // Create new process group
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // On Windows, use CREATE_NEW_PROCESS_GROUP to detach
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;

        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    // Spawn the process
    let child = cmd.spawn().context("Failed to spawn mik server process")?;

    // Get PID before releasing the handle
    let pid = child.id();

    // Drop the child handle to release resources.
    // Note: Child::drop() does NOT wait for the child process - it only
    // closes our handle to the process. The process continues running
    // independently because we've already detached it via setsid (Unix)
    // or DETACHED_PROCESS (Windows).
    //
    // Previously this used std::mem::forget(child) which leaked the
    // internal file descriptors and handles.
    drop(child);

    tracing::info!(
        instance = %config.name,
        pid = pid,
        port = config.port,
        log = %log_path.display(),
        "Spawned mik instance"
    );

    Ok(InstanceInfo { pid, log_path })
}

/// Kills a running instance by sending a termination signal.
///
/// Attempts graceful shutdown first (SIGTERM on Unix, equivalent on Windows),
/// and falls back to forceful termination if the process doesn't exit.
///
/// # Arguments
///
/// * `pid` - Process ID of the instance to kill
///
/// # Errors
///
/// Returns an error if the process cannot be killed or doesn't exist.
pub fn kill_instance(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{self, Signal};
        use nix::unistd::Pid as NixPid;

        let nix_pid = NixPid::from_raw(pid as i32);

        // Try graceful shutdown first (SIGTERM)
        signal::kill(nix_pid, Signal::SIGTERM)
            .with_context(|| format!("Failed to send SIGTERM to process {}", pid))?;

        // Wait briefly for graceful shutdown
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Check if process is still running
        if is_running(pid)? {
            tracing::warn!(
                pid = pid,
                "Process didn't respond to SIGTERM, sending SIGKILL"
            );

            // Force kill with SIGKILL
            signal::kill(nix_pid, Signal::SIGKILL)
                .with_context(|| format!("Failed to send SIGKILL to process {}", pid))?;
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // On Windows, use taskkill for graceful shutdown
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()
            .with_context(|| format!("Failed to kill process {pid}"))?;

        if !status.success() {
            // Try forceful kill
            let force_status = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .status()
                .with_context(|| format!("Failed to force kill process {pid}"))?;

            if !force_status.success() {
                anyhow::bail!("Failed to kill process {pid}");
            }
        }
    }

    tracing::info!(pid = pid, "Killed instance");
    Ok(())
}

/// Checks if a process with the given PID is currently running.
///
/// Uses sysinfo to query the system's process table and verify the process
/// exists and is active.
///
/// # Arguments
///
/// * `pid` - Process ID to check
///
/// # Returns
///
/// `true` if the process is running, `false` otherwise.
///
/// # Errors
///
/// This function returns `Ok(false)` for non-existent processes rather than
/// erroring, making it safe to use for polling process status.
pub fn is_running(pid: u32) -> Result<bool> {
    let mut system = System::new();

    // Refresh all processes
    system.refresh_processes(ProcessesToUpdate::All, true);

    Ok(system.process(Pid::from(pid as usize)).is_some())
}

/// Gets the log directory path for mik instances.
///
/// Returns `~/.mik/logs` on all platforms.
fn get_log_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to get home directory")?;

    Ok(home.join(".mik").join("logs"))
}

/// Reads the last N lines from a log file.
///
/// Useful for implementing `mik logs` command to show recent log entries.
///
/// # Arguments
///
/// * `log_path` - Path to the log file
/// * `lines` - Number of lines to read from the end
///
/// # Returns
///
/// A vector of log lines (most recent first).
pub fn tail_log(log_path: &Path, lines: usize) -> Result<Vec<String>> {
    let file = File::open(log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;

    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader
        .lines()
        .collect::<std::io::Result<_>>()
        .context("Failed to read log file")?;

    // Take last N lines
    let start = all_lines.len().saturating_sub(lines);
    Ok(all_lines[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_get_log_dir() {
        let log_dir = get_log_dir().expect("Failed to get log dir");
        assert!(log_dir.ends_with(".mik/logs"));
    }

    #[test]
    fn test_tail_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("test.log");

        let mut file = File::create(&log_file).unwrap();
        writeln!(file, "Line 1").unwrap();
        writeln!(file, "Line 2").unwrap();
        writeln!(file, "Line 3").unwrap();
        writeln!(file, "Line 4").unwrap();
        writeln!(file, "Line 5").unwrap();

        let lines = tail_log(&log_file, 3).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "Line 3");
        assert_eq!(lines[1], "Line 4");
        assert_eq!(lines[2], "Line 5");
    }

    #[test]
    fn test_tail_log_more_than_available() {
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("test.log");

        let mut file = File::create(&log_file).unwrap();
        writeln!(file, "Line 1").unwrap();
        writeln!(file, "Line 2").unwrap();

        let lines = tail_log(&log_file, 10).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Line 1");
        assert_eq!(lines[1], "Line 2");
    }

    #[test]
    fn test_is_running_current_process() {
        // Current process should always be running
        let current_pid = std::process::id();
        assert!(is_running(current_pid).unwrap());
    }

    #[test]
    fn test_is_running_nonexistent_process() {
        // Very high PID unlikely to exist
        let fake_pid = u32::MAX - 1;
        assert!(!is_running(fake_pid).unwrap());
    }

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
        cleanup_old_logs(&log_file, 2).unwrap();

        // Count remaining rotated files
        let remaining: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("test.log."))
            .collect();

        assert_eq!(remaining.len(), 2);
    }
}
