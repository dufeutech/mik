//! HTTP API server for remote daemon management.
//!
//! Provides a REST API for managing WASM instances and embedded services,
//! mirroring CLI functionality for programmatic access.
//!
//! ## Endpoints
//!
//! ### Instance Management
//! - `GET /instances` - List all instances
//! - `POST /instances` - Start new instance
//! - `GET /instances/:name` - Get instance details
//! - `DELETE /instances/:name` - Stop instance
//! - `POST /instances/:name/restart` - Restart instance
//! - `GET /instances/:name/logs` - Get instance logs
//!
//! ### KV Service (`/kv`)
//! - `GET /kv/:key` - Get value
//! - `PUT /kv/:key` - Set value (with optional TTL)
//! - `DELETE /kv/:key` - Delete key
//! - `GET /kv` - List keys (with optional prefix)
//!
//! ### SQL Service (`/sql`)
//! - `POST /sql/query` - Execute SELECT query
//! - `POST /sql/execute` - Execute INSERT/UPDATE/DELETE
//! - `POST /sql/batch` - Execute batch of statements
//!
//! ### Storage Service (`/storage`)
//! - `GET /storage/*path` - Get object
//! - `PUT /storage/*path` - Put object
//! - `DELETE /storage/*path` - Delete object
//! - `HEAD /storage/*path` - Get object metadata
//! - `GET /storage` - List objects (with optional prefix)
//!
//! ### Cron Scheduler (`/cron`)
//! - `GET /cron` - List all scheduled jobs
//! - `POST /cron` - Create a new scheduled job
//! - `GET /cron/:name` - Get job details
//! - `PATCH /cron/:name` - Update job (pause/resume)
//! - `DELETE /cron/:name` - Delete a scheduled job
//! - `POST /cron/:name/trigger` - Manually trigger a job
//! - `GET /cron/:name/history` - Get job execution history
//!
//! ### Observability
//! - `GET /metrics` - Prometheus metrics
//!
//! ### System
//! - `GET /health` - Health check
//! - `GET /version` - Version info
//!
//! ## Authentication
//!
//! API key authentication is optional. To enable it, set the `MIK_API_KEY`
//! environment variable. When enabled, all requests (except `/health` and
//! `/metrics`) must include an `X-API-Key` header with the matching key.
//!
//! ```bash
//! # Enable API key authentication
//! export MIK_API_KEY="your-secret-key"
//! mik daemon
//!
//! # Make authenticated requests
//! curl -H "X-API-Key: your-secret-key" http://localhost:9090/instances
//! ```
//!
//! Exempt endpoints (for monitoring/health checks):
//! - `/health` - Always accessible
//! - `/metrics` - Always accessible for Prometheus scraping

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, head, post, put},
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::daemon::config::DaemonConfig;
use crate::daemon::cron::CronScheduler;
use crate::daemon::metrics;
#[cfg(feature = "otlp")]
use crate::daemon::otlp;
use crate::daemon::process::{self, SpawnConfig};
use crate::daemon::services::{kv::KvStore, sql::SqlService, storage::StorageService};
use crate::daemon::state::{Instance, StateStore, Status};

pub mod handlers;
pub mod types;

// Re-export all types for backward compatibility
pub use types::*;

// Use handlers from the handlers module
use handlers::{
    // Cron
    cron_create,
    cron_delete,
    cron_get,
    cron_history,
    cron_list,
    cron_trigger,
    cron_update,
    // Instances
    get_instance,
    get_logs,
    health,
    // KV
    kv_delete,
    kv_get,
    kv_list,
    kv_set,
    list_instances,
    restart_instance,
    // SQL
    sql_batch,
    sql_execute,
    sql_query,
    start_instance,
    stop_instance,
    // Storage
    storage_delete,
    storage_get,
    storage_head,
    storage_list,
    storage_put,
    version,
};

// =============================================================================
// App State
// =============================================================================

/// Shared application state for HTTP handlers.
pub(crate) struct AppState {
    store: StateStore,
    kv: Option<KvStore>,
    sql: Option<SqlService>,
    storage: Option<StorageService>,
    cron: CronScheduler,
}

