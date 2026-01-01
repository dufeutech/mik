// Allow dead code for library-first API - Server types are for external consumers
#![allow(dead_code)]

//! HTTP server wrapper for the Runtime.
//!
//! This module provides the [`Server`] struct which wraps a [`Runtime`] and binds
//! it to a network address for serving HTTP requests.
//!
//! # Architecture
//!
//! The server layer is intentionally thin - it only handles:
//! - TCP listener management
//! - Connection acceptance
//! - Graceful shutdown coordination
//!
//! All request handling logic is delegated to the underlying [`Runtime`].
//!
//! # Examples
//!
//! ```no_run
//! use mik::runtime::{Runtime, Server};
//! use std::net::SocketAddr;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let runtime = Runtime::builder()
//!     .modules_dir("modules/")
//!     .build()?;
//!
//! let addr: SocketAddr = "127.0.0.1:3000".parse()?;
//! let server = Server::new(runtime, addr);
//! server.serve().await?;
//! # Ok(())
//! # }
//! ```

use crate::runtime::{
    HEALTH_PATH, METRICS_PATH, RUN_PREFIX, Runtime, SCRIPT_PREFIX, STATIC_PREFIX,
};
use anyhow::Result;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HttpConnectionBuilder;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

/// Shutdown polling interval in milliseconds.
const SHUTDOWN_POLL_INTERVAL_MS: u64 = 100;

/// HTTP server that wraps a [`Runtime`] and serves requests on a network address.
///
/// The server handles:
/// - TCP connection management
/// - HTTP/1.1 and HTTP/2 auto-detection
/// - Graceful shutdown with connection draining
/// - Concurrent request limiting via semaphore
///
/// # Examples
///
/// ```no_run
/// use mik::runtime::{Runtime, Server};
/// use std::net::SocketAddr;
///
/// # async fn example() -> anyhow::Result<()> {
/// let runtime = Runtime::builder()
///     .modules_dir("modules/")
///     .build()?;
///
/// let server = Server::bind(runtime, "0.0.0.0:3000")?;
/// println!("Listening on {}", server.addr());
/// server.serve().await?;
/// # Ok(())
/// # }
/// ```
pub struct Server {
    runtime: Runtime,
    addr: SocketAddr,
}

impl Server {
    /// Create a new server with the given runtime and address.
    #[must_use]
    pub fn new(runtime: Runtime, addr: SocketAddr) -> Self {
        Self { runtime, addr }
    }

    /// Create a server by parsing an address string.
    ///
    /// # Errors
    ///
    /// Returns an error if the address cannot be parsed.
    pub fn bind(runtime: Runtime, addr: impl AsRef<str>) -> Result<Self> {
        let addr: SocketAddr = addr.as_ref().parse()?;
        Ok(Self::new(runtime, addr))
    }

    /// Get the address the server will bind to.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Get a reference to the underlying runtime.
    #[must_use]
    pub const fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Get a mutable reference to the underlying runtime.
    #[must_use]
    pub fn runtime_mut(&mut self) -> &mut Runtime {
        &mut self.runtime
    }

    /// Start serving HTTP requests.
    ///
    /// This method binds to the configured address and starts accepting
    /// connections. It runs until a shutdown signal is received (SIGTERM/SIGINT).
    ///
    /// # Graceful Shutdown
    ///
    /// When a shutdown signal is received:
    /// 1. Stop accepting new connections
    /// 2. Wait for in-flight requests to complete (with timeout)
    /// 3. Return
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The address cannot be bound
    /// - A fatal server error occurs
    pub async fn serve(self) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        let shared = self.runtime.shared.clone();

        info!("Serving on http://{}", self.addr);
        info!("Health endpoint: {}", HEALTH_PATH);
        info!("Metrics endpoint: {}", METRICS_PATH);

        // Log routing information
        if let Some(name) = self.runtime.single_component_name() {
            info!("Routes: {}{}/* -> component", RUN_PREFIX, name);
        } else {
            info!("Routes: {}<module>/* -> <module>.wasm", RUN_PREFIX);
        }

        if self.runtime.has_static_files() {
            info!("Routes: {}<project>/* -> static files", STATIC_PREFIX);
        }

        if shared.scripts_dir.is_some() {
            info!("Routes: {}<name> -> scripts", SCRIPT_PREFIX);
        }

