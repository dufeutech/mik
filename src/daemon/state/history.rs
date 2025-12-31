//! Job execution history storage operations for the state store.
//!
//! Provides operations for storing and querying cron job execution history.

use anyhow::{Context, Result};
use redb::ReadableDatabase;

use crate::daemon::cron::JobExecution;

use super::{CRON_HISTORY_TABLE, StateStore};

impl StateStore {
    /// Saves a job execution to the history.
    pub fn save_job_execution(&self, execution: &JobExecution) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(CRON_HISTORY_TABLE)
                .context("Failed to open cron_history table")?;

            // Key format: "job_name:timestamp:execution_id" for chronological ordering
            let key = format!(
                "{}:{}:{}",
                execution.job_name,
                execution.started_at.timestamp(),
                execution.id
            );
            let json =
                serde_json::to_vec(execution).context("Failed to serialize job execution")?;

            table
                .insert(key.as_str(), json.as_slice())
                .context("Failed to insert job execution")?;
        }

        write_txn
            .commit()
            .context("Failed to commit job execution transaction")?;

        Ok(())
    }

    /// Lists job executions for a specific job, ordered by most recent first.
    ///
    /// Uses an efficient range query on the key prefix instead of scanning all entries.
    pub fn list_job_executions(&self, job_name: &str, limit: usize) -> Result<Vec<JobExecution>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(CRON_HISTORY_TABLE)
            .context("Failed to open cron_history table")?;

        // Key format is "job_name:timestamp:execution_id"
        // Use range query from "job_name:" to "job_name;\xff" (semicolon is after colon in ASCII)
        let start_key = format!("{job_name}:");
        let end_key = format!("{job_name};"); // ';' is ASCII 59, ':' is ASCII 58

        let mut executions: Vec<JobExecution> = Vec::new();

        // Use range query for efficient prefix scanning
        for entry in table
            .range(start_key.as_str()..end_key.as_str())
            .context("Failed to query cron_history table")?
        {
            let (key, value) = entry.context("Failed to read cron_history entry")?;
            let key_str = key.value();

            let execution: JobExecution = serde_json::from_slice(value.value())
                .with_context(|| format!("Failed to deserialize job execution: {key_str}"))?;
            executions.push(execution);
        }

        // Sort by started_at descending (most recent first)
        executions.sort_by(|a, b| b.started_at.cmp(&a.started_at));

        // Apply limit
        executions.truncate(limit);

        Ok(executions)
    }

    /// Cleans up old job executions, keeping only the most recent N per job.
    pub fn cleanup_job_executions(&self, job_name: &str, keep: usize) -> Result<usize> {
        let executions = self.list_job_executions(job_name, usize::MAX)?;

        if executions.len() <= keep {
            return Ok(0);
        }

        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        let mut removed = 0;
        {
            let mut table = write_txn
                .open_table(CRON_HISTORY_TABLE)
                .context("Failed to open cron_history table")?;

            // Remove executions beyond the keep limit
            for execution in executions.iter().skip(keep) {
                let key = format!(
                    "{}:{}:{}",
                    execution.job_name,
                    execution.started_at.timestamp(),
                    execution.id
                );
                if table.remove(key.as_str())?.is_some() {
                    removed += 1;
                }
            }
        }

        write_txn
            .commit()
            .context("Failed to commit cleanup transaction")?;

        Ok(removed)
    }

    /// Saves a job execution asynchronously.
    pub async fn save_job_execution_async(&self, execution: JobExecution) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.save_job_execution(&execution))
            .await
            .context("Task join error")?
    }

    /// Lists job executions asynchronously.
    pub async fn list_job_executions_async(
        &self,
        job_name: String,
        limit: usize,
    ) -> Result<Vec<JobExecution>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list_job_executions(&job_name, limit))
            .await
            .context("Task join error")?
    }

    /// Cleans up old job executions asynchronously.
    pub async fn cleanup_job_executions_async(
        &self,
        job_name: String,
        keep: usize,
    ) -> Result<usize> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.cleanup_job_executions(&job_name, keep))
            .await
            .context("Task join error")?
    }
}
