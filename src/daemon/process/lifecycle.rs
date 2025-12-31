//! Process lifecycle management for mik daemon instances.
//!
//! This module handles spawning and terminating mik server instances
//! as background processes. Each instance runs independently with its
//! own port, logs, and configuration.

#![allow(clippy::collapsible_if)]

use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};

use super::log_rotation::rotate_log_if_needed;
use super::types::{InstanceInfo, LogRotationConfig, SpawnConfig};
use super::utils::get_log_dir;

#[cfg(unix)]
use super::health::is_running;

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
/// - `setsid()` is async-signal-safe according to `POSIX`
/// - No memory allocation or locking occurs in the `pre_exec` closure
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