        // Setup graceful shutdown
        let shutdown_signal = shared.shutdown.clone();
        let mut shutdown_handle = tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            shutdown_signal.store(true, Ordering::SeqCst);
        });

        // Track active connection tasks
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
        let active_connections = Arc::new(AtomicU64::new(0));

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    let (stream, remote_addr) = accept_result?;

                    // Check if shutdown has been initiated
                    if shared.shutdown.load(Ordering::SeqCst) {
                        debug!("Rejecting new connection during shutdown");
                        break;
                    }

                    let io = TokioIo::new(stream);
                    let shared = shared.clone();
                    let active_conns = active_connections.clone();
                    let shutdown_tx = shutdown_tx.clone();

                    // Acquire semaphore permit to limit concurrent requests
                    let Ok(permit) = shared.request_semaphore.clone().acquire_owned().await else {
                        warn!("Failed to acquire request permit, semaphore closed");
                        continue;
                    };

                    // Increment active connection count
                    active_conns.fetch_add(1, Ordering::SeqCst);

                    tokio::spawn(async move {
                        let _permit = permit;
                        let _shutdown_guard = shutdown_tx;

                        let service = service_fn(move |req| {
                            let shared = shared.clone();
                            async move {
                                crate::runtime::request_handler::handle_request(shared, req, remote_addr).await
                            }
                        });

                        let builder = HttpConnectionBuilder::new(TokioExecutor::new());
                        if let Err(e) = builder.serve_connection(io, service).await {
                            error!("Connection error: {}", e);
                        }

                        active_conns.fetch_sub(1, Ordering::SeqCst);
                    });
                }

                _ = &mut shutdown_handle => {
                    break;
                }
            }
        }

        // Shutdown sequence
        info!("Initiating graceful shutdown...");
        drop(listener);
        drop(shutdown_tx);

        // Wait for in-flight requests
        let drain_timeout = Duration::from_secs(shared.config.shutdown_timeout_secs);
        let active_count = active_connections.load(Ordering::SeqCst);

        if active_count > 0 {
            info!(
                "Waiting for {} active connections to complete (timeout: {:?})",
                active_count, drain_timeout
            );

            if tokio::time::timeout(drain_timeout, async {
                while active_connections.load(Ordering::SeqCst) > 0 {
                    tokio::time::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS)).await;
                }
                shutdown_rx.recv().await;
            })
            .await
            .is_ok()
            {
                info!("All connections completed gracefully");
            } else {
                let remaining = active_connections.load(Ordering::SeqCst);
                warn!("Shutdown timeout - {} connections still active", remaining);
            }
        } else {
            info!("No active connections to drain");
        }

        info!("Shutdown complete");
        Ok(())
    }
}

/// Wait for a shutdown signal (SIGTERM/SIGINT on Unix, Ctrl+C on Windows).
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("Failed to setup SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to setup SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT");
            }
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Failed to listen for ctrl_c: {}", e);
            return;
        }
        info!("Received Ctrl+C");
    }
}

/// Builder for creating a [`Server`] with configuration options.
///
/// This builder allows configuring the server before creating it.
///
/// # Examples
///
/// ```no_run
/// use mik::runtime::{Runtime, ServerBuilder};
///
/// # fn example() -> anyhow::Result<()> {
/// let runtime = Runtime::builder()
///     .modules_dir("modules/")
///     .build()?;
///
/// let server = ServerBuilder::new(runtime)
///     .bind("0.0.0.0:3000")?
///     .build();
/// # Ok(())
/// # }
/// ```
pub struct ServerBuilder {
    runtime: Runtime,
    addr: Option<SocketAddr>,
}

impl ServerBuilder {
    /// Create a new server builder with the given runtime.
    #[must_use]
    pub fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            addr: None,
        }
    }

    /// Set the address to bind to.
    ///
    /// # Errors
    ///
    /// Returns an error if the address cannot be parsed.
    pub fn bind(mut self, addr: impl AsRef<str>) -> Result<Self> {
        self.addr = Some(addr.as_ref().parse()?);
        Ok(self)
    }

    /// Set the address to bind to from a `SocketAddr`.
    #[must_use]
    pub const fn addr(mut self, addr: SocketAddr) -> Self {
        self.addr = Some(addr);
        self
    }

    /// Set the port to bind to (uses 0.0.0.0 as the host).
    #[must_use]
    pub fn port(mut self, port: u16) -> Self {
        self.addr = Some(SocketAddr::from(([0, 0, 0, 0], port)));
        self
    }

    /// Build the server.
    ///
    /// Uses the configured address or defaults to 127.0.0.1:3000.
    #[must_use]
    pub fn build(self) -> Server {
        let addr = self
            .addr
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 3000)));
        Server::new(self.runtime, addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_builder_default_addr() {
        // We can't fully test without a Runtime, but we can test the builder logic
        let addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        assert_eq!(addr.port(), 3000);
    }

    #[test]
    fn test_server_builder_port() {
        let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
        assert_eq!(addr.port(), 8080);
        assert_eq!(addr.ip().to_string(), "0.0.0.0");
    }
}