type SharedState = Arc<RwLock<AppState>>;

// Types moved to types.rs

// =============================================================================
// HTTP API Server
// =============================================================================

/// Starts the HTTP API server on the specified port.
///
/// The server provides REST endpoints for managing WASM instances and
/// embedded services. See module-level documentation for endpoint details.
///
/// Services can be selectively enabled/disabled via `DaemonConfig`.
/// Disabled services return 503 Service Unavailable.
pub async fn serve(port: u16, state_path: PathBuf, config: DaemonConfig) -> Result<()> {
    use crate::daemon::services::get_data_dir;

    let data_dir = get_data_dir().context("Failed to get data directory")?;

    // Initialize Prometheus metrics
    let _metrics_handle = metrics::init_metrics();
    tracing::info!("Prometheus metrics initialized");

    // Log API key auth status
    if API_KEY.is_some() {
        tracing::info!("API key authentication enabled (MIK_API_KEY is set)");
    } else {
        tracing::debug!("API key authentication disabled (MIK_API_KEY not set)");
    }

    let store = StateStore::open(&state_path).context("Failed to open state database")?;

    // Initialize embedded services (conditionally based on config)
    let kv = if config.services.kv_enabled {
        Some(KvStore::open(data_dir.join("kv.redb")).context("Failed to open KV store")?)
    } else {
        tracing::info!("KV service disabled by configuration");
        None
    };

    let sql = if config.services.sql_enabled {
        Some(SqlService::open(data_dir.join("sql.db")).context("Failed to open SQL database")?)
    } else {
        tracing::info!("SQL service disabled by configuration");
        None
    };

    let storage = if config.services.storage_enabled {
        Some(
            StorageService::open(data_dir.join("storage"))
                .context("Failed to open storage service")?,
        )
    } else {
        tracing::info!("Storage service disabled by configuration");
        None
    };

    // Initialize cron scheduler
    let cron = CronScheduler::new()
        .await
        .context("Failed to create cron scheduler")?;

    // Load persisted cron jobs from database
    let persisted_jobs = store
        .list_cron_jobs_async()
        .await
        .context("Failed to load persisted cron jobs")?;

    for config in persisted_jobs {
        match cron.add_job(config.clone()).await {
            Ok(()) => tracing::info!(name = %config.name, "Restored cron job from database"),
            Err(e) => tracing::warn!(name = %config.name, error = %e, "Failed to restore cron job"),
        }
    }

    cron.start()
        .await
        .context("Failed to start cron scheduler")?;
    tracing::info!("Cron scheduler started");

    let app_state = Arc::new(RwLock::new(AppState {
        store,
        kv,
        sql,
        storage,
        cron,
    }));

    // Clone state for shutdown handler
    let shutdown_state = Arc::clone(&app_state);

    let app = Router::new()
        // Instance management
        .route("/instances", get(list_instances))
        .route("/instances", post(start_instance))
        .route("/instances/{name}", get(get_instance))
        .route("/instances/{name}", delete(stop_instance))
        .route("/instances/{name}/restart", post(restart_instance))
        .route("/instances/{name}/logs", get(get_logs))
        // KV service
        .route("/kv", get(kv_list))
        .route("/kv/{key}", get(kv_get))
        .route("/kv/{key}", put(kv_set))
        .route("/kv/{key}", delete(kv_delete))
        // SQL service
        .route("/sql/query", post(sql_query))
        .route("/sql/execute", post(sql_execute))
        .route("/sql/batch", post(sql_batch))
        // Storage service
        .route("/storage", get(storage_list))
        .route("/storage/{*path}", get(storage_get))
        .route("/storage/{*path}", put(storage_put))
        .route("/storage/{*path}", delete(storage_delete))
        .route("/storage/{*path}", head(storage_head))
        // Cron scheduler
        .route("/cron", get(cron_list).post(cron_create))
        .route(
            "/cron/{name}",
            get(cron_get).patch(cron_update).delete(cron_delete),
        )
        .route("/cron/{name}/trigger", post(cron_trigger))
        .route("/cron/{name}/history", get(cron_history))
        // Observability
        .route("/metrics", get(metrics_endpoint))
        // System endpoints
        .route("/health", get(health))
        .route("/version", get(version))
        .with_state(app_state)
        // Request body size limit (10MB) - prevents DoS via large payloads
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        // API key authentication (optional, set MIK_API_KEY env var to enable)
        .layer(middleware::from_fn(api_key_auth_middleware))
        // Metrics middleware - records HTTP request metrics
        .layer(middleware::from_fn(metrics_middleware));

    // Use HOST env var if set (for Docker), otherwise default to 0.0.0.0
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)));

    tracing::info!("Starting mik daemon HTTP API on {}", addr);

    // Create shutdown signal for health check task
    let (health_check_tx, health_check_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn instance health check background task
    let health_check_state = Arc::clone(&shutdown_state);
    let health_check_handle = tokio::spawn(async move {
        instance_health_check_task(health_check_state, health_check_rx).await;
    });

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server error")?;

    // Signal health check task to stop
    let _ = health_check_tx.send(());
    let _ = health_check_handle.await;

    // Graceful shutdown: stop all running instances
    graceful_shutdown(shutdown_state).await;

    Ok(())
}

