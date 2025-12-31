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
//!
//! # Module Structure
//!
//! - `types` - Type definitions (`ScheduleConfig`, `JobExecution`, `JobInfo`)
//! - `manifest` - Manifest parsing for `[[schedules]]`
//! - `scheduler` - Core `CronScheduler` implementation
//! - `execution` - Job execution and health check logic

mod execution;
mod manifest;
mod scheduler;
mod types;

// Re-export public API for backward compatibility
pub use manifest::parse_schedules_from_manifest;
pub use scheduler::CronScheduler;
pub use types::{JobExecution, ScheduleConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
