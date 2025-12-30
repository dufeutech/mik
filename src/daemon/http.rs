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
//! ### Queue Service (`/queues`)
//! - `POST /queues/:name/push` - Push message
//! - `POST /queues/:name/pop` - Pop message
//! - `GET /queues/:name/peek` - Peek at next message
//! - `GET /queues/:name` - Get queue info
//! - `DELETE /queues/:name` - Delete queue
//! - `GET /queues` - List all queues
//! - `POST /topics/:name/publish` - Publish to topic
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
//! ### Service Discovery (`/services`)
//! - `GET /services` - List registered services (with optional `?service_type` filter)
//! - `POST /services` - Register a new service
//! - `GET /services/:name` - Get service details
//! - `DELETE /services/:name` - Unregister a service
//! - `POST /services/:name/heartbeat` - Update service heartbeat
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

use super::error as daemon_error;
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, head, post, put},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::daemon::cron::{CronScheduler, ScheduleConfig, parse_schedules_from_manifest};
use crate::daemon::metrics;
use crate::daemon::process::{self, SpawnConfig};
use crate::daemon::services::{
    kv::KvStore,
    queue::{QueueConfig, QueueService},
    sql::{SqlService, Value as SqlValue},
    storage::{ObjectMeta, StorageService},
};
use crate::daemon::state::{Instance, ServiceType, Sidecar, StateStore, Status};

// =============================================================================
// App State
// =============================================================================

/// Shared application state for HTTP handlers.
struct AppState {
    store: StateStore,
    kv: KvStore,
    sql: SqlService,
    storage: StorageService,
    queue: QueueService,
    cron: CronScheduler,
}

type SharedState = Arc<RwLock<AppState>>;

// =============================================================================
// Request/Response Types
// =============================================================================

/// Request to start a new instance.
#[derive(Debug, Deserialize)]
pub struct StartInstanceRequest {
    /// Instance name (required)
    pub name: String,
    /// Port for HTTP server (default: 3000)
    pub port: Option<u16>,
    /// Path to mik.toml config (default: ./mik.toml)
    pub config: Option<String>,
    /// Working directory (default: current directory)
    pub working_dir: Option<String>,
    /// Enable auto-restart on crash (default: false)
    #[serde(default)]
    pub auto_restart: bool,
}

/// Validates an instance name to prevent path traversal and other issues.
///
/// Instance names must:
/// - Be 1-64 characters long
/// - Contain only alphanumeric characters, hyphens, and underscores
/// - Not start with a hyphen or underscore
fn validate_instance_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Instance name cannot be empty".into());
    }
    if name.len() > 64 {
        return Err("Instance name must be 64 characters or less".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Instance name can only contain alphanumeric characters, hyphens, and underscores"
                .into(),
        );
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err("Instance name cannot start with a hyphen or underscore".into());
    }
    Ok(())
}

/// Response for instance operations.
#[derive(Debug, Serialize)]
pub struct InstanceResponse {
    pub name: String,
    pub port: u16,
    pub pid: u32,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
}

impl From<&Instance> for InstanceResponse {
    fn from(instance: &Instance) -> Self {
        let status = match &instance.status {
            Status::Running => "running".to_string(),
            Status::Stopped => "stopped".to_string(),
            Status::Crashed { exit_code } => format!("crashed (exit: {exit_code})"),
        };

        let uptime = if instance.status == Status::Running {
            let duration = chrono::Utc::now().signed_duration_since(instance.started_at);
            Some(format_duration(duration))
        } else {
            None
        };

        Self {
            name: instance.name.clone(),
            port: instance.port,
            pid: instance.pid,
            status,
            started_at: Some(instance.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            uptime,
        }
    }
}

/// Response containing list of instances.
#[derive(Debug, Serialize)]
pub struct ListInstancesResponse {
    pub instances: Vec<InstanceResponse>,
}

/// Health check response.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime: String,
}

/// Version response.
#[derive(Debug, Serialize, Deserialize)]
pub struct VersionResponse {
    pub version: String,
    pub build: String,
}

/// Query parameters for logs endpoint.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    /// Number of lines to return (default: 50)
    pub lines: Option<usize>,
}

/// Logs response.
#[derive(Debug, Serialize)]
pub struct LogsResponse {
    pub name: String,
    pub lines: Vec<String>,
}

/// Error response.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// =============================================================================
// KV Service Types
// =============================================================================

/// Request to set a KV value.
#[derive(Debug, Deserialize)]
pub struct KvSetRequest {
    pub value: String,
    /// TTL in seconds (optional)
    pub ttl: Option<u64>,
}

/// Response for KV get operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct KvGetResponse {
    pub key: String,
    pub value: String,
}

/// Query parameters for KV list.
#[derive(Debug, Deserialize)]
pub struct KvListQuery {
    pub prefix: Option<String>,
}

/// Response for KV list operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct KvListResponse {
    pub keys: Vec<String>,
}

// =============================================================================
// SQL Service Types
// =============================================================================

/// Request for SQL query.
#[derive(Debug, Deserialize)]
pub struct SqlQueryRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
}

/// Request for SQL execute.
#[derive(Debug, Deserialize)]
pub struct SqlExecuteRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
}

/// Request for SQL batch execution.
#[derive(Debug, Deserialize)]
pub struct SqlBatchRequest {
    pub statements: Vec<SqlExecuteRequest>,
}

/// Response for SQL query.
#[derive(Debug, Serialize, Deserialize)]
pub struct SqlQueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

/// Response for SQL execute.
#[derive(Debug, Serialize, Deserialize)]
pub struct SqlExecuteResponse {
    pub rows_affected: u64,
}

/// Response for SQL batch execution.
#[derive(Debug, Serialize, Deserialize)]
pub struct SqlBatchResponse {
    pub results: Vec<SqlExecuteResponse>,
}

// =============================================================================
// Storage Service Types
// =============================================================================

/// Query parameters for storage list.
#[derive(Debug, Deserialize)]
pub struct StorageListQuery {
    pub prefix: Option<String>,
    pub limit: Option<usize>,
}

/// Response for storage list operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct StorageListResponse {
    pub objects: Vec<StorageObjectInfo>,
}

/// Storage object info.
#[derive(Debug, Serialize, Deserialize)]
pub struct StorageObjectInfo {
    pub path: String,
    pub size: u64,
    pub content_type: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ObjectMeta> for StorageObjectInfo {
    fn from(meta: ObjectMeta) -> Self {
        Self {
            path: meta.path,
            size: meta.size,
            content_type: meta.content_type,
            created_at: meta.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            updated_at: meta.modified_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        }
    }
}

// =============================================================================
// Queue Service Types
// =============================================================================

/// Request to push a message.
#[derive(Debug, Deserialize)]
pub struct QueuePushRequest {
    pub payload: serde_json::Value,
}

/// Response for queue pop operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueuePopResponse {
    pub message: Option<QueueMessageInfo>,
}