/// Graceful shutdown signal handler.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("Shutdown signal received, stopping daemon...");
}

/// Performs graceful shutdown by stopping all running instances and the cron scheduler.
async fn graceful_shutdown(state: SharedState) {
    tracing::info!("Starting graceful shutdown...");

    // Extract the store first to avoid holding the lock during blocking operations
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    // Get all running instances using async method
    let instances = match store.list_instances_async().await {
        Ok(instances) => instances,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list instances during shutdown");
            return;
        },
    };

    // Stop each running instance
    let mut stopped_count = 0;
    for instance in instances {
        if instance.status == Status::Running {
            // Check if process is actually running
            match process::is_running(instance.pid) {
                Ok(true) => {
                    if let Err(e) = process::kill_instance(instance.pid) {
                        tracing::warn!(
                            instance = %instance.name,
                            pid = instance.pid,
                            error = %e,
                            "Failed to stop instance"
                        );
                    } else {
                        tracing::info!(
                            instance = %instance.name,
                            pid = instance.pid,
                            "Stopped instance"
                        );
                        stopped_count += 1;
                    }

                    // Update instance status in database using async method
                    let mut updated = instance.clone();
                    updated.status = Status::Stopped;
                    if let Err(e) = store.save_instance_async(updated).await {
                        tracing::warn!(
                            instance = %instance.name,
                            error = %e,
                            "Failed to update instance status"
                        );
                    }
                },
                Ok(false) => {
                    // Process already dead, just update status
                    let mut updated = instance.clone();
                    updated.status = Status::Stopped;
                    let _ = store.save_instance_async(updated).await;
                },
                Err(e) => {
                    tracing::warn!(
                        instance = %instance.name,
                        error = %e,
                        "Failed to check instance status"
                    );
                },
            }
        }
    }

    if stopped_count > 0 {
        tracing::info!(count = stopped_count, "Stopped running instances");
    }

    // Stop the cron scheduler
    {
        let mut state = state.write().await;
        if let Err(e) = state.cron.shutdown().await {
            tracing::warn!(error = %e, "Failed to stop cron scheduler");
        }
    }

    // Flush OTLP traces before exit
    #[cfg(feature = "otlp")]
    otlp::shutdown();

    tracing::info!("Graceful shutdown complete");
}

/// Maximum number of auto-restarts before giving up.
const MAX_AUTO_RESTARTS: u32 = 10;

/// Base backoff delay in seconds for auto-restart.
const BASE_BACKOFF_SECS: u64 = 5;

/// Maximum backoff delay in seconds.
const MAX_BACKOFF_SECS: u64 = 300; // 5 minutes

/// Health check interval in seconds.
const HEALTH_CHECK_INTERVAL_SECS: u64 = 10;

