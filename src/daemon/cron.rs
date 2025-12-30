//! Cron scheduler for scheduled WASM job execution.
//!
//! Provides cron-based scheduling for WASM modules using `tokio-cron-scheduler`.
//! Jobs are defined in mik.toml and can be managed via HTTP API.
//!
//! # Cron Expression Format
//!
//! Uses 7-field format: `sec min hour day month weekday year`
//!
//! # Example Configuration
//!
//! ```toml
//! [[schedules]]
//! name = "cleanup"
//! module = "modules/cleanup.wasm"
//! cron = "0 0 0 * * * *"  # Daily at midnight
//!
//! [[schedules]]
//! name = "sync"
//! module = "modules/sync.wasm"
//! cron = "0 */5 * * * * *"  # Every 5 minutes
//! ```
//!
//! # HTTP API
//!
//! - `GET /cron` - List all scheduled jobs
//! - `GET /cron/:name` - Get job details
//! - `POST /cron/:name/trigger` - Manually trigger a job
//! - `GET /cron/:name/history` - Get execution history

// Allow unused - cron scheduler for future scheduled job support
#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

/// Maximum number of history entries to keep per job.
const MAX_HISTORY_ENTRIES: usize = 100;

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

fn default_port() -> u16 {
    3000
}

fn default_health_path() -> String {
    "/health".to_string()
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_path() -> String {
    "/".to_string()
}

fn default_enabled() -> bool {
    true
}

/// Partial manifest for parsing [[schedules]] from mik.toml.
#[derive(Debug, Default, Deserialize)]
struct SchedulesManifest {
    #[serde(default)]
    schedules: Vec<ScheduleConfig>,
}

/// Parse [[schedules]] from a mik.toml file.
///
/// Returns an empty Vec if the file doesn't exist or has no schedules.
pub fn parse_schedules_from_manifest(path: &std::path::Path) -> Result<Vec<ScheduleConfig>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let manifest: SchedulesManifest =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;

    Ok(manifest.schedules)
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
struct JobState {
    config: ScheduleConfig,
    job_id: Option<uuid::Uuid>,
    /// Execution history stored as `VecDeque` for O(1) front removal.
    history: VecDeque<JobExecution>,
    execution_count: u64,
    success_count: u64,
    failure_count: u64,
}

/// Cron scheduler service.
pub struct CronScheduler {
    scheduler: JobScheduler,
    jobs: Arc<RwLock<HashMap<String, JobState>>>,
}

impl CronScheduler {
    /// Creates a new cron scheduler.
    pub async fn new() -> Result<Self> {
        let scheduler = JobScheduler::new()
            .await
            .context("Failed to create job scheduler")?;

        Ok(Self {
            scheduler,
            jobs: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Starts the scheduler.
    pub async fn start(&self) -> Result<()> {
        self.scheduler
            .start()
            .await
            .context("Failed to start scheduler")?;
        tracing::info!("Cron scheduler started");
        Ok(())
    }

    /// Stops the scheduler.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.scheduler
            .shutdown()
            .await
            .context("Failed to shutdown scheduler")?;
        tracing::info!("Cron scheduler stopped");
        Ok(())
    }

    /// Adds a scheduled job.
    pub async fn add_job(&self, config: ScheduleConfig) -> Result<()> {
        let name = config.name.clone();
        let cron_expr = config.cron.clone();

        // Validate cron expression by attempting to create a job
        let jobs_clone = Arc::clone(&self.jobs);
        let job_name = name.clone();

        let job = Job::new_async(cron_expr.as_str(), move |_uuid, _lock| {
            let jobs = Arc::clone(&jobs_clone);
            let name = job_name.clone();
            Box::pin(async move {
                Self::execute_job_internal(&jobs, &name).await;
            })
        })
        .with_context(|| format!("Invalid cron expression: {cron_expr}"))?;

        let job_id = self
            .scheduler
            .add(job)
            .await
            .context("Failed to add job to scheduler")?;

        // Store job state
        let mut jobs = self.jobs.write().await;
        jobs.insert(
            name.clone(),
            JobState {
                config,
                job_id: Some(job_id),
                history: VecDeque::new(),
                execution_count: 0,
                success_count: 0,
                failure_count: 0,
            },
        );

        tracing::info!(job = %name, cron = %cron_expr, "Scheduled job added");
        Ok(())
    }

    /// Removes a scheduled job.
    pub async fn remove_job(&self, name: &str) -> Result<bool> {
        let mut jobs = self.jobs.write().await;

        if let Some(state) = jobs.remove(name) {
            if let Some(job_id) = state.job_id {
                self.scheduler
                    .remove(&job_id)
                    .await
                    .context("Failed to remove job from scheduler")?;
            }
            tracing::info!(job = %name, "Scheduled job removed");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Sets the enabled state of a job (pause/resume).
    ///
    /// Returns the new enabled state, or None if job not found.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Option<bool> {
        let mut jobs = self.jobs.write().await;

        if let Some(state) = jobs.get_mut(name) {
            state.config.enabled = enabled;
            let action = if enabled { "resumed" } else { "paused" };
            tracing::info!(job = %name, enabled = enabled, "Job {action}");
            Some(enabled)
        } else {
            None
        }
    }

    /// Gets the current config for a job.
    pub async fn get_config(&self, name: &str) -> Option<ScheduleConfig> {
        let jobs = self.jobs.read().await;
        jobs.get(name).map(|state| state.config.clone())
    }

    /// Lists all scheduled jobs.
    pub async fn list_jobs(&self) -> Vec<JobInfo> {
        let jobs = self.jobs.read().await;

        jobs.values()
            .map(|state| JobInfo {
                name: state.config.name.clone(),
                cron: state.config.cron.clone(),
                module: state.config.module.display().to_string(),
                enabled: state.config.enabled,
                next_run: None, // Would need scheduler API to get this
                last_execution: state.history.back().cloned(),
                execution_count: state.execution_count,
                success_count: state.success_count,
                failure_count: state.failure_count,
            })
            .collect()
    }

    /// Gets information about a specific job.
    pub async fn get_job(&self, name: &str) -> Option<JobInfo> {
        let jobs = self.jobs.read().await;

        jobs.get(name).map(|state| JobInfo {
            name: state.config.name.clone(),
            cron: state.config.cron.clone(),
            module: state.config.module.display().to_string(),
            enabled: state.config.enabled,
            next_run: None,
            last_execution: state.history.back().cloned(),
            execution_count: state.execution_count,
            success_count: state.success_count,
            failure_count: state.failure_count,
        })
    }

    /// Gets execution history for a job.
    pub async fn get_history(&self, name: &str, limit: Option<usize>) -> Option<Vec<JobExecution>> {
        let jobs = self.jobs.read().await;

        jobs.get(name).map(|state| {
            let limit = limit.unwrap_or(50).min(MAX_HISTORY_ENTRIES);
            state.history.iter().rev().take(limit).cloned().collect()
        })
    }

    /// Manually triggers a job execution.
    pub async fn trigger_job(&self, name: &str) -> Result<JobExecution> {
        // Execute the job - execute_job handles the case where job doesn't exist
        let execution = self.execute_job(name, true).await;

        // Check if the job was actually found and executed
        if !execution.success && execution.error.as_deref() == Some("Job not found") {
            anyhow::bail!("Job '{name}' not found");
        }

        Ok(execution)
    }

    /// Executes a job and records the result.
    async fn execute_job(&self, name: &str, manual: bool) -> JobExecution {
        let execution_id = Uuid::new_v4().to_string();
        let started_at = Utc::now();

        // Get job config
        let config = {
            let jobs = self.jobs.read().await;
            jobs.get(name).map(|s| s.config.clone())
        };

        let (success, error, duration_ms) = match config {
            Some(config) => {
                let start = std::time::Instant::now();
                let result = Self::execute_wasm_module(&config).await;
                #[allow(clippy::cast_possible_truncation)]
                let duration = start.elapsed().as_millis() as u64;

                match result {
                    Ok(()) => (true, None, duration),
                    Err(e) => (false, Some(e.to_string()), duration),
                }
            },
            None => (false, Some("Job not found".to_string()), 0),
        };

        let completed_at = Utc::now();

        // Log before creating execution (to avoid borrowing issues)
        if success {
            tracing::info!(
                job = %name,
                duration_ms = duration_ms,
                manual = manual,
                "Job executed successfully"
            );
        } else {
            tracing::warn!(
                job = %name,
                duration_ms = duration_ms,
                manual = manual,
                error = ?error,
                "Job execution failed"
            );
        }

        let execution = JobExecution {
            id: execution_id,
            job_name: name.to_string(),
            started_at,
            completed_at: Some(completed_at),
            duration_ms: Some(duration_ms),
            success,
            error,
            manual,
        };

        // Record execution in history
        let mut jobs = self.jobs.write().await;
        if let Some(state) = jobs.get_mut(name) {
            state.execution_count += 1;
            if success {
                state.success_count += 1;
            } else {
                state.failure_count += 1;
            }

            // Trim history if at capacity (O(1) with VecDeque)
            if state.history.len() >= MAX_HISTORY_ENTRIES {
                state.history.pop_front();
            }
            state.history.push_back(execution.clone());
        }

        execution
    }

    /// Internal job execution (called by scheduler).
    async fn execute_job_internal(jobs: &Arc<RwLock<HashMap<String, JobState>>>, name: &str) {
        let execution_id = Uuid::new_v4().to_string();
        let started_at = Utc::now();

        // Get job config
        let config = {
            let jobs_guard = jobs.read().await;
            jobs_guard.get(name).map(|s| s.config.clone())
        };

        let (success, error, duration_ms) = match config {
            Some(config) => {
                if !config.enabled {
                    return; // Skip disabled jobs
                }

                let start = std::time::Instant::now();
                let result = Self::execute_wasm_module(&config).await;
                #[allow(clippy::cast_possible_truncation)]
                let duration = start.elapsed().as_millis() as u64;

                match result {
                    Ok(()) => (true, None, duration),
                    Err(e) => (false, Some(e.to_string()), duration),
                }
            },
            None => return,
        };

        let completed_at = Utc::now();

        let execution = JobExecution {
            id: execution_id,
            job_name: name.to_string(),
            started_at,
            completed_at: Some(completed_at),
            duration_ms: Some(duration_ms),
            success,
            error: error.clone(),
            manual: false,
        };

        // Record execution
        let mut jobs_guard = jobs.write().await;
        if let Some(state) = jobs_guard.get_mut(name) {
            state.execution_count += 1;
            if success {
                state.success_count += 1;
            } else {
                state.failure_count += 1;
            }

            // Trim history if at capacity (O(1) with VecDeque)
            if state.history.len() >= MAX_HISTORY_ENTRIES {
                state.history.pop_front();
            }
            state.history.push_back(execution);
        }

        if success {
            tracing::info!(job = %name, duration_ms = duration_ms, "Scheduled job executed");
        } else {
            tracing::warn!(job = %name, error = ?error, "Scheduled job failed");
        }
    }

    /// Waits for an instance to become healthy with retry and backoff.
    ///
    /// Attempts up to `max_attempts` health checks with exponential backoff,
    /// starting at 500ms and doubling each attempt (500ms, 1s, 2s, 4s, 8s).
    async fn wait_for_healthy(
        client: &reqwest::Client,
        port: u16,
        health_path: &str,
        max_attempts: u32,
    ) -> Result<()> {
        // Ensure health_path starts with /
        let path = if health_path.starts_with('/') {
            health_path.to_string()
        } else {
            format!("/{health_path}")
        };
        let health_url = format!("http://localhost:{port}{path}");
        let mut last_error = String::new();

        for attempt in 1..=max_attempts {
            let health_response = client
                .get(&health_url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await;

            match health_response {
                Ok(resp) if resp.status().is_success() => {
                    if attempt > 1 {
                        tracing::debug!(
                            port = port,
                            attempt = attempt,
                            "Instance health check passed after retry"
                        );
                    } else {
                        tracing::debug!(port = port, "Instance health check passed");
                    }
                    return Ok(());
                },
                Ok(resp) => {
                    last_error = format!("status {}", resp.status().as_u16());
                },
                Err(e) => {
                    last_error = e.to_string();
                },
            }

            if attempt < max_attempts {
                // Exponential backoff: 500ms, 1s, 2s, 4s, 8s
                let backoff_ms = 500 * 2u64.pow(attempt - 1);
                let backoff = std::time::Duration::from_millis(backoff_ms.min(8000));
                tracing::debug!(
                    port = port,
                    attempt = attempt,
                    backoff_ms = backoff.as_millis(),
                    "Health check failed, retrying..."
                );
                tokio::time::sleep(backoff).await;
            }
        }

        anyhow::bail!(
            "Instance not healthy on port {port} after {max_attempts} attempts: {last_error}"
        )
    }

    /// Executes a WASM module by calling the running mik instance via HTTP.
    ///
    /// First performs a health check with retries, then makes an HTTP request to
    /// `http://localhost:{port}/run/{module}{path}`.
    async fn execute_wasm_module(config: &ScheduleConfig) -> Result<()> {
        // Extract module name from path (e.g., "modules/cleanup.wasm" -> "cleanup")
        let module_name = config
            .module
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid module path: {}", config.module.display()))?;

        // Create HTTP client with timeout
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        // Health check with retries: verify the instance is running before executing
        // Uses up to 5 attempts with exponential backoff (total wait ~15s max)
        Self::wait_for_healthy(&client, config.port, &config.health_path, 5).await?;

        // Build the URL: http://localhost:{port}/run/{module}{path}
        let url = format!(
            "http://localhost:{}/run/{}{}",
            config.port, module_name, config.path
        );

        // Build the request based on method
        let mut request = match config.method.to_uppercase().as_str() {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            "PATCH" => client.patch(&url),
            other => anyhow::bail!("Unsupported HTTP method: {other}"),
        };

        // Add custom headers if provided
        if let Some(headers) = &config.headers {
            for (key, value) in headers {
                request = request.header(key, value);
            }
        }

        // Add body if provided (sets Content-Type: application/json)
        if let Some(body) = &config.body {
            request = request.json(body);
        }

        // Send the request
        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to call module at {url}"))?;

        // Check response status
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            anyhow::bail!(
                "Module returned error status {}: {}",
                status.as_u16(),
                body.chars().take(200).collect::<String>()
            );
        }

        tracing::debug!(
            module = %module_name,
            url = %url,
            status = %status.as_u16(),
            "Cron job executed successfully"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scheduler_creation() {
        let scheduler = CronScheduler::new().await.unwrap();
        assert!(scheduler.list_jobs().await.is_empty());
    }

    #[tokio::test]
    async fn test_add_and_list_job() {
        let scheduler = CronScheduler::new().await.unwrap();

        // tokio-cron-scheduler uses 7-field format: sec min hour day month weekday year
        let config = ScheduleConfig {
            name: "test-job".to_string(),
            module: PathBuf::from("modules/test.wasm"),
            cron: "0 0 * * * * *".to_string(), // Every hour at :00
            method: "GET".to_string(),
            path: "/".to_string(),
            enabled: true,
            port: 3000,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };

        scheduler.add_job(config).await.unwrap();

        let jobs = scheduler.list_jobs().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "test-job");
        assert_eq!(jobs[0].cron, "0 0 * * * * *");
    }

    #[tokio::test]
    async fn test_remove_job() {
        let scheduler = CronScheduler::new().await.unwrap();

        let config = ScheduleConfig {
            name: "to-remove".to_string(),
            module: PathBuf::from("modules/test.wasm"),
            cron: "0 0 * * * * *".to_string(), // 7-field format
            method: "GET".to_string(),
            path: "/".to_string(),
            enabled: true,
            port: 3000,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };

        scheduler.add_job(config).await.unwrap();
        assert_eq!(scheduler.list_jobs().await.len(), 1);

        let removed = scheduler.remove_job("to-remove").await.unwrap();
        assert!(removed);
        assert!(scheduler.list_jobs().await.is_empty());
    }

    #[tokio::test]
    async fn test_get_job() {
        let scheduler = CronScheduler::new().await.unwrap();

        let config = ScheduleConfig {
            name: "my-job".to_string(),
            module: PathBuf::from("modules/test.wasm"),
            cron: "0 */5 * * * * *".to_string(), // Every 5 minutes (7-field)
            method: "POST".to_string(),
            path: "/trigger".to_string(),
            enabled: true,
            port: 3000,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };

        scheduler.add_job(config).await.unwrap();

        let job = scheduler.get_job("my-job").await;
        assert!(job.is_some());
        let job = job.unwrap();
        assert_eq!(job.name, "my-job");
        assert_eq!(job.cron, "0 */5 * * * * *");

        let missing = scheduler.get_job("nonexistent").await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_invalid_cron_expression() {
        let scheduler = CronScheduler::new().await.unwrap();

        let config = ScheduleConfig {
            name: "bad-cron".to_string(),
            module: PathBuf::from("modules/test.wasm"),
            cron: "invalid cron".to_string(),
            method: "GET".to_string(),
            path: "/".to_string(),
            enabled: true,
            port: 3000,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };

        let result = scheduler.add_job(config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_trigger_job_records_failure() {
        // This test verifies that triggering a job when no server is running
        // properly records the failure in the execution history.
        let scheduler = CronScheduler::new().await.unwrap();

        let config = ScheduleConfig {
            name: "trigger-test".to_string(),
            module: PathBuf::from("modules/test.wasm"),
            cron: "0 0 0 * * * *".to_string(), // Daily at midnight (7-field)
            method: "GET".to_string(),
            path: "/".to_string(),
            enabled: true,
            port: 39999, // Port unlikely to have a running server
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };

        scheduler.add_job(config).await.unwrap();

        // Triggering should complete (not panic) even when HTTP fails
        let execution = scheduler.trigger_job("trigger-test").await.unwrap();

        // Execution should be recorded as failed (no server running)
        assert!(!execution.success);
        assert!(execution.manual);
        assert!(execution.duration_ms.is_some());
        assert!(execution.error.is_some());

        // Check history records the failure
        let history = scheduler.get_history("trigger-test", None).await.unwrap();
        assert_eq!(history.len(), 1);
        assert!(!history[0].success);
    }

    #[tokio::test]
    async fn test_trigger_nonexistent_job() {
        let scheduler = CronScheduler::new().await.unwrap();
        let result = scheduler.trigger_job("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pause_resume_job() {
        let scheduler = CronScheduler::new().await.unwrap();

        let config = ScheduleConfig {
            name: "pausable-job".to_string(),
            module: PathBuf::from("modules/test.wasm"),
            cron: "0 0 * * * * *".to_string(),
            method: "GET".to_string(),
            path: "/".to_string(),
            enabled: true,
            port: 3000,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };

        scheduler.add_job(config).await.unwrap();

        // Verify job is enabled
        let job = scheduler.get_job("pausable-job").await.unwrap();
        assert!(job.enabled);

        // Pause the job
        let result = scheduler.set_enabled("pausable-job", false).await;
        assert_eq!(result, Some(false));

        // Verify job is paused
        let job = scheduler.get_job("pausable-job").await.unwrap();
        assert!(!job.enabled);

        // Resume the job
        let result = scheduler.set_enabled("pausable-job", true).await;
        assert_eq!(result, Some(true));

        // Verify job is resumed
        let job = scheduler.get_job("pausable-job").await.unwrap();
        assert!(job.enabled);

        // Try to pause nonexistent job
        let result = scheduler.set_enabled("nonexistent", false).await;
        assert_eq!(result, None);
    }
}