/// Response for queue peek operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueuePeekResponse {
    pub message: Option<QueueMessageInfo>,
}

/// Queue message info.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueueMessageInfo {
    pub id: String,
    pub payload: serde_json::Value,
    pub created_at: String,
}

/// Response for queue info.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueueInfoResponse {
    pub name: String,
    pub length: usize,
    pub persistent: bool,
}

/// Response for list queues.
#[derive(Debug, Serialize, Deserialize)]
pub struct ListQueuesResponse {
    pub queues: Vec<QueueInfoResponse>,
}

/// Request to publish to a topic.
#[derive(Debug, Deserialize)]
pub struct TopicPublishRequest {
    pub payload: serde_json::Value,
}

// =============================================================================
// Cron Service Types
// =============================================================================

/// Response for cron job info.
#[derive(Debug, Serialize, Deserialize)]
pub struct CronJobResponse {
    pub name: String,
    pub cron: String,
    pub module: String,
    pub method: String,
    pub path: String,
    pub enabled: bool,
    pub status: String,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
}

/// Response for listing cron jobs.
#[derive(Debug, Serialize, Deserialize)]
pub struct ListCronJobsResponse {
    pub jobs: Vec<CronJobResponse>,
}

/// Response for cron job execution history.
#[derive(Debug, Serialize, Deserialize)]
pub struct CronHistoryResponse {
    pub job_name: String,
    pub executions: Vec<CronExecutionInfo>,
}

/// Info about a single cron execution.
#[derive(Debug, Serialize, Deserialize)]
pub struct CronExecutionInfo {
    pub id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub success: bool,
    pub error: Option<String>,
    pub manual: bool,
}

/// Response for triggering a job.
#[derive(Debug, Serialize, Deserialize)]
pub struct CronTriggerResponse {
    pub job_name: String,
    pub execution_id: String,
    pub triggered: bool,
}

/// Request to create a cron job.
#[derive(Debug, Deserialize)]
pub struct CronCreateRequest {
    pub name: String,
    pub module: String,
    pub cron: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional request body (JSON) to send with the request.
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    /// Optional request headers to send.
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// Health check path (default: /health).
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_health_path() -> String {
    "/health".to_string()
}

fn default_path() -> String {
    "/".to_string()
}

fn default_enabled() -> bool {
    true
}

fn default_port() -> u16 {
    3000
}

/// Response for creating a cron job.
#[derive(Debug, Serialize)]
pub struct CronCreateResponse {
    pub name: String,
    pub created: bool,
}

/// Response for deleting a cron job.
#[derive(Debug, Serialize)]
pub struct CronDeleteResponse {
    pub name: String,
    pub deleted: bool,
}

/// Request to update a cron job (pause/resume).
#[derive(Debug, Deserialize)]
pub struct CronUpdateRequest {
    /// Set to false to pause, true to resume.
    pub enabled: Option<bool>,
}

/// Response for updating a cron job.
#[derive(Debug, Serialize)]
pub struct CronUpdateResponse {
    pub name: String,
    pub enabled: bool,
    pub updated: bool,
}

// =============================================================================
// Service Discovery Types
// =============================================================================

/// Request to register a sidecar service.
#[derive(Debug, Deserialize)]
pub struct RegisterServiceRequest {
    /// Unique service name (e.g., "mikcar-primary")
    pub name: String,
    /// Service type: "kv", "sql", "storage", "queue", or "custom:name"
    pub service_type: String,
    /// Base URL for the service (e.g., `http://localhost:9001`)
    pub url: String,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
}

/// Response for a registered service.
#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceResponse {
    pub name: String,
    pub service_type: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub registered_at: String,
    pub last_heartbeat: String,
    pub healthy: bool,
}

impl From<&Sidecar> for ServiceResponse {
    fn from(s: &Sidecar) -> Self {
        Self {
            name: s.name.clone(),
            service_type: s.service_type.to_string(),
            url: s.url.clone(),
            description: s.description.clone(),
            registered_at: s.registered_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            last_heartbeat: s.last_heartbeat.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            healthy: s.healthy,
        }
    }
}

/// Response for listing services.
#[derive(Debug, Serialize, Deserialize)]
pub struct ListServicesResponse {
    pub services: Vec<ServiceResponse>,
}

/// Query parameters for listing services.
#[derive(Debug, Deserialize)]
pub struct ListServicesQuery {
    /// Filter by service type (e.g., "kv", "sql")
    pub service_type: Option<String>,
}

/// Response for service registration.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterServiceResponse {
    pub name: String,
    pub registered: bool,
}

/// Response for service deletion.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteServiceResponse {
    pub name: String,
    pub deleted: bool,
}

/// Response for heartbeat.
#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub name: String,
    pub updated: bool,
}

// =============================================================================
// HTTP API Server
// =============================================================================