/// Background task that monitors instance health and auto-restarts crashed instances.
async fn instance_health_check_task(
    state: SharedState,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    tracing::info!("Instance health check task started");

    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                check_and_recover_instances(&state).await;
            }
            _ = &mut shutdown_rx => {
                tracing::info!("Health check task shutting down");
                break;
            }
        }
    }
}

/// Checks all running instances and attempts to recover crashed ones.
async fn check_and_recover_instances(state: &SharedState) {
    // Extract the store first to avoid holding the lock during blocking operations
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    // Get all instances using async method
    let instances = match store.list_instances_async().await {
        Ok(instances) => instances,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list instances for health check");
            return;
        },
    };

    for instance in instances {
        check_and_recover_single_instance(&store, instance).await;
    }
}

/// Check a single instance and attempt recovery if crashed.
async fn check_and_recover_single_instance(store: &StateStore, instance: Instance) {
    // Only check instances marked as Running
    if instance.status != Status::Running {
        return;
    }

    // Check if process is actually running
    let is_running = match process::is_running(instance.pid) {
        Ok(running) => running,
        Err(e) => {
            tracing::warn!(
                instance = %instance.name,
                pid = instance.pid,
                error = %e,
                "Failed to check process status"
            );
            return;
        },
    };

    if is_running {
        return; // Instance is healthy
    }

    // Instance crashed - handle recovery
    handle_crashed_instance(store, instance).await;
}

/// Handle a crashed instance: update status and attempt auto-restart if enabled.
async fn handle_crashed_instance(store: &StateStore, instance: Instance) {
    tracing::warn!(
        instance = %instance.name,
        pid = instance.pid,
        "Instance crashed (process not running)"
    );

    // Update status to Crashed using async method
    let mut crashed_instance = instance.clone();
    crashed_instance.status = Status::Crashed { exit_code: -1 };

    if let Err(e) = store.save_instance_async(crashed_instance).await {
        tracing::error!(
            instance = %instance.name,
            error = %e,
            "Failed to update instance status to crashed"
        );
        return;
    }

    // Check if auto-restart is enabled
    if !instance.auto_restart {
        tracing::info!(
            instance = %instance.name,
            "Auto-restart disabled, not recovering"
        );
        return;
    }

    // Attempt auto-restart with backoff
    attempt_auto_restart(store, &instance).await;
}

/// Attempt to auto-restart a crashed instance with exponential backoff.
async fn attempt_auto_restart(store: &StateStore, instance: &Instance) {
    // Check if we've exceeded max restarts
    if instance.restart_count >= MAX_AUTO_RESTARTS {
        tracing::error!(
            instance = %instance.name,
            restart_count = instance.restart_count,
            "Max auto-restarts ({}) exceeded, giving up",
            MAX_AUTO_RESTARTS
        );
        return;
    }

    // Calculate backoff delay (exponential with cap)
    let backoff_secs = std::cmp::min(
        BASE_BACKOFF_SECS * 2u64.pow(instance.restart_count),
        MAX_BACKOFF_SECS,
    );

    // Check if enough time has passed since last restart
    if !should_restart_now(instance, backoff_secs) {
        return;
    }

    // Attempt restart
    tracing::info!(
        instance = %instance.name,
        restart_count = instance.restart_count + 1,
        backoff_secs = backoff_secs,
        "Attempting auto-restart"
    );

    spawn_and_save_restarted_instance(store, instance).await;
}

/// Check if enough time has passed since the last restart attempt.
fn should_restart_now(instance: &Instance, backoff_secs: u64) -> bool {
    if let Some(last_restart) = instance.last_restart_at {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(last_restart)
            .num_seconds();
        #[allow(clippy::cast_possible_wrap)]
        let backoff_secs_i64 = backoff_secs as i64;
        if elapsed < backoff_secs_i64 {
            tracing::debug!(
                instance = %instance.name,
                backoff_remaining = backoff_secs_i64 - elapsed,
                "Waiting for backoff before restart"
            );
            return false;
        }
    }
    true
}

