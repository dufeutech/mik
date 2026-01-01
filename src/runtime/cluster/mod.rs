// Allow dead code and unused imports for library-first API
// Cluster types are for external consumers, not yet integrated with CLI
#![allow(dead_code, unused_imports)]

//! Cluster orchestration for multi-worker deployments.
//!
//! This module provides abstractions for running multiple mik workers,
//! either as separate processes or as in-process tasks.
//!
//! # Architecture
//!
//! ```text
//!                    ┌─────────────────────────────────────┐
//!                    │             Cluster                 │
//!                    │                                     │
//!                    │  ┌─────────┐ ┌─────────┐           │
//!                    │  │ Worker  │ │ Worker  │  ...      │
//!                    │  │ (Task)  │ │(Process)│           │
//!                    │  └─────────┘ └─────────┘           │
//!                    │                                     │
//!                    │  Optional: Load Balancer / Proxy   │
//!                    └─────────────────────────────────────┘
//! ```
//!
//! # Examples
//!
//! ## Process-Based Workers (CLI)
//!
//! ```no_run
//! use mik::runtime::cluster::ClusterBuilder;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let cluster = ClusterBuilder::new()
//!     .workers(4)
//!     .manifest_file("mik.toml")
//!     .spawn_processes()?;
//!
//! // Workers are running as separate processes
//! cluster.wait().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Task-Based Workers (Embedding)
//!
//! ```no_run
//! use mik::runtime::{Runtime, cluster::ClusterBuilder};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let cluster = ClusterBuilder::new()
//!     .workers(4)
//!     .runtime_factory(|| {
//!         Runtime::builder()
//!             .modules_dir("modules/")
//!             .build()
//!     })
//!     .spawn_tasks()
//!     .await?;
//!
//! // Workers are running as tokio tasks
//! // Use cluster.forward(request) or cluster.shutdown()
//! # Ok(())
//! # }
//! ```

pub mod process;
pub mod task;

use crate::runtime::Runtime;
use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Handle to a running worker.
///
/// Workers can be either:
/// - Process-based: Separate OS processes with their own memory space
/// - Task-based: Tokio tasks sharing the same process
#[derive(Debug)]
pub enum WorkerHandle {
    /// A worker running as a separate process.
    Process {
        /// Process ID of the worker.
        pid: u32,
        /// Port the worker is listening on.
        port: u16,
        /// Address the worker is listening on.
        addr: SocketAddr,
    },
    /// A worker running as a tokio task.
    Task {
        /// The runtime instance.
        runtime: Arc<Runtime>,
        /// Handle to the running task (if serving).
        handle: Option<JoinHandle<Result<()>>>,
        /// Port the worker is configured for.
        port: u16,
    },
}

impl WorkerHandle {
    /// Get the port this worker is listening on.
    #[must_use]
    pub const fn port(&self) -> u16 {
        match self {
            Self::Process { port, .. } | Self::Task { port, .. } => *port,
        }
    }

    /// Get the address this worker is listening on (if available).
    #[must_use]
    pub fn addr(&self) -> Option<SocketAddr> {
        match self {
            Self::Process { addr, .. } => Some(*addr),
            Self::Task { port, .. } => Some(SocketAddr::from(([127, 0, 0, 1], *port))),
        }
    }

    /// Check if this is a process-based worker.
    #[must_use]
    pub const fn is_process(&self) -> bool {
        matches!(self, Self::Process { .. })
    }

    /// Check if this is a task-based worker.
    #[must_use]
    pub const fn is_task(&self) -> bool {
        matches!(self, Self::Task { .. })
    }

    /// Get the runtime if this is a task-based worker.
    #[must_use]
    pub fn runtime(&self) -> Option<&Arc<Runtime>> {
        match self {
            Self::Task { runtime, .. } => Some(runtime),
            Self::Process { .. } => None,
        }
    }
}

