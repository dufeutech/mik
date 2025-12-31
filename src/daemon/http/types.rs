//! Request and response types for the HTTP API.
//!
//! This module contains all the request/response types used by the daemon HTTP API handlers.

use crate::daemon::services::storage::ObjectMeta;
use crate::daemon::state::{Instance, Sidecar, Status};
use serde::{Deserialize, Serialize};

// =============================================================================
// Instance Types
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
pub fn validate_instance_name(name: &str) -> Result<(), String> {
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

// =============================================================================
// Common Types
// =============================================================================

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

const fn default_enabled() -> bool {
    true
}

const fn default_port() -> u16 {
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
// Utilities
// =============================================================================

/// Format a chrono duration as a human-readable string.
pub fn format_duration(duration: chrono::Duration) -> String {
    let total_secs = duration.num_seconds();
    if total_secs < 0 {
        return "0s".to_string();
    }
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {secs}s")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}