/// Spawn a new instance process and save the updated state.
async fn spawn_and_save_restarted_instance(store: &StateStore, instance: &Instance) {
    let spawn_config = SpawnConfig {
        name: instance.name.clone(),
        port: instance.port,
        config_path: instance.config.clone(),
        working_dir: instance
            .config
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf(),
        hot_reload: false,
    };

    match process::spawn_instance(&spawn_config) {
        Ok(info) => {
            let restarted = Instance {
                name: instance.name.clone(),
                port: instance.port,
                pid: info.pid,
                status: Status::Running,
                config: instance.config.clone(),
                started_at: chrono::Utc::now(),
                modules: instance.modules.clone(),
                auto_restart: true,
                restart_count: instance.restart_count + 1,
                last_restart_at: Some(chrono::Utc::now()),
            };

            // Use async method
            if let Err(e) = store.save_instance_async(restarted.clone()).await {
                tracing::error!(
                    instance = %instance.name,
                    error = %e,
                    "Failed to save restarted instance state"
                );
            } else {
                tracing::info!(
                    instance = %instance.name,
                    pid = info.pid,
                    restart_count = restarted.restart_count,
                    "Instance auto-restarted successfully"
                );
            }
        },
        Err(e) => {
            tracing::error!(
                instance = %instance.name,
                error = %e,
                "Failed to auto-restart instance"
            );
        },
    }
}

/// API key for optional authentication.
///
/// Loaded once at startup from `MIK_API_KEY` environment variable.
/// If not set, authentication is disabled (backwards compatible).
static API_KEY: std::sync::LazyLock<Option<String>> =
    std::sync::LazyLock::new(|| std::env::var("MIK_API_KEY").ok().filter(|k| !k.is_empty()));

/// Middleware for optional API key authentication.
///
/// If `MIK_API_KEY` environment variable is set, requires requests to include
/// a matching `X-API-Key` header. The following endpoints are exempt:
/// - `/health` - Health checks (for load balancers/monitoring)
/// - `/metrics` - Prometheus metrics (for scraping)
///
/// If `MIK_API_KEY` is not set, all requests pass through (backwards compatible).
async fn api_key_auth_middleware(request: Request, next: Next) -> Response {
    // If no API key configured, skip authentication
    let Some(expected_key) = API_KEY.as_ref() else {
        return next.run(request).await;
    };

    // Exempt health and metrics endpoints from auth
    let path = request.uri().path();
    if path == "/health" || path == "/metrics" {
        return next.run(request).await;
    }

    // Check for X-API-Key header
    let provided_key = request
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok());

    match provided_key {
        Some(key) if key == expected_key => next.run(request).await,
        Some(_) => {
            tracing::warn!(path = %path, "API key authentication failed: invalid key");
            (StatusCode::UNAUTHORIZED, "Invalid API key").into_response()
        },
        None => {
            tracing::warn!(path = %path, "API key authentication failed: missing header");
            (StatusCode::UNAUTHORIZED, "Missing X-API-Key header").into_response()
        },
    }
}

/// Middleware to record HTTP request metrics.
async fn metrics_middleware(request: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = request.method().to_string();
    let path = request.uri().path().to_string();

    let response = next.run(request).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16();

    // Record HTTP metrics
    metrics::record_http_request(&method, &path, status, duration);

    response
}

// =============================================================================
// Metrics Handler
// =============================================================================

