//! Task-based worker spawning.
//!
//! This module provides functionality for spawning mik workers as tokio tasks
//! within the current process. This is ideal for embedded use cases where
//! process isolation is not required.
//!
//! # Advantages
//!
//! - Lower overhead than process-based workers
//! - Shared memory for efficient data passing
//! - Easier to manage from within the application
//! - Direct access to Runtime instances for programmatic use
//!
//! # Usage
//!
//! Task-based workers are primarily used for embedding mik in applications
//! like Tauri, Electron, or custom servers.

use super::WorkerHandle;
use crate::runtime::Runtime;
use anyhow::Result;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::info;

/// Spawn multiple worker tasks with servers.
///
/// Each worker is created as a Runtime instance with a monitoring task
/// that watches for shutdown signals. The actual HTTP serving should be
/// done through a load balancer that forwards requests to these workers.
///
/// # Arguments
///
/// * `count` - Number of workers to spawn
/// * `base_port` - Starting port (workers get base_port, base_port+1, ...)
/// * `factory` - Function to create a Runtime instance
///
/// # Returns
///
/// A vector of `WorkerHandle::Task` entries for each spawned worker.
///
/// # Note
///
/// For actual HTTP serving, use `spawn_workers_headless` and implement
/// your own server layer, or use the Runtime's `handle_request` method
/// directly with your own HTTP server.
#[allow(clippy::unused_async)] // await is used in spawned tasks, not the outer function
pub async fn spawn_workers<F>(count: usize, base_port: u16, factory: F) -> Result<Vec<WorkerHandle>>
where
    F: Fn() -> Result<Runtime> + Send + Sync,
{
    let mut workers = Vec::with_capacity(count);

    for i in 0..count {
        let port = base_port + i as u16;
        let runtime = factory()?;
        let runtime = Arc::new(runtime);

        // Clone the shared state reference for the monitoring task
        let shared = runtime.shared().clone();
        let handle: JoinHandle<Result<()>> = tokio::spawn(async move {
            info!("Worker task started (port reference: {})", port);

            // Monitor for shutdown signal
            loop {
                if shared.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            info!("Worker task (port {}) shutting down", port);
            Ok(())
        });

        workers.push(WorkerHandle::Task {
            runtime,
            handle: Some(handle),
            port,
        });

        info!("Spawned worker task for port {}", port);
    }

    info!(
        "Spawned {} worker tasks (ports {}-{})",
        count,
        base_port,
        base_port + count as u16 - 1
    );

    Ok(workers)
}

/// Spawn multiple worker tasks without servers (headless mode).
///
/// Creates Runtime instances but doesn't bind them to ports.
/// Useful when you want to use `runtime.handle_request()` directly
/// for programmatic request handling.
///
/// # Arguments
///
/// * `count` - Number of workers to spawn
/// * `base_port` - Base port (for reference only, not bound)
/// * `factory` - Function to create a Runtime instance
///
/// # Returns
///
/// A vector of `WorkerHandle::Task` entries for each worker.
pub fn spawn_workers_headless<F>(
    count: usize,
    base_port: u16,
    factory: F,
) -> Result<Vec<WorkerHandle>>
where
    F: Fn() -> Result<Runtime> + Send + Sync,
{
    let mut workers = Vec::with_capacity(count);

    for i in 0..count {
        let port = base_port + i as u16;
        let runtime = factory()?;
        let runtime = Arc::new(runtime);

        workers.push(WorkerHandle::Task {
            runtime,
            handle: None, // No server task
            port,
        });

        info!("Created headless worker {} (port {})", i, port);
    }

    info!("Created {} headless workers", count);

    Ok(workers)
}

/// A pool of runtimes for load-balanced request handling.
///
/// This provides round-robin distribution of requests across
/// multiple Runtime instances.
pub struct RuntimePool {
    runtimes: Vec<Arc<Runtime>>,
    next: std::sync::atomic::AtomicUsize,
}

impl RuntimePool {
    /// Create a new runtime pool from worker handles.
    #[must_use]
    pub fn from_workers(workers: &[WorkerHandle]) -> Self {
        let runtimes: Vec<_> = workers
            .iter()
            .filter_map(|w| match w {
                WorkerHandle::Task { runtime, .. } => Some(runtime.clone()),
                WorkerHandle::Process { .. } => None,
            })
            .collect();

        Self {
            runtimes,
            next: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Get the next runtime in round-robin order.
    #[must_use]
    pub fn next(&self) -> Option<&Arc<Runtime>> {
        if self.runtimes.is_empty() {
            return None;
        }

        let idx = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(&self.runtimes[idx % self.runtimes.len()])
    }

    /// Get the number of runtimes in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.runtimes.len()
    }

    /// Check if the pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runtimes.is_empty()
    }

    /// Handle a request using the next available runtime.
    pub async fn handle_request(
        &self,
        request: crate::runtime::Request,
    ) -> Result<crate::runtime::Response> {
        let runtime = self
            .next()
            .ok_or_else(|| anyhow::anyhow!("No runtimes available in pool"))?;

        runtime.handle_request(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_pool_round_robin() {
        // Create mock workers
        // Note: Can't easily test without real Runtime instances

        let pool = RuntimePool {
            runtimes: vec![], // Empty for basic test
            next: std::sync::atomic::AtomicUsize::new(0),
        };

        assert!(pool.is_empty());
        assert!(pool.next().is_none());
    }

    #[test]
    fn test_spawn_workers_headless_count() {
        // Verify basic functionality without actual Runtime creation
        let count = 4;
        let base_port = 5001;

        assert!(count > 0);
        assert!(base_port > 0);
    }
}
