//! Job execution logic for the cron scheduler.
//!
//! Handles HTTP-based WASM module execution, health checks, and execution recording.

use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use super::types::{JobExecution, JobState, JobsMap, MAX_HISTORY_ENTRIES, ScheduleConfig};
use crate::daemon::metrics;

/// Waits for an instance to become healthy with retry and backoff.
///
/// Attempts up to `max_attempts` health checks with exponential backoff,
/// starting at 500ms and doubling each attempt (500ms, 1s, 2s, 4s, 8s).
pub(crate) async fn wait_for_healthy(
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

    anyhow::bail!("Instance not healthy on port {port} after {max_attempts} attempts: {last_error}")
}

/// Executes a WASM module by calling the running mik instance via HTTP.
///
/// First performs a health check with retries, then makes an HTTP request to
/// `http://localhost:{port}/run/{module}{path}`.
pub(crate) async fn execute_wasm_module(config: &ScheduleConfig) -> Result<()> {
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
    wait_for_healthy(&client, config.port, &config.health_path, 5).await?;

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

/// Executes a job and records the result.
///
/// This is the public execution method that records execution history.
pub(crate) async fn execute_job(
    jobs: &Arc<RwLock<JobsMap>>,
    name: &str,
    manual: bool,
) -> JobExecution {
    let execution_id = Uuid::new_v4().to_string();
    let started_at = Utc::now();

    // Get job config
    let config = {
        let jobs_guard = jobs.read().await;
        jobs_guard.get(name).map(|s| s.config.clone())
    };

    let (success, error, duration_ms) = match config {
        Some(config) => {
            let start = std::time::Instant::now();
            let result = execute_wasm_module(&config).await;
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
    record_execution(jobs, name, &execution, success).await;

    execution
}

/// Internal job execution (called by scheduler).
///
/// This is used by the cron scheduler when jobs are triggered automatically.
pub(crate) async fn execute_job_internal(
    jobs: &Arc<RwLock<HashMap<String, JobState>>>,
    name: &str,
) {
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
            let result = execute_wasm_module(&config).await;
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
    record_execution(jobs, name, &execution, success).await;

    if success {
        tracing::info!(job = %name, duration_ms = duration_ms, "Scheduled job executed");
    } else {
        tracing::warn!(job = %name, error = ?error, "Scheduled job failed");
    }
}

/// Records an execution in the job's history.
async fn record_execution(
    jobs: &Arc<RwLock<JobsMap>>,
    name: &str,
    execution: &JobExecution,
    success: bool,
) {
    // Record metrics
    #[allow(clippy::cast_precision_loss)] // Duration in ms is safe to cast
    let duration_secs = execution.duration_ms.map_or(0.0, |ms| ms as f64 / 1000.0);
    metrics::record_cron_execution(name, success, duration_secs);

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
        state.history.push_back(execution.clone());
    }
}