/// GET /metrics - Prometheus metrics endpoint.
async fn metrics_endpoint() -> impl IntoResponse {
    let body = metrics::render_metrics();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

// =============================================================================
// Error Handling
// =============================================================================

/// Application error types for HTTP responses.
pub(crate) enum AppError {
    NotFound(String),
    BadRequest(String),
    Conflict(String),
    Internal(String),
    ServiceUnavailable(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Conflict(msg) => (StatusCode::CONFLICT, msg),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            Self::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg),
        };

        (status, Json(ErrorResponse { error: message })).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

// =============================================================================
// Helpers
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    // Note: format_duration tests are in utils.rs

    /// Create a test router with all service routes.
    async fn create_test_app() -> Router {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Leak the temp_dir so it lives for the duration of tests
        // In real tests, we'd use a proper fixture
        let data_dir: &'static std::path::Path = Box::leak(Box::new(data_dir));

        let store = StateStore::open(data_dir.join("state.redb")).unwrap();
        let kv = Some(KvStore::open(data_dir.join("kv.redb")).unwrap());
        let sql = Some(SqlService::open(data_dir.join("sql.db")).unwrap());
        let storage = Some(StorageService::open(data_dir.join("storage")).unwrap());
        let cron = CronScheduler::new().await.unwrap();

        let app_state = Arc::new(RwLock::new(AppState {
            store,
            kv,
            sql,
            storage,
            cron,
        }));

        Router::new()
            // KV service
            .route("/kv", get(kv_list))
            .route("/kv/{key}", get(kv_get))
            .route("/kv/{key}", put(kv_set))
            .route("/kv/{key}", delete(kv_delete))
            // SQL service
            .route("/sql/query", post(sql_query))
            .route("/sql/execute", post(sql_execute))
            .route("/sql/batch", post(sql_batch))
            // Storage service
            .route("/storage", get(storage_list))
            .route("/storage/{*path}", get(storage_get))
            .route("/storage/{*path}", put(storage_put))
            .route("/storage/{*path}", delete(storage_delete))
            .route("/storage/{*path}", head(storage_head))
            // System endpoints
            .route("/health", get(health))
            .route("/version", get(version))
            .with_state(app_state)
    }

    // =========================================================================
    // Health & Version Tests
    // =========================================================================

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = create_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.status, "healthy");
    }

    #[tokio::test]
    async fn test_version_endpoint() {
        let app = create_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let version: VersionResponse = serde_json::from_slice(&body).unwrap();
        assert!(!version.version.is_empty());
    }

    // =========================================================================
    // KV Service Tests
    // =========================================================================

    #[tokio::test]
    async fn test_kv_set_and_get() {
        let app = create_test_app().await;

        // Set a value
        let set_request = Request::builder()
            .method(Method::PUT)
            .uri("/kv/test-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"value": "test-value"}"#))
            .unwrap();

        let response = app.clone().oneshot(set_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Get the value
        let get_request = Request::builder()
            .uri("/kv/test-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let kv_response: KvGetResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(kv_response.key, "test-key");
        assert_eq!(kv_response.value, "test-value");
    }

    #[tokio::test]
    async fn test_kv_get_not_found() {
        let app = create_test_app().await;

        let request = Request::builder()
            .uri("/kv/nonexistent-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_kv_delete() {
        let app = create_test_app().await;

        // First set a value
        let set_request = Request::builder()
            .method(Method::PUT)
            .uri("/kv/delete-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"value": "to-delete"}"#))
            .unwrap();

        let response = app.clone().oneshot(set_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Delete the key
        let delete_request = Request::builder()
            .method(Method::DELETE)
            .uri("/kv/delete-key")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(delete_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let get_request = Request::builder()
            .uri("/kv/delete-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_kv_list_keys() {
        let app = create_test_app().await;

        // Set multiple keys
        for key in ["prefix:a", "prefix:b", "other:c"] {
            let set_request = Request::builder()
                .method(Method::PUT)
                .uri(format!("/kv/{key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value": "test"}"#))
                .unwrap();

            app.clone().oneshot(set_request).await.unwrap();
        }

        // List all keys
        let request = Request::builder().uri("/kv").body(Body::empty()).unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_response: KvListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list_response.keys.len(), 3);

        // List with prefix filter
        let request = Request::builder()
            .uri("/kv?prefix=prefix:")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_response: KvListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list_response.keys.len(), 2);
    }

    #[tokio::test]
    async fn test_kv_set_with_ttl() {
        let app = create_test_app().await;

        let set_request = Request::builder()
            .method(Method::PUT)
            .uri("/kv/ttl-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"value": "expires-soon", "ttl": 3600}"#))
            .unwrap();

        let response = app.clone().oneshot(set_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify it exists
        let get_request = Request::builder()
            .uri("/kv/ttl-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // =========================================================================
    // SQL Service Tests
    // =========================================================================
    // Note: These tests are ignored in CI due to platform-specific SQLite issues.
    // Run locally with: cargo test -- --ignored

    #[tokio::test]
    #[ignore = "requires local SQLite setup"]
    async fn test_sql_execute_create_table() {
        let app = create_test_app().await;

        let request = Request::builder()
            .method(Method::POST)
            .uri("/sql/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "sql": "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
            }"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[ignore = "requires local SQLite setup"]
    async fn test_sql_execute_insert_and_query() {
        let app = create_test_app().await;

        // Create table
        let create_request = Request::builder()
            .method(Method::POST)
            .uri("/sql/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "sql": "CREATE TABLE test_users (id INTEGER PRIMARY KEY, name TEXT)"
            }"#,
            ))
            .unwrap();

        app.clone().oneshot(create_request).await.unwrap();

        // Insert data
        let insert_request = Request::builder()
            .method(Method::POST)
            .uri("/sql/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "sql": "INSERT INTO test_users (name) VALUES (?)",
                "params": ["Alice"]
            }"#,
            ))
            .unwrap();

        let response = app.clone().oneshot(insert_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let execute_response: SqlExecuteResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(execute_response.rows_affected, 1);

        // Query data
        let query_request = Request::builder()
            .method(Method::POST)
            .uri("/sql/query")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "sql": "SELECT * FROM test_users WHERE name = ?",
                "params": ["Alice"]
            }"#,
            ))
            .unwrap();

        let response = app.oneshot(query_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let query_response: SqlQueryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(query_response.rows.len(), 1);
        assert!(query_response.columns.contains(&"name".to_string()));
    }

    #[tokio::test]
    #[ignore = "requires local SQLite setup"]
    async fn test_sql_batch_execute() {
        let app = create_test_app().await;

        // Create table first
        let create_request = Request::builder()
            .method(Method::POST)
            .uri("/sql/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "sql": "CREATE TABLE batch_test (id INTEGER PRIMARY KEY, value TEXT)"
            }"#,
            ))
            .unwrap();

        app.clone().oneshot(create_request).await.unwrap();

        // Batch insert
        let batch_request = Request::builder()
            .method(Method::POST)
            .uri("/sql/batch")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "statements": [
                    {"sql": "INSERT INTO batch_test (value) VALUES (?)", "params": ["one"]},
                    {"sql": "INSERT INTO batch_test (value) VALUES (?)", "params": ["two"]},
                    {"sql": "INSERT INTO batch_test (value) VALUES (?)", "params": ["three"]}
                ]
            }"#,
            ))
            .unwrap();

        let response = app.oneshot(batch_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let batch_response: SqlBatchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(batch_response.results.len(), 3);
        for result in batch_response.results {
            assert_eq!(result.rows_affected, 1);
        }
    }

    // =========================================================================
    // Storage Service Tests
    // =========================================================================

    #[tokio::test]
    async fn test_storage_put_and_get() {
        let app = create_test_app().await;

        let content = b"Hello, World!";

        // Put an object
        let put_request = Request::builder()
            .method(Method::PUT)
            .uri("/storage/test/hello.txt")
            .header("content-type", "text/plain")
            .body(Body::from(content.to_vec()))
            .unwrap();

        let response = app.clone().oneshot(put_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Get the object
        let get_request = Request::builder()
            .uri("/storage/test/hello.txt")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], content);
    }

    #[tokio::test]
    async fn test_storage_get_not_found() {
        let app = create_test_app().await;

        let request = Request::builder()
            .uri("/storage/nonexistent/file.txt")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_storage_head() {
        let app = create_test_app().await;

        let content = b"Test content for head request";

        // Put an object
        let put_request = Request::builder()
            .method(Method::PUT)
            .uri("/storage/head-test.txt")
            .header("content-type", "text/plain")
            .body(Body::from(content.to_vec()))
            .unwrap();

        app.clone().oneshot(put_request).await.unwrap();

        // Head request
        let head_request = Request::builder()
            .method(Method::HEAD)
            .uri("/storage/head-test.txt")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(head_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain"
        );
        assert_eq!(
            response.headers().get("content-length").unwrap(),
            &content.len().to_string()
        );
    }

    #[tokio::test]
    async fn test_storage_delete() {
        let app = create_test_app().await;

        // Put an object
        let put_request = Request::builder()
            .method(Method::PUT)
            .uri("/storage/to-delete.txt")
            .header("content-type", "text/plain")
            .body(Body::from("delete me"))
            .unwrap();

        app.clone().oneshot(put_request).await.unwrap();

        // Delete the object
        let delete_request = Request::builder()
            .method(Method::DELETE)
            .uri("/storage/to-delete.txt")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(delete_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let get_request = Request::builder()
            .uri("/storage/to-delete.txt")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_storage_list() {
        let app = create_test_app().await;

        // Put multiple objects
        for path in ["docs/a.txt", "docs/b.txt", "images/c.png"] {
            let put_request = Request::builder()
                .method(Method::PUT)
                .uri(format!("/storage/{path}"))
                .header("content-type", "text/plain")
                .body(Body::from("content"))
                .unwrap();

            app.clone().oneshot(put_request).await.unwrap();
        }

        // List all objects
        let request = Request::builder()
            .uri("/storage")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_response: StorageListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list_response.objects.len(), 3);

        // List with prefix
        let request = Request::builder()
            .uri("/storage?prefix=docs/")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_response: StorageListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list_response.objects.len(), 2);
    }

    // =========================================================================
    // Error Handling Tests
    // =========================================================================

    #[tokio::test]
    #[ignore = "requires local SQLite setup"]
    async fn test_sql_invalid_query() {
        let app = create_test_app().await;

        let request = Request::builder()
            .method(Method::POST)
            .uri("/sql/query")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"sql": "SELECT * FROM nonexistent_table"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_kv_invalid_json() {
        let app = create_test_app().await;

        let request = Request::builder()
            .method(Method::PUT)
            .uri("/kv/test-key")
            .header("content-type", "application/json")
            .body(Body::from("not valid json"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should return 422 Unprocessable Entity or 400 Bad Request
        assert!(response.status().is_client_error());
    }

    // =========================================================================
    // API Key Authentication Tests
    // =========================================================================

    /// Create a test app with the API key auth middleware layer.
    async fn create_test_app_with_auth() -> Router {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let data_dir: &'static std::path::Path = Box::leak(Box::new(data_dir));

        let store = StateStore::open(data_dir.join("state.redb")).unwrap();
        let kv = Some(KvStore::open(data_dir.join("kv.redb")).unwrap());
        let sql = Some(SqlService::open(data_dir.join("sql.db")).unwrap());
        let storage = Some(StorageService::open(data_dir.join("storage")).unwrap());
        let cron = CronScheduler::new().await.unwrap();

        let app_state = Arc::new(RwLock::new(AppState {
            store,
            kv,
            sql,
            storage,
            cron,
        }));

        Router::new()
            .route("/health", get(health))
            .route("/metrics", get(metrics_endpoint))
            .route("/kv/{key}", get(kv_get))
            .with_state(app_state)
            .layer(middleware::from_fn(api_key_auth_middleware))
    }

    #[tokio::test]
    async fn test_auth_health_endpoint_always_accessible() {
        // Health endpoint should always be accessible, even with auth enabled
        let app = create_test_app_with_auth().await;

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should be OK regardless of API key configuration
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_metrics_endpoint_always_accessible() {
        // Metrics endpoint should always be accessible for Prometheus scraping
        let app = create_test_app_with_auth().await;

        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should be OK regardless of API key configuration
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_static_api_key_loading() {
        // Verify the API_KEY static loads correctly (None when env not set)
        // This test documents the expected behavior
        let key = API_KEY.as_ref();

        // In test environment, MIK_API_KEY is typically not set
        // This validates the LazyLock initializes without panic
        if let Some(k) = key {
            // If set, ensure it's not empty (our filter)
            assert!(!k.is_empty());
        }
    }
}
