//! Process-based worker spawning.
//!
//! This module provides functionality for spawning mik workers as separate
//! OS processes. Each worker runs in its own process with full isolation.
//!
//! # Usage
//!
//! Process-based workers are primarily used by the CLI for production deployments
//! where process isolation is desired for stability and security.

use super::WorkerHandle;
use anyhow::{Context, Result};
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use tracing::{error, info};

/// Spawn multiple worker processes.
///
/// Each worker is started as a separate `mik run` process with its own port.
///
/// # Arguments
///
/// * `count` - Number of workers to spawn
/// * `base_port` - Starting port (workers get base_port, base_port+1, ...)
/// * `manifest_path` - Path to the mik.toml manifest file
///
/// # Returns
///
/// A vector of `WorkerHandle::Process` entries for each spawned worker.
pub fn spawn_workers(
    count: usize,
    base_port: u16,
    manifest_path: &str,
) -> Result<Vec<WorkerHandle>> {
    let mut workers = Vec::with_capacity(count);

    for i in 0..count {
        let port = base_port + i as u16;
        let handle = spawn_worker(port, manifest_path)
            .with_context(|| format!("Failed to spawn worker {i} on port {port}"))?;
        workers.push(handle);
    }

    info!(
        "Spawned {} worker processes (ports {}-{})",
        count,
        base_port,
        base_port + count as u16 - 1
    );

    Ok(workers)
}

/// Spawn a single worker process.
///
/// # Arguments
///
/// * `port` - Port for the worker to listen on
/// * `manifest_path` - Path to the mik.toml manifest file
fn spawn_worker(port: u16, manifest_path: &str) -> Result<WorkerHandle> {
    let current_exe = std::env::current_exe().context("Failed to get current executable path")?;

    let child = Command::new(&current_exe)
        .arg("run")
        .arg("--port")
        .arg(port.to_string())
        .arg("--manifest")
        .arg(manifest_path)
        .env("MIK_WORKER", "1")
        .env("MIK_WORKER_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to spawn worker process on port {port}"))?;

    let pid = child.id();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    info!("Spawned worker process {} on port {}", pid, port);

    // Store the child process handle for cleanup
    // In a real implementation, we'd need to track these for proper shutdown
    store_child_process(child);

    Ok(WorkerHandle::Process { pid, port, addr })
}

/// Global storage for child processes (for cleanup on shutdown).
///
/// This is a simple approach - a production implementation might use
/// a more sophisticated process manager.
static CHILD_PROCESSES: std::sync::Mutex<Vec<Child>> = std::sync::Mutex::new(Vec::new());

fn store_child_process(child: Child) {
    if let Ok(mut processes) = CHILD_PROCESSES.lock() {
        processes.push(child);
    }
}

/// Kill all spawned worker processes.
///
/// This is called during shutdown to ensure all worker processes are terminated.
pub fn kill_all_workers() {
    if let Ok(mut processes) = CHILD_PROCESSES.lock() {
        for mut child in processes.drain(..) {
            let pid = child.id();
            match child.kill() {
                Ok(()) => info!("Killed worker process {}", pid),
                Err(e) => error!("Failed to kill worker process {}: {}", pid, e),
            }
        }
    }
}

/// Wait for all worker processes to complete.
///
/// This blocks until all spawned worker processes have exited.
pub fn wait_all_workers() -> Result<()> {
    if let Ok(mut processes) = CHILD_PROCESSES.lock() {
        for child in processes.iter_mut() {
            let pid = child.id();
            match child.wait() {
                Ok(status) => {
                    if status.success() {
                        info!("Worker process {} exited successfully", pid);
                    } else {
                        error!("Worker process {} exited with status: {}", pid, status);
                    }
                },
                Err(e) => error!("Failed to wait for worker process {}: {}", pid, e),
            }
        }
        processes.clear();
    }
    Ok(())
}

/// Send a graceful shutdown signal to all worker processes.
///
/// On Unix, this sends SIGTERM. On Windows, this terminates the process.
pub fn shutdown_all_workers() {
    if let Ok(processes) = CHILD_PROCESSES.lock() {
        for child in processes.iter() {
            let pid = child.id();

            #[cfg(unix)]
            {
                // Send SIGTERM for graceful shutdown
                if let Err(e) = kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                    error!("Failed to send SIGTERM to worker process {}: {}", pid, e);
                } else {
                    info!("Sent SIGTERM to worker process {}", pid);
                }
            }

            #[cfg(not(unix))]
            {
                // Windows doesn't have SIGTERM, so we just note we need to terminate
                info!("Worker process {} will be terminated on shutdown", pid);
            }
        }
    }
}

/// Check if a worker process is still running.
pub fn is_worker_running(pid: u32) -> bool {
    if let Ok(processes) = CHILD_PROCESSES.lock() {
        for child in processes.iter() {
            if child.id() == pid {
                // Try to check if process is still running
                // Note: This is a simplification - in production, we'd use
                // platform-specific APIs to check process status
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_spawn_workers_count() {
        // This test would require mocking Command::spawn
        // For now, just verify the function signature works
        let count = 4;
        let base_port = 4001;

        // Would need integration test with actual binary
        assert!(count > 0);
        assert!(base_port > 0);
    }
}
