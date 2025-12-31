//! Core cron scheduler implementation.
//!
//! Provides the `CronScheduler` service for managing scheduled WASM jobs.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_cron_scheduler::{Job, JobScheduler};

use super::execution::{execute_job, execute_job_internal};
use super::types::{JobExecution, JobInfo, JobState, JobsMap, MAX_HISTORY_ENTRIES, ScheduleConfig};

/// Cron scheduler service.
pub struct CronScheduler {
    scheduler: JobScheduler,
    jobs: Arc<RwLock<JobsMap>>,
}

impl CronScheduler {
    /// Creates a new cron scheduler.
    pub async fn new() -> Result<Self> {
        let scheduler = JobScheduler::new()
            .await
            .context("Failed to create job scheduler")?;

        Ok(Self {
            scheduler,
            jobs: Arc::new(RwLock::new(JobsMap::new())),
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
                execute_job_internal(&jobs, &name).await;
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
        jobs.insert(name.clone(), JobState::new(config, Some(job_id)));

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
        jobs.values().map(JobState::to_job_info).collect()
    }

    /// Gets information about a specific job.
    pub async fn get_job(&self, name: &str) -> Option<JobInfo> {
        let jobs = self.jobs.read().await;
        jobs.get(name).map(JobState::to_job_info)
    }

    /// Gets execution history for a job (in-memory, faster than database).
    #[allow(dead_code)] // HTTP handler uses database history for persistence; kept for future optimization
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
        let execution = execute_job(&self.jobs, name, true).await;

        // Check if the job was actually found and executed
        if !execution.success && execution.error.as_deref() == Some("Job not found") {
            anyhow::bail!("Job '{name}' not found");
        }

        Ok(execution)
    }
}
