//! Type definitions for the cron scheduler.
//!
//! Contains configuration types, execution records, and job state structures.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// Maximum number of history entries to keep per job.
pub const MAX_HISTORY_ENTRIES: usize = 100;

/// A scheduled job configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    /// Unique name for this schedule.
    pub name: String,
    /// Path to the WASM module to execute (module name, e.g., "cleanup").
    pub module: PathBuf,
    /// Cron expression (e.g., "0 0 * * *" for daily at midnight).
    pub cron: String,
    /// Optional HTTP method to use (default: GET).
    #[serde(default = "default_method")]
    pub method: String,
    /// Optional path to call (default: /).
    #[serde(default = "default_path")]
    pub path: String,
    /// Whether the job is enabled (default: true).
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Port of the mik instance to call (default: 3000).
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional request body (JSON) to send with the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    /// Optional request headers to send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// Health check path (default: /health).
    /// Override this if your WASM module has a different health endpoint.
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

/// Default port value (3000).
pub const fn default_port() -> u16 {
    3000
}

/// Default health check path ("/health").
pub fn default_health_path() -> String {
    "/health".to_string()
}

/// Default HTTP method ("GET").
pub fn default_method() -> String {
    "GET".to_string()
}

/// Default path ("/").
pub fn default_path() -> String {
    "/".to_string()
}

/// Default enabled state (true).
pub const fn default_enabled() -> bool {
    true
}

/// Result of a job execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobExecution {
    /// Unique execution ID.
    pub id: String,
    /// Job name.
    pub job_name: String,
    /// When the execution started.
    pub started_at: DateTime<Utc>,
    /// When the execution completed (None if still running).
    pub completed_at: Option<DateTime<Utc>>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Whether the execution succeeded.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
    /// Whether this was a manual trigger.
    pub manual: bool,
}

/// Information about a scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    /// Job name.
    pub name: String,
    /// Cron expression.
    pub cron: String,
    /// Module path.
    pub module: String,
    /// HTTP method (GET, POST, etc.).
    pub method: String,
    /// Request path.
    pub path: String,
    /// Whether the job is enabled.
    pub enabled: bool,
    /// Next scheduled run time.
    pub next_run: Option<DateTime<Utc>>,
    /// Last execution result.
    pub last_execution: Option<JobExecution>,
    /// Total number of executions.
    pub execution_count: u64,
    /// Number of successful executions.
    pub success_count: u64,
    /// Number of failed executions.
    pub failure_count: u64,
}

/// Internal job state.
pub(crate) struct JobState {
    pub config: ScheduleConfig,
    pub job_id: Option<uuid::Uuid>,
    /// Execution history stored as `VecDeque` for O(1) front removal.
    pub history: VecDeque<JobExecution>,
    pub execution_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
}

impl JobState {
    /// Creates a new `JobState` with the given configuration.
    pub const fn new(config: ScheduleConfig, job_id: Option<uuid::Uuid>) -> Self {
        Self {
            config,
            job_id,
            history: VecDeque::new(),
            execution_count: 0,
            success_count: 0,
            failure_count: 0,
        }
    }

    /// Converts this state to a `JobInfo` for external consumption.
    pub fn to_job_info(&self) -> JobInfo {
        JobInfo {
            name: self.config.name.clone(),
            cron: self.config.cron.clone(),
            module: self.config.module.display().to_string(),
            method: self.config.method.clone(),
            path: self.config.path.clone(),
            enabled: self.config.enabled,
            next_run: None, // Would need scheduler API to get this
            last_execution: self.history.back().cloned(),
            execution_count: self.execution_count,
            success_count: self.success_count,
            failure_count: self.failure_count,
        }
    }
}

/// Type alias for the jobs map used throughout the scheduler.
pub(crate) type JobsMap = HashMap<String, JobState>;