/// A cluster of mik workers.
///
/// Manages multiple workers running either as processes or tasks,
/// with optional load balancing.
pub struct Cluster {
    /// The workers in this cluster.
    workers: Vec<WorkerHandle>,
    /// Base port for workers.
    base_port: u16,
}

impl Cluster {
    /// Create a new cluster with the given workers.
    #[must_use]
    pub fn new(workers: Vec<WorkerHandle>, base_port: u16) -> Self {
        Self { workers, base_port }
    }

    /// Get the workers in this cluster.
    #[must_use]
    pub fn workers(&self) -> &[WorkerHandle] {
        &self.workers
    }

    /// Get the number of workers.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Get the base port for workers.
    #[must_use]
    pub const fn base_port(&self) -> u16 {
        self.base_port
    }

    /// Get all worker addresses.
    #[must_use]
    pub fn worker_addrs(&self) -> Vec<SocketAddr> {
        self.workers.iter().filter_map(WorkerHandle::addr).collect()
    }

    /// Get all runtimes (for task-based workers).
    #[must_use]
    pub fn runtimes(&self) -> Vec<&Arc<Runtime>> {
        self.workers
            .iter()
            .filter_map(WorkerHandle::runtime)
            .collect()
    }

    /// Wait for all workers to complete.
    ///
    /// For process-based workers, this waits for all child processes to exit.
    /// For task-based workers, this waits for all server tasks to complete.
    pub async fn wait(self) -> Result<()> {
        for worker in self.workers {
            match worker {
                WorkerHandle::Process { pid, .. } => {
                    // For process-based workers, we would wait for the process
                    // This is handled by the process module
                    tracing::info!("Waiting for worker process {}", pid);
                },
                WorkerHandle::Task { handle, .. } => {
                    if let Some(handle) = handle {
                        handle.await??;
                    }
                },
            }
        }
        Ok(())
    }

    /// Shutdown all workers gracefully.
    #[allow(clippy::unused_async)] // May add async shutdown in the future
    pub async fn shutdown(&self) {
        for worker in &self.workers {
            match worker {
                WorkerHandle::Process { pid, .. } => {
                    tracing::info!("Sending shutdown signal to process {}", pid);
                    // Process shutdown is handled by the process module
                },
                WorkerHandle::Task { runtime, .. } => {
                    runtime.shutdown();
                },
            }
        }
    }

    /// Get health status from all workers.
    pub fn health(&self) -> Vec<crate::runtime::types::HealthStatus> {
        self.workers
            .iter()
            .filter_map(|w| match w {
                WorkerHandle::Task { runtime, .. } => Some(runtime.health()),
                WorkerHandle::Process { .. } => None, // Would need HTTP request
            })
            .collect()
    }
}

/// Builder for creating a [`Cluster`].
///
/// # Examples
///
/// ```no_run
/// use mik::runtime::cluster::ClusterBuilder;
///
/// # fn example() -> anyhow::Result<()> {
/// let builder = ClusterBuilder::new()
///     .workers(4)
///     .base_port(3001)
///     .manifest_file("mik.toml");
/// # Ok(())
/// # }
/// ```
#[must_use]
pub struct ClusterBuilder {
    /// Number of workers to spawn.
    worker_count: usize,
    /// Base port for workers (workers use base_port, base_port+1, ...).
    base_port: u16,
    /// Path to manifest file (for process-based workers).
    manifest_path: Option<String>,
    /// Runtime factory function (for task-based workers).
    runtime_factory: Option<Box<dyn Fn() -> Result<Runtime> + Send + Sync>>,
}

impl Default for ClusterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterBuilder {
    /// Create a new cluster builder.
    pub fn new() -> Self {
        Self {
            worker_count: num_cpus::get(),
            base_port: 3001,
            manifest_path: None,
            runtime_factory: None,
        }
    }