/// Starts the HTTP API server on the specified port.
///
/// The server provides REST endpoints for managing WASM instances and
/// embedded services. See module-level documentation for endpoint details.
pub async fn serve(port: u16, state_path: PathBuf) -> Result<()> {
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

    // Initialize embedded services
    let kv = KvStore::open(data_dir.join("kv.redb")).context("Failed to open KV store")?;
    let sql = SqlService::open(data_dir.join("sql.db")).context("Failed to open SQL database")?;
    let storage =
        StorageService::open(data_dir.join("storage")).context("Failed to open storage service")?;
    let queue =
        QueueService::new(QueueConfig::default()).context("Failed to create queue service")?;

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
        queue,
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
        // Queue service
        .route("/queues", get(queue_list))
        .route("/queues/{name}", get(queue_info))
        .route("/queues/{name}", delete(queue_delete))
        .route("/queues/{name}/push", post(queue_push))
        .route("/queues/{name}/pop", post(queue_pop))
        .route("/queues/{name}/peek", get(queue_peek))
        .route("/topics/{name}/publish", post(topic_publish))
        // Cron scheduler
        .route("/cron", get(cron_list).post(cron_create))
        .route(
            "/cron/{name}",
            get(cron_get).patch(cron_update).delete(cron_delete),
        )
        .route("/cron/{name}/trigger", post(cron_trigger))
        .route("/cron/{name}/history", get(cron_history))
        // Service discovery
        .route("/services", get(services_list).post(services_register))
        .route(
            "/services/{name}",
            get(services_get).delete(services_delete),
        )
        .route("/services/{name}/heartbeat", post(services_heartbeat))
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
            .unwrap_or(std::path::Path::new("."))
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
// Route Handlers
// =============================================================================

/// GET /instances - List all instances.
async fn list_instances(
    State(state): State<SharedState>,
) -> Result<Json<ListInstancesResponse>, AppError> {
    // Clone store and drop lock before async operation
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instances = store.list_instances_async().await?;

    let mut responses = Vec::with_capacity(instances.len());
    for instance in &instances {
        // Check actual running status
        let mut response = InstanceResponse::from(instance);
        if instance.status == Status::Running && !process::is_running(instance.pid)? {
            response.status = "crashed".to_string();
            response.uptime = None;
        }
        responses.push(response);
    }

    Ok(Json(ListInstancesResponse {
        instances: responses,
    }))
}

/// POST /instances - Start a new instance.
async fn start_instance(
    State(state): State<SharedState>,
    Json(req): Json<StartInstanceRequest>,
) -> Result<(StatusCode, Json<InstanceResponse>), AppError> {
    // Validate instance name to prevent path traversal and other security issues
    if let Err(e) = validate_instance_name(&req.name) {
        return Err(AppError::BadRequest(e));
    }

    // Clone store for async check, then drop lock
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    // Check if instance already exists and is running (non-blocking)
    if let Some(existing) = store.get_instance_async(req.name.clone()).await?
        && existing.status == Status::Running
        && process::is_running(existing.pid)?
    {
        return Err(AppError::Conflict(format!(
            "Instance '{}' is already running on port {}",
            req.name, existing.port
        )));
    }

    // Resolve config path
    let config_path = match req.config {
        Some(ref path) => PathBuf::from(path),
        None => std::env::current_dir()?.join("mik.toml"),
    };

    if !config_path.exists() {
        return Err(AppError::BadRequest(format!(
            "Config file not found: {}",
            config_path.display()
        )));
    }

    let working_dir = match req.working_dir {
        Some(ref path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };

    let port = req.port.unwrap_or(3000);

    let spawn_config = SpawnConfig {
        name: req.name.clone(),
        port,
        config_path: config_path.clone(),
        working_dir: working_dir.clone(),
        hot_reload: false,
    };

    let info = process::spawn_instance(&spawn_config)?;

    // Save instance state
    let instance = Instance {
        name: req.name.clone(),
        port,
        pid: info.pid,
        status: Status::Running,
        config: config_path,
        started_at: chrono::Utc::now(),
        modules: vec![],
        auto_restart: req.auto_restart,
        restart_count: 0,
        last_restart_at: None,
    };

    // Save instance asynchronously (non-blocking)
    store.save_instance_async(instance.clone()).await?;

    // Parse and register schedules from mik.toml
    // Need to hold lock for cron operations
    let state = state.write().await;
    match parse_schedules_from_manifest(&instance.config) {
        Ok(schedules) if !schedules.is_empty() => {
            tracing::info!(
                instance = %req.name,
                count = schedules.len(),
                "Loading schedules from mik.toml"
            );

            for mut schedule in schedules {
                // Scope job name to instance: "instance_name:job_name"
                let scoped_name = format!("{}:{}", req.name, schedule.name);
                schedule.name.clone_from(&scoped_name);

                // Use instance port if schedule doesn't specify one
                if schedule.port == 3000 {
                    schedule.port = port;
                }

                // Add to scheduler
                match state.cron.add_job(schedule.clone()).await {
                    Ok(()) => {
                        // Persist to database (already async)
                        if let Err(e) = state.store.save_cron_job_async(schedule.clone()).await {
                            tracing::warn!(
                                job = %scoped_name,
                                error = %e,
                                "Failed to persist schedule job"
                            );
                        } else {
                            tracing::info!(job = %scoped_name, "Registered schedule from mik.toml");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            job = %scoped_name,
                            error = %e,
                            "Failed to register schedule from mik.toml"
                        );
                    },
                }
            }
        },
        Ok(_) => {
            // No schedules in config, that's fine
        },
        Err(e) => {
            tracing::warn!(
                instance = %req.name,
                error = %e,
                "Failed to parse schedules from mik.toml"
            );
        },
    }

    Ok((StatusCode::CREATED, Json(InstanceResponse::from(&instance))))
}

/// GET /instances/:name - Get instance details.
async fn get_instance(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<InstanceResponse>, AppError> {
    // Clone store and drop lock before async operation
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instance = store
        .get_instance_async(name.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Instance '{name}' not found")))?;

    let mut response = InstanceResponse::from(&instance);

    // Check actual running status
    if instance.status == Status::Running && !process::is_running(instance.pid)? {
        response.status = "crashed".to_string();
        response.uptime = None;
    }

    Ok(Json(response))
}

/// DELETE /instances/:name - Stop an instance.
async fn stop_instance(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<InstanceResponse>, AppError> {
    // Clone store for async operations
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instance = store
        .get_instance_async(name.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Instance '{name}' not found")))?;

    if instance.status != Status::Running {
        return Err(AppError::BadRequest(format!(
            "Instance '{}' is not running (status: {:?})",
            name, instance.status
        )));
    }

    // Kill the process
    if process::is_running(instance.pid)? {
        process::kill_instance(instance.pid)?;
    }

    // Remove instance-scoped cron jobs (those starting with "instance_name:")
    // Need to hold write lock for cron operations
    let state = state.write().await;
    let prefix = format!("{name}:");
    let jobs = state.cron.list_jobs().await;
    for job in jobs {
        if job.name.starts_with(&prefix) {
            if let Err(e) = state.cron.remove_job(&job.name).await {
                tracing::warn!(job = %job.name, error = %e, "Failed to remove cron job on instance stop");
            } else {
                // Also remove from database (already async)
                if let Err(e) = state.store.remove_cron_job_async(job.name.clone()).await {
                    tracing::warn!(job = %job.name, error = %e, "Failed to remove cron job from database");
                } else {
                    tracing::info!(job = %job.name, "Removed cron job on instance stop");
                }
            }
        }
    }

    // Update state asynchronously
    let mut updated = instance;
    updated.status = Status::Stopped;
    store.save_instance_async(updated.clone()).await?;

    Ok(Json(InstanceResponse::from(&updated)))
}

/// POST /instances/:name/restart - Restart an instance.
async fn restart_instance(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<InstanceResponse>, AppError> {
    // Clone store for async operations
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instance = store
        .get_instance_async(name.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Instance '{name}' not found")))?;

    // Stop if running
    if instance.status == Status::Running && process::is_running(instance.pid)? {
        process::kill_instance(instance.pid)?;
    }

    // Remove old instance-scoped cron jobs before restart
    // Need write lock for cron operations
    let state_guard = state.write().await;
    let prefix = format!("{name}:");
    let jobs = state_guard.cron.list_jobs().await;
    for job in jobs {
        if job.name.starts_with(&prefix) {
            if let Err(e) = state_guard.cron.remove_job(&job.name).await {
                tracing::warn!(job = %job.name, error = %e, "Failed to remove cron job on restart");
            } else {
                let _ = state_guard
                    .store
                    .remove_cron_job_async(job.name.clone())
                    .await;
            }
        }
    }
    // Drop write lock before spawn_blocking operations
    drop(state_guard);

    // Restart with same config
    let spawn_config = SpawnConfig {
        name: instance.name.clone(),
        port: instance.port,
        config_path: instance.config.clone(),
        working_dir: instance
            .config
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf(),
        hot_reload: false,
    };

    let info = process::spawn_instance(&spawn_config)?;

    // Update instance state (preserve auto_restart settings)
    let updated = Instance {
        name: instance.name.clone(),
        port: instance.port,
        pid: info.pid,
        status: Status::Running,
        config: instance.config.clone(),
        started_at: chrono::Utc::now(),
        modules: instance.modules.clone(),
        auto_restart: instance.auto_restart,
        restart_count: instance.restart_count,
        last_restart_at: instance.last_restart_at,
    };

    // Save asynchronously
    store.save_instance_async(updated.clone()).await?;

    // Re-parse and register schedules from mik.toml
    // Need write lock again for cron operations
    let state_guard = state.write().await;
    if let Ok(schedules) = parse_schedules_from_manifest(&instance.config) {
        for mut schedule in schedules {
            let scoped_name = format!("{name}:{}", schedule.name);
            schedule.name.clone_from(&scoped_name);
            if schedule.port == 3000 {
                schedule.port = instance.port;
            }

            if let Ok(()) = state_guard.cron.add_job(schedule.clone()).await {
                let _ = state_guard.store.save_cron_job_async(schedule).await;
                tracing::info!(job = %scoped_name, "Re-registered schedule on restart");
            }
        }
    }

    Ok(Json(InstanceResponse::from(&updated)))
}

/// GET /instances/:name/logs - Get instance logs.
async fn get_logs(
    Path(name): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, AppError> {
    let home = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".to_string()))?;

    let log_path = home.join(".mik").join("logs").join(format!("{name}.log"));

    if !log_path.exists() {
        return Ok(Json(LogsResponse {
            name,
            lines: vec![],
        }));
    }

    let lines_count = query.lines.unwrap_or(50);
    let lines = process::tail_log(&log_path, lines_count)?;

    Ok(Json(LogsResponse { name, lines }))
}

/// GET /health - Health check.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime: "running".to_string(),
    })
}

/// GET /version - Version info.
async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: "release".to_string(),
    })
}

// =============================================================================
// KV Service Handlers
// =============================================================================

/// GET /kv - List all keys with optional prefix filter.
async fn kv_list(
    State(state): State<SharedState>,
    Query(query): Query<KvListQuery>,
) -> Result<Json<KvListResponse>, AppError> {
    metrics::record_kv_operation("list");
    // Clone KV store and drop lock before async operation
    let kv = {
        let state = state.read().await;
        state.kv.clone()
    };
    let keys = kv.list_keys_async(query.prefix).await?;
    Ok(Json(KvListResponse { keys }))
}

/// GET /kv/:key - Get a value by key.
async fn kv_get(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> Result<Json<KvGetResponse>, AppError> {
    metrics::record_kv_operation("get");
    // Clone KV store and drop lock before async operation
    let kv = {
        let state = state.read().await;
        state.kv.clone()
    };
    let bytes = kv
        .get_async(key.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Key '{key}' not found")))?;

    let value = String::from_utf8(bytes)
        .map_err(|_| AppError::Internal("Value is not valid UTF-8".to_string()))?;

    Ok(Json(KvGetResponse { key, value }))
}

/// PUT /kv/:key - Set a value with optional TTL.
async fn kv_set(
    State(state): State<SharedState>,
    Path(key): Path<String>,
    Json(req): Json<KvSetRequest>,
) -> Result<StatusCode, AppError> {
    metrics::record_kv_operation("set");
    // Clone KV store and drop lock before async operation
    let kv = {
        let state = state.read().await;
        state.kv.clone()
    };
    let value_bytes = req.value.into_bytes();

    match req.ttl {
        Some(ttl) => kv.set_with_ttl_async(key, value_bytes, ttl).await?,
        None => kv.set_async(key, value_bytes).await?,
    }

    Ok(StatusCode::OK)
}

/// DELETE /kv/:key - Delete a key.
async fn kv_delete(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> Result<StatusCode, AppError> {
    metrics::record_kv_operation("delete");
    // Clone KV store and drop lock before async operation
    let kv = {
        let state = state.read().await;
        state.kv.clone()
    };
    kv.delete_async(key).await?;
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// SQL Service Handlers
// =============================================================================

/// Convert JSON value to SQL value.
fn json_to_sql_value(v: &serde_json::Value) -> SqlValue {
    match v {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValue::Real(f)
            } else {
                SqlValue::Text(n.to_string())
            }
        },
        serde_json::Value::String(s) => SqlValue::Text(s.clone()),
        _ => SqlValue::Text(v.to_string()),
    }
}

/// Convert SQL value to JSON value.
fn sql_to_json_value(v: &SqlValue) -> serde_json::Value {
    match v {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(i) => serde_json::json!(*i),
        SqlValue::Real(f) => serde_json::json!(*f),
        SqlValue::Text(s) => serde_json::json!(s),
        SqlValue::Blob(b) => {
            // Encode blob as hex string for JSON safety
            let hex = b.iter().fold(String::new(), |mut acc, byte| {
                use std::fmt::Write;
                let _ = write!(acc, "{byte:02x}");
                acc
            });
            serde_json::json!({ "hex": hex })
        },
    }
}

/// POST /sql/query - Execute a SELECT query.
async fn sql_query(
    State(state): State<SharedState>,
    Json(req): Json<SqlQueryRequest>,
) -> Result<Json<SqlQueryResponse>, AppError> {
    let start = Instant::now();
    // Clone SQL service and drop lock before async operation
    let sql = {
        let state = state.read().await;
        state.sql.clone()
    };
    let params: Vec<SqlValue> = req.params.iter().map(json_to_sql_value).collect();

    let rows = sql.query_async(req.sql, params).await?;
    metrics::record_sql_query("query", start.elapsed().as_secs_f64());

    // Extract column names from the first row (if available)
    let columns = rows.first().map(|r| r.columns.clone()).unwrap_or_default();

    let json_rows: Vec<Vec<serde_json::Value>> = rows
        .iter()
        .map(|row| row.values.iter().map(sql_to_json_value).collect())
        .collect();

    Ok(Json(SqlQueryResponse {
        columns,
        rows: json_rows,
    }))
}

/// POST /sql/execute - Execute an INSERT/UPDATE/DELETE statement.
async fn sql_execute(
    State(state): State<SharedState>,
    Json(req): Json<SqlExecuteRequest>,
) -> Result<Json<SqlExecuteResponse>, AppError> {
    let start = Instant::now();
    // Clone SQL service and drop lock before async operation
    let sql = {
        let state = state.read().await;
        state.sql.clone()
    };
    let params: Vec<SqlValue> = req.params.iter().map(json_to_sql_value).collect();

    let rows_affected = sql.execute_async(req.sql, params).await?;
    metrics::record_sql_query("execute", start.elapsed().as_secs_f64());

    Ok(Json(SqlExecuteResponse {
        rows_affected: rows_affected as u64,
    }))
}

/// POST /sql/batch - Execute a batch of statements.
async fn sql_batch(
    State(state): State<SharedState>,
    Json(req): Json<SqlBatchRequest>,
) -> Result<Json<SqlBatchResponse>, AppError> {
    let start = Instant::now();
    // Clone SQL service and drop lock before async operation
    let sql = {
        let state = state.read().await;
        state.sql.clone()
    };

    let mut results = Vec::with_capacity(req.statements.len());
    for stmt in &req.statements {
        let params: Vec<SqlValue> = stmt.params.iter().map(json_to_sql_value).collect();
        let rows_affected = sql.execute_async(stmt.sql.clone(), params).await?;
        results.push(SqlExecuteResponse {
            rows_affected: rows_affected as u64,
        });
    }
    metrics::record_sql_query("batch", start.elapsed().as_secs_f64());

    Ok(Json(SqlBatchResponse { results }))
}

// =============================================================================
// Storage Service Handlers
// =============================================================================

/// GET /storage - List objects with optional prefix.
async fn storage_list(
    State(state): State<SharedState>,
    Query(query): Query<StorageListQuery>,
) -> Result<Json<StorageListResponse>, AppError> {
    metrics::record_storage_operation("list", None);
    // Clone storage service and drop lock before async operation
    let storage = {
        let state = state.read().await;
        state.storage.clone()
    };
    let mut objects: Vec<StorageObjectInfo> = storage
        .list_objects_async(query.prefix)
        .await?
        .into_iter()
        .map(StorageObjectInfo::from)
        .collect();

    // Apply limit if specified
    if let Some(limit) = query.limit {
        objects.truncate(limit);
    }

    Ok(Json(StorageListResponse { objects }))
}

/// GET /storage/*path - Get an object.
async fn storage_get(
    State(state): State<SharedState>,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Clone storage service and drop lock before async operation
    let storage = {
        let state = state.read().await;
        state.storage.clone()
    };
    let (data, meta) = storage
        .get_object_async(path.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Object '{path}' not found")))?;

    metrics::record_storage_operation("get", Some(data.len() as u64));
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, meta.content_type)],
        data,
    ))
}

/// PUT /storage/*path - Store an object.
async fn storage_put(
    State(state): State<SharedState>,
    Path(path): Path<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    metrics::record_storage_operation("put", Some(body.len() as u64));
    // Clone storage service and drop lock before async operation
    let storage = {
        let state = state.read().await;
        state.storage.clone()
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    storage
        .put_object_async(path, body.to_vec(), content_type)
        .await?;
    Ok(StatusCode::CREATED)
}

/// DELETE /storage/*path - Delete an object.
async fn storage_delete(
    State(state): State<SharedState>,
    Path(path): Path<String>,
) -> Result<StatusCode, AppError> {
    metrics::record_storage_operation("delete", None);
    // Clone storage service and drop lock before async operation
    let storage = {
        let state = state.read().await;
        state.storage.clone()
    };
    storage.delete_object_async(path).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// HEAD /storage/*path - Get object metadata.
async fn storage_head(
    State(state): State<SharedState>,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    metrics::record_storage_operation("head", None);
    // Clone storage service and drop lock before async operation
    let storage = {
        let state = state.read().await;
        state.storage.clone()
    };
    let meta = storage
        .head_object_async(path.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Object '{path}' not found")))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, meta.content_type),
            (header::CONTENT_LENGTH, meta.size.to_string()),
        ],
    ))
}

// =============================================================================
// Queue Service Handlers
// =============================================================================

/// GET /queues - List all queues.
async fn queue_list(
    State(state): State<SharedState>,
) -> Result<Json<ListQueuesResponse>, AppError> {
    metrics::record_queue_operation("list", "all");
    let state = state.read().await;
    let names = state.queue.list_queues();

    let mut queues = Vec::with_capacity(names.len());
    for name in names {
        let length = state.queue.len(&name)?;
        metrics::set_queue_length(&name, length);
        queues.push(QueueInfoResponse {
            name,
            length,
            persistent: false, // In-memory queues
        });
    }

    Ok(Json(ListQueuesResponse { queues }))
}

/// GET /queues/:name - Get queue info.
async fn queue_info(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<QueueInfoResponse>, AppError> {
    metrics::record_queue_operation("info", &name);
    let state = state.read().await;
    let length = state.queue.len(&name)?;
    metrics::set_queue_length(&name, length);

    Ok(Json(QueueInfoResponse {
        name,
        length,
        persistent: false,
    }))
}

/// DELETE /queues/:name - Delete a queue.
async fn queue_delete(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    metrics::record_queue_operation("delete", &name);
    let state = state.write().await;
    state.queue.delete_queue(&name)?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /queues/:name/push - Push a message to a queue.
async fn queue_push(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<QueuePushRequest>,
) -> Result<StatusCode, AppError> {
    metrics::record_queue_operation("push", &name);
    let state = state.write().await;
    let payload = serde_json::to_vec(&req.payload)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON payload: {e}")))?;

    state.queue.push(&name, &payload)?;
    Ok(StatusCode::CREATED)
}

/// POST /queues/:name/pop - Pop a message from a queue.
async fn queue_pop(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<QueuePopResponse>, AppError> {
    metrics::record_queue_operation("pop", &name);
    let state = state.write().await;
    let message = state.queue.pop(&name)?;

    let response = match message {
        Some(msg) => {
            let payload: serde_json::Value =
                serde_json::from_slice(&msg.data).unwrap_or_else(|_| {
                    // Encode as hex if not valid JSON
                    let hex = msg.data.iter().fold(String::new(), |mut acc, b| {
                        use std::fmt::Write;
                        let _ = write!(acc, "{b:02x}");
                        acc
                    });
                    serde_json::json!({"hex": hex})
                });

            QueuePopResponse {
                message: Some(QueueMessageInfo {
                    id: msg.id,
                    payload,
                    created_at: msg.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                }),
            }
        },
        None => QueuePopResponse { message: None },
    };

    Ok(Json(response))
}

/// GET /queues/:name/peek - Peek at the next message without removing it.
async fn queue_peek(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<QueuePeekResponse>, AppError> {
    metrics::record_queue_operation("peek", &name);
    let state = state.read().await;
    let message = state.queue.peek(&name)?;

    let response = match message {
        Some(msg) => {
            let payload: serde_json::Value =
                serde_json::from_slice(&msg.data).unwrap_or_else(|_| {
                    // Encode as hex if not valid JSON
                    let hex = msg.data.iter().fold(String::new(), |mut acc, b| {
                        use std::fmt::Write;
                        let _ = write!(acc, "{b:02x}");
                        acc
                    });
                    serde_json::json!({"hex": hex})
                });

            QueuePeekResponse {
                message: Some(QueueMessageInfo {
                    id: msg.id,
                    payload,
                    created_at: msg.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                }),
            }
        },
        None => QueuePeekResponse { message: None },
    };

    Ok(Json(response))
}

/// POST /topics/:name/publish - Publish a message to a topic.
async fn topic_publish(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<TopicPublishRequest>,
) -> Result<StatusCode, AppError> {
    metrics::record_queue_operation("publish", &name);
    let state = state.write().await;
    let payload = serde_json::to_vec(&req.payload)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON payload: {e}")))?;

    state.queue.publish(&name, &payload)?;
    Ok(StatusCode::OK)
}

// =============================================================================
// Cron Scheduler Handlers
// =============================================================================

/// GET /cron - List all scheduled jobs.
async fn cron_list(
    State(state): State<SharedState>,
) -> Result<Json<ListCronJobsResponse>, AppError> {
    let state = state.read().await;
    let jobs = state.cron.list_jobs().await;

    let responses: Vec<CronJobResponse> = jobs
        .iter()
        .map(|job| {
            // Determine status based on last execution
            let status_str = match &job.last_execution {
                Some(exec) if exec.success => "idle",
                Some(_) => "failed",
                None => "idle",
            };

            CronJobResponse {
                name: job.name.clone(),
                cron: job.cron.clone(),
                module: job.module.clone(),
                method: "GET".to_string(), // Default, could be stored in JobInfo
                path: "/".to_string(),     // Default, could be stored in JobInfo
                enabled: job.enabled,
                status: status_str.to_string(),
                last_run: job
                    .last_execution
                    .as_ref()
                    .map(|e| e.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                next_run: job
                    .next_run
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            }
        })
        .collect();

    Ok(Json(ListCronJobsResponse { jobs: responses }))
}

/// POST /cron - Create a new cron job.
async fn cron_create(
    State(state): State<SharedState>,
    Json(req): Json<CronCreateRequest>,
) -> Result<Json<CronCreateResponse>, AppError> {
    use std::path::PathBuf;

    let state = state.read().await;

    // Create schedule config
    let config = ScheduleConfig {
        name: req.name.clone(),
        module: PathBuf::from(&req.module),
        cron: req.cron,
        method: req.method,
        path: req.path,
        enabled: req.enabled,
        port: req.port,
        body: req.body,
        headers: req.headers,
        health_path: req.health_path,
    };

    // Add the job to scheduler
    state
        .cron
        .add_job(config.clone())
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to create cron job: {e}")))?;

    // Persist to database
    state
        .store
        .save_cron_job_async(config)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to persist cron job: {e}")))?;

    tracing::info!(name = %req.name, "Cron job created and persisted");

    Ok(Json(CronCreateResponse {
        name: req.name,
        created: true,
    }))
}

/// GET /cron/:name - Get job details.
async fn cron_get(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronJobResponse>, AppError> {
    let state = state.read().await;
    let job = state
        .cron
        .get_job(&name)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Cron job '{name}' not found")))?;

    // Determine status based on last execution
    let status_str = match &job.last_execution {
        Some(exec) if exec.success => "idle",
        Some(_) => "failed",
        None => "idle",
    };

    Ok(Json(CronJobResponse {
        name: job.name.clone(),
        cron: job.cron.clone(),
        module: job.module.clone(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: job.enabled,
        status: status_str.to_string(),
        last_run: job
            .last_execution
            .as_ref()
            .map(|e| e.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        next_run: job
            .next_run
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
    }))
}

/// DELETE /cron/:name - Delete a cron job.
async fn cron_delete(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronDeleteResponse>, AppError> {
    let state = state.read().await;

    let deleted = state
        .cron
        .remove_job(&name)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to delete cron job: {e}")))?;

    if !deleted {
        return Err(AppError::NotFound(format!("Cron job '{name}' not found")));
    }

    // Remove from database
    state
        .store
        .remove_cron_job_async(name.clone())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to remove cron job from database: {e}")))?;

    tracing::info!(name = %name, "Cron job deleted and removed from database");

    Ok(Json(CronDeleteResponse {
        name,
        deleted: true,
    }))
}

/// PATCH /cron/:name - Update a cron job (pause/resume).
async fn cron_update(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<CronUpdateRequest>,
) -> Result<Json<CronUpdateResponse>, AppError> {
    let state = state.read().await;

    // Check if there's anything to update
    let Some(enabled) = req.enabled else {
        return Err(AppError::BadRequest(
            "No fields to update. Provide 'enabled' field.".to_string(),
        ));
    };

    // Update the enabled state
    let new_enabled = state
        .cron
        .set_enabled(&name, enabled)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Cron job '{name}' not found")))?;

    // Persist the updated config to database
    if let Some(config) = state.cron.get_config(&name).await {
        state
            .store
            .save_cron_job_async(config)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to persist cron job update: {e}")))?;
    }

    let action = if new_enabled { "resumed" } else { "paused" };
    tracing::info!(name = %name, enabled = new_enabled, "Cron job {action}");

    Ok(Json(CronUpdateResponse {
        name,
        enabled: new_enabled,
        updated: true,
    }))
}

/// POST /cron/:name/trigger - Manually trigger a job.
async fn cron_trigger(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronTriggerResponse>, AppError> {
    let state = state.read().await;

    // Check job exists
    if state.cron.get_job(&name).await.is_none() {
        return Err(AppError::NotFound(format!("Cron job '{name}' not found")));
    }

    let execution = state
        .cron
        .trigger_job(&name)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to trigger job: {e}")))?;

    // Persist execution to database
    if let Err(e) = state
        .store
        .save_job_execution_async(execution.clone())
        .await
    {
        tracing::warn!(job = %name, error = %e, "Failed to persist job execution");
    }

    Ok(Json(CronTriggerResponse {
        job_name: name,
        execution_id: execution.id,
        triggered: true,
    }))
}

/// GET /cron/:name/history - Get job execution history.
async fn cron_history(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronHistoryResponse>, AppError> {
    let state = state.read().await;

    // Check job exists
    if state.cron.get_job(&name).await.is_none() {
        return Err(AppError::NotFound(format!("Cron job '{name}' not found")));
    }

    // Get history from database (persistent)
    let history = state
        .store
        .list_job_executions_async(name.clone(), 100)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read job history: {e}")))?;

    let executions: Vec<CronExecutionInfo> = history
        .iter()
        .map(|exec| CronExecutionInfo {
            id: exec.id.clone(),
            started_at: exec.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            completed_at: exec
                .completed_at
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            duration_ms: exec.duration_ms,
            success: exec.success,
            error: exec.error.clone(),
            manual: exec.manual,
        })
        .collect();

    Ok(Json(CronHistoryResponse {
        job_name: name,
        executions,
    }))
}

// =============================================================================
// Service Discovery Handlers
// =============================================================================

/// Helper to parse service type from string.
fn parse_service_type(s: &str) -> ServiceType {
    match s.to_lowercase().as_str() {
        "kv" => ServiceType::Kv,
        "sql" => ServiceType::Sql,
        "storage" => ServiceType::Storage,
        "queue" => ServiceType::Queue,
        other => {
            if let Some(name) = other.strip_prefix("custom:") {
                ServiceType::Custom(name.to_string())
            } else {
                ServiceType::Custom(other.to_string())
            }
        },
    }
}

/// GET /services - List registered services.
async fn services_list(
    State(state): State<SharedState>,
    Query(query): Query<ListServicesQuery>,
) -> Result<Json<ListServicesResponse>, AppError> {
    let state = state.read().await;

    let sidecars = if let Some(ref type_filter) = query.service_type {
        let service_type = parse_service_type(type_filter);
        state
            .store
            .list_sidecars_by_type_async(service_type)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to list services: {e}")))?
    } else {
        state
            .store
            .list_sidecars_async()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to list services: {e}")))?
    };

    let services: Vec<ServiceResponse> = sidecars.iter().map(ServiceResponse::from).collect();

    Ok(Json(ListServicesResponse { services }))
}

/// POST /services - Register a new service.
async fn services_register(
    State(state): State<SharedState>,
    Json(req): Json<RegisterServiceRequest>,
) -> Result<Json<RegisterServiceResponse>, AppError> {
    let state = state.read().await;

    // Validate URL format
    if !req.url.starts_with("http://") && !req.url.starts_with("https://") {
        return Err(AppError::BadRequest(
            "URL must start with http:// or https://".to_string(),
        ));
    }

    let now = chrono::Utc::now();
    let sidecar = Sidecar {
        name: req.name.clone(),
        service_type: parse_service_type(&req.service_type),
        url: req.url,
        description: req.description,
        registered_at: now,
        last_heartbeat: now,
        healthy: true,
    };

    state
        .store
        .save_sidecar_async(sidecar)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to register service: {e}")))?;

    tracing::info!(name = %req.name, "Service registered");

    Ok(Json(RegisterServiceResponse {
        name: req.name,
        registered: true,
    }))
}

/// GET /services/{name} - Get a specific service.
async fn services_get(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<ServiceResponse>, AppError> {
    let state = state.read().await;

    let sidecar = state
        .store
        .get_sidecar_async(name.clone())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to get service: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Service '{name}' not found")))?;

    Ok(Json(ServiceResponse::from(&sidecar)))
}

/// DELETE /services/{name} - Unregister a service.
async fn services_delete(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<DeleteServiceResponse>, AppError> {
    let state = state.read().await;

    let deleted = state
        .store
        .remove_sidecar_async(name.clone())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to delete service: {e}")))?;

    if deleted {
        tracing::info!(name = %name, "Service unregistered");
    }

    Ok(Json(DeleteServiceResponse { name, deleted }))
}

/// POST /services/{name}/heartbeat - Update service heartbeat.
async fn services_heartbeat(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<HeartbeatResponse>, AppError> {
    let state = state.read().await;

    let updated = state
        .store
        .update_sidecar_heartbeat_async(name.clone(), true)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to update heartbeat: {e}")))?;

    if !updated {
        return Err(AppError::NotFound(format!("Service '{name}' not found")));
    }

    Ok(Json(HeartbeatResponse { name, updated }))
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
enum AppError {
    NotFound(String),
    BadRequest(String),
    Conflict(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        (status, Json(ErrorResponse { error: message })).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::Internal(err.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Internal(err.to_string())
    }
}

impl From<daemon_error::Error> for AppError {
    fn from(err: daemon_error::Error) -> Self {
        let status = err.status_code();
        let message = err.to_string();
        match status {
            404 => AppError::NotFound(message),
            400 => AppError::BadRequest(message),
            409 => AppError::Conflict(message),
            _ => AppError::Internal(message),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

// Use shared utility for duration formatting
use crate::utils::format_duration;

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
        let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
        let sql = SqlService::open(data_dir.join("sql.db")).unwrap();
        let storage = StorageService::open(data_dir.join("storage")).unwrap();
        let queue = QueueService::new(QueueConfig::default()).unwrap();
        let cron = CronScheduler::new().await.unwrap();

        let app_state = Arc::new(RwLock::new(AppState {
            store,
            kv,
            sql,
            storage,
            queue,
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
            // Queue service
            .route("/queues", get(queue_list))
            .route("/queues/{name}", get(queue_info))
            .route("/queues/{name}", delete(queue_delete))
            .route("/queues/{name}/push", post(queue_push))
            .route("/queues/{name}/pop", post(queue_pop))
            .route("/queues/{name}/peek", get(queue_peek))
            .route("/topics/{name}/publish", post(topic_publish))
            // Service discovery
            .route("/services", get(services_list).post(services_register))
            .route(
                "/services/{name}",
                get(services_get).delete(services_delete),
            )
            .route("/services/{name}/heartbeat", post(services_heartbeat))
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

    #[tokio::test]
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
    // Queue Service Tests
    // =========================================================================

    #[tokio::test]
    async fn test_queue_push_and_pop() {
        let app = create_test_app().await;

        // Push a message
        let push_request = Request::builder()
            .method(Method::POST)
            .uri("/queues/test-queue/push")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"payload": {"message": "hello"}}"#))
            .unwrap();

        let response = app.clone().oneshot(push_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Pop the message
        let pop_request = Request::builder()
            .method(Method::POST)
            .uri("/queues/test-queue/pop")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(pop_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let pop_response: QueuePopResponse = serde_json::from_slice(&body).unwrap();
        assert!(pop_response.message.is_some());
        let msg = pop_response.message.unwrap();
        assert!(!msg.id.is_empty());
    }

    #[tokio::test]
    async fn test_queue_peek() {
        let app = create_test_app().await;

        // Push a message
        let push_request = Request::builder()
            .method(Method::POST)
            .uri("/queues/peek-queue/push")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"payload": "test message"}"#))
            .unwrap();

        app.clone().oneshot(push_request).await.unwrap();

        // Peek (should not remove)
        let peek_request = Request::builder()
            .uri("/queues/peek-queue/peek")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(peek_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let peek_response: QueuePeekResponse = serde_json::from_slice(&body).unwrap();
        assert!(peek_response.message.is_some());

        // Peek again (message should still be there)
        let peek_request = Request::builder()
            .uri("/queues/peek-queue/peek")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(peek_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let peek_response: QueuePeekResponse = serde_json::from_slice(&body).unwrap();
        assert!(peek_response.message.is_some());
    }

    #[tokio::test]
    async fn test_queue_pop_empty() {
        let app = create_test_app().await;

        // Pop from non-existent queue
        let pop_request = Request::builder()
            .method(Method::POST)
            .uri("/queues/empty-queue/pop")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(pop_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let pop_response: QueuePopResponse = serde_json::from_slice(&body).unwrap();
        assert!(pop_response.message.is_none());
    }

    #[tokio::test]
    async fn test_queue_info() {
        let app = create_test_app().await;

        // Push some messages
        for i in 0..3 {
            let push_request = Request::builder()
                .method(Method::POST)
                .uri("/queues/info-queue/push")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"payload": {i}}}"#)))
                .unwrap();

            app.clone().oneshot(push_request).await.unwrap();
        }

        // Get queue info
        let info_request = Request::builder()
            .uri("/queues/info-queue")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(info_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let info_response: QueueInfoResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(info_response.name, "info-queue");
        assert_eq!(info_response.length, 3);
    }

    #[tokio::test]
    async fn test_queue_list() {
        let app = create_test_app().await;

        // Create multiple queues
        for name in ["queue-a", "queue-b", "queue-c"] {
            let push_request = Request::builder()
                .method(Method::POST)
                .uri(format!("/queues/{name}/push"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"payload": "msg"}"#))
                .unwrap();

            app.clone().oneshot(push_request).await.unwrap();
        }

        // List queues
        let list_request = Request::builder()
            .uri("/queues")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(list_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_response: ListQueuesResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list_response.queues.len(), 3);
    }

    #[tokio::test]
    async fn test_queue_delete() {
        let app = create_test_app().await;

        // Create a queue
        let push_request = Request::builder()
            .method(Method::POST)
            .uri("/queues/delete-queue/push")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"payload": "to delete"}"#))
            .unwrap();

        app.clone().oneshot(push_request).await.unwrap();

        // Delete the queue
        let delete_request = Request::builder()
            .method(Method::DELETE)
            .uri("/queues/delete-queue")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(delete_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify queue is empty (info should return 0 length)
        let info_request = Request::builder()
            .uri("/queues/delete-queue")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(info_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let info_response: QueueInfoResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(info_response.length, 0);
    }

    #[tokio::test]
    async fn test_topic_publish() {
        let app = create_test_app().await;

        // Publish to a topic (no subscribers, should still succeed)
        let publish_request = Request::builder()
            .method(Method::POST)
            .uri("/topics/test-topic/publish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"payload": {"event": "test"}}"#))
            .unwrap();

        let response = app.oneshot(publish_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // =========================================================================
    // Error Handling Tests
    // =========================================================================

    #[tokio::test]
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
    // Service Discovery Tests
    // =========================================================================

    #[tokio::test]
    async fn test_services_list_empty() {
        let app = create_test_app().await;

        let request = Request::builder()
            .uri("/services")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
        assert!(list.services.is_empty());
    }

    #[tokio::test]
    async fn test_services_register_and_get() {
        let app = create_test_app().await;

        // Register a service
        let register_request = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "test-sidecar",
                "service_type": "sql",
                "url": "http://localhost:9001",
                "description": "Test SQL sidecar"
            }"#,
            ))
            .unwrap();

        let response = app.clone().oneshot(register_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let register_resp: RegisterServiceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(register_resp.name, "test-sidecar");
        assert!(register_resp.registered);

        // Get the service
        let get_request = Request::builder()
            .uri("/services/test-sidecar")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let service: ServiceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(service.name, "test-sidecar");
        assert_eq!(service.service_type, "sql");
        assert_eq!(service.url, "http://localhost:9001");
        assert_eq!(service.description, Some("Test SQL sidecar".to_string()));
        assert!(service.healthy);
    }

    #[tokio::test]
    async fn test_services_list_with_filter() {
        let app = create_test_app().await;

        // Register a SQL service
        let register_sql = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "sql-sidecar",
                "service_type": "sql",
                "url": "http://localhost:9001"
            }"#,
            ))
            .unwrap();
        app.clone().oneshot(register_sql).await.unwrap();

        // Register a KV service
        let register_kv = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "kv-sidecar",
                "service_type": "kv",
                "url": "http://localhost:9002"
            }"#,
            ))
            .unwrap();
        app.clone().oneshot(register_kv).await.unwrap();

        // List all services
        let list_all = Request::builder()
            .uri("/services")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(list_all).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.services.len(), 2);

        // Filter by SQL type
        let list_sql = Request::builder()
            .uri("/services?service_type=sql")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(list_sql).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.services.len(), 1);
        assert_eq!(list.services[0].service_type, "sql");

        // Filter by KV type
        let list_kv = Request::builder()
            .uri("/services?service_type=kv")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(list_kv).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.services.len(), 1);
        assert_eq!(list.services[0].service_type, "kv");
    }

    #[tokio::test]
    async fn test_services_get_not_found() {
        let app = create_test_app().await;

        let request = Request::builder()
            .uri("/services/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_services_delete() {
        let app = create_test_app().await;

        // Register a service
        let register_request = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "to-delete",
                "service_type": "storage",
                "url": "http://localhost:9003"
            }"#,
            ))
            .unwrap();
        app.clone().oneshot(register_request).await.unwrap();

        // Verify it exists
        let get_request = Request::builder()
            .uri("/services/to-delete")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Delete the service
        let delete_request = Request::builder()
            .method(Method::DELETE)
            .uri("/services/to-delete")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(delete_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let delete_resp: DeleteServiceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(delete_resp.name, "to-delete");
        assert!(delete_resp.deleted);

        // Verify it's gone
        let get_request = Request::builder()
            .uri("/services/to-delete")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_services_delete_nonexistent() {
        let app = create_test_app().await;

        let request = Request::builder()
            .method(Method::DELETE)
            .uri("/services/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let delete_resp: DeleteServiceResponse = serde_json::from_slice(&body).unwrap();
        assert!(!delete_resp.deleted);
    }

    #[tokio::test]
    async fn test_services_heartbeat() {
        let app = create_test_app().await;

        // Register a service
        let register_request = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "heartbeat-test",
                "service_type": "queue",
                "url": "http://localhost:9004"
            }"#,
            ))
            .unwrap();
        app.clone().oneshot(register_request).await.unwrap();

        // Send heartbeat
        let heartbeat_request = Request::builder()
            .method(Method::POST)
            .uri("/services/heartbeat-test/heartbeat")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(heartbeat_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let heartbeat_resp: HeartbeatResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(heartbeat_resp.name, "heartbeat-test");
        assert!(heartbeat_resp.updated);
    }

    #[tokio::test]
    async fn test_services_heartbeat_not_found() {
        let app = create_test_app().await;

        let request = Request::builder()
            .method(Method::POST)
            .uri("/services/nonexistent/heartbeat")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_services_register_invalid_url() {
        let app = create_test_app().await;

        let request = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "bad-url",
                "service_type": "sql",
                "url": "not-a-valid-url"
            }"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_services_custom_type() {
        let app = create_test_app().await;

        // Register with custom type
        let register_request = Request::builder()
            .method(Method::POST)
            .uri("/services")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                "name": "custom-sidecar",
                "service_type": "custom:metrics",
                "url": "http://localhost:9005"
            }"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(register_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify the type
        let get_request = Request::builder()
            .uri("/services/custom-sidecar")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(get_request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let service: ServiceResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(service.service_type, "custom:metrics");
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
        let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
        let sql = SqlService::open(data_dir.join("sql.db")).unwrap();
        let storage = StorageService::open(data_dir.join("storage")).unwrap();
        let queue = QueueService::new(QueueConfig::default()).unwrap();
        let cron = CronScheduler::new().await.unwrap();

        let app_state = Arc::new(RwLock::new(AppState {
            store,
            kv,
            sql,
            storage,
            queue,
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
        if key.is_some() {
            // If set, ensure it's not empty (our filter)
            assert!(!key.unwrap().is_empty());
        }
    }
}