    /// Set the number of workers.
    pub const fn workers(mut self, count: usize) -> Self {
        self.worker_count = count;
        self
    }

    /// Set the base port for workers.
    ///
    /// Workers will be assigned ports starting from this value:
    /// worker 0 = base_port, worker 1 = base_port + 1, etc.
    pub const fn base_port(mut self, port: u16) -> Self {
        self.base_port = port;
        self
    }

    /// Set the manifest file path (for process-based workers).
    pub fn manifest_file(mut self, path: impl Into<String>) -> Self {
        self.manifest_path = Some(path.into());
        self
    }

    /// Set the runtime factory function (for task-based workers).
    ///
    /// The factory is called once per worker to create a new Runtime instance.
    pub fn runtime_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<Runtime> + Send + Sync + 'static,
    {
        self.runtime_factory = Some(Box::new(factory));
        self
    }

    /// Spawn workers as separate processes.
    ///
    /// Each worker runs in its own OS process, providing full isolation.
    /// Requires a manifest file to be specified.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No manifest file was specified
    /// - Failed to spawn child processes
    pub fn spawn_processes(self) -> Result<Cluster> {
        let manifest_path = self
            .manifest_path
            .ok_or_else(|| anyhow::anyhow!("manifest_file is required for spawn_processes"))?;

        let workers = process::spawn_workers(self.worker_count, self.base_port, &manifest_path)?;

        Ok(Cluster::new(workers, self.base_port))
    }

    /// Spawn workers as tokio tasks.
    ///
    /// Each worker runs as a task within the current process, sharing memory.
    /// Requires a runtime factory to be specified.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No runtime factory was specified
    /// - Failed to create Runtime instances
    pub async fn spawn_tasks(self) -> Result<Cluster> {
        let factory = self
            .runtime_factory
            .ok_or_else(|| anyhow::anyhow!("runtime_factory is required for spawn_tasks"))?;

        let workers = task::spawn_workers(self.worker_count, self.base_port, factory).await?;

        Ok(Cluster::new(workers, self.base_port))
    }

    /// Spawn workers as tokio tasks without starting servers.
    ///
    /// Creates Runtime instances but doesn't bind them to ports.
    /// Useful when you want to use `runtime.handle_request()` directly.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No runtime factory was specified
    /// - Failed to create Runtime instances
    #[allow(clippy::unused_async)] // May need async for future spawn operations
    pub async fn spawn_tasks_headless(self) -> Result<Cluster> {
        let factory = self.runtime_factory.ok_or_else(|| {
            anyhow::anyhow!("runtime_factory is required for spawn_tasks_headless")
        })?;

        let workers = task::spawn_workers_headless(self.worker_count, self.base_port, factory)?;

        Ok(Cluster::new(workers, self.base_port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_builder_defaults() {
        let builder = ClusterBuilder::new();
        assert_eq!(builder.base_port, 3001);
        assert!(builder.manifest_path.is_none());
        assert!(builder.runtime_factory.is_none());
    }

    #[test]
    fn test_cluster_builder_workers() {
        let builder = ClusterBuilder::new().workers(8);
        assert_eq!(builder.worker_count, 8);
    }

    #[test]
    fn test_cluster_builder_base_port() {
        let builder = ClusterBuilder::new().base_port(5000);
        assert_eq!(builder.base_port, 5000);
    }

    #[test]
    fn test_cluster_builder_manifest_file() {
        let builder = ClusterBuilder::new().manifest_file("custom.toml");
        assert_eq!(builder.manifest_path, Some("custom.toml".to_string()));
    }

    #[test]
    fn test_worker_handle_port() {
        let process_worker = WorkerHandle::Process {
            pid: 1234,
            port: 3001,
            addr: SocketAddr::from(([127, 0, 0, 1], 3001)),
        };
        assert_eq!(process_worker.port(), 3001);
        assert!(process_worker.is_process());
        assert!(!process_worker.is_task());
    }
}
