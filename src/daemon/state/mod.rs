//! Persistent state management for the mik daemon using redb.
//!
//! Tracks WASM instances across daemon restarts, storing instance metadata,
//! status, and configuration in a single-file ACID database at `~/.mik/state.redb`.
//!
//! # Async Usage
//!
//! All database operations are blocking. When using from async contexts,
//! use the async methods (`save_instance_async()`, `get_instance_async()`, etc.)
//! which automatically wrap operations in `spawn_blocking` to avoid blocking
//! the async runtime.
//!
//! # Module Structure
//!
//! - `types` - Core data structures (`Instance`, `Status`, `Sidecar`, `ServiceType`)
//! - `instances` - Instance CRUD operations
//! - `cron` - Cron job configuration storage
//! - `history` - Job execution history storage
//! - `sidecars` - Sidecar service registry operations

mod cron;
mod history;
mod instances;
mod sidecars;
mod types;

pub use types::{Instance, ServiceType, Sidecar, Status};

use anyhow::{Context, Result};
use redb::{Database, TableDefinition};
use std::path::Path;
use std::sync::Arc;

/// Table name for instance storage - centralized to avoid duplication
pub(crate) const INSTANCES_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("instances");

/// Table name for cron job storage
pub(crate) const CRON_JOBS_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("cron_jobs");

/// Table name for cron job execution history
/// Key format: `job_name:execution_id`
pub(crate) const CRON_HISTORY_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("cron_history");

/// Table name for sidecar service registry
pub(crate) const SIDECARS_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("sidecars");

/// State storage interface wrapping redb database.
///
/// Provides CRUD operations for instance metadata with ACID guarantees.
/// All operations serialize to JSON for human-readable debugging in redb.
///
/// # Thread Safety
///
/// `StateStore` is `Clone` and can be shared across threads. The underlying
/// database handles concurrent access safely.
#[derive(Clone)]
pub struct StateStore {
    pub(crate) db: Arc<Database>,
}

impl StateStore {
    /// Opens or creates the state database at the given path.
    ///
    /// Creates parent directories if needed. Uses redb's ACID guarantees
    /// to prevent corruption on crashes or unclean shutdowns.
    /// Initializes the instances table on first open.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists before opening database
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory: {}", parent.display())
            })?;
        }

        let db = Database::create(path)
            .with_context(|| format!("Failed to open state database: {}", path.display()))?;

        // Initialize tables on first open to ensure they exist for reads
        let write_txn = db
            .begin_write()
            .context("Failed to begin initialization transaction")?;
        {
            let _table = write_txn
                .open_table(INSTANCES_TABLE)
                .context("Failed to initialize instances table")?;
            let _cron_table = write_txn
                .open_table(CRON_JOBS_TABLE)
                .context("Failed to initialize cron_jobs table")?;
            let _history_table = write_txn
                .open_table(CRON_HISTORY_TABLE)
                .context("Failed to initialize cron_history table")?;
            let _sidecars_table = write_txn
                .open_table(SIDECARS_TABLE)
                .context("Failed to initialize sidecars table")?;
        }
        write_txn
            .commit()
            .context("Failed to commit initialization transaction")?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Opens the state database asynchronously.
    ///
    /// This is the preferred method when calling from async contexts as it
    /// uses `spawn_blocking` to avoid blocking the async runtime.
    #[allow(dead_code)]
    pub async fn open_async<P: AsRef<Path> + Send + 'static>(path: P) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::open(path))
            .await
            .context("Task join error")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::cron::JobExecution;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_instance(name: &str, port: u16) -> Instance {
        Instance {
            name: name.to_string(),
            port,
            pid: 12345,
            status: Status::Running,
            config: PathBuf::from("/test/mik.toml"),
            started_at: Utc::now(),
            modules: vec!["api.wasm".to_string(), "hello.wasm".to_string()],
            auto_restart: false,
            restart_count: 0,
            last_restart_at: None,
        }
    }

    #[test]
    fn test_save_and_get_instance() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let instance = create_test_instance("test", 3000);
        store.save_instance(&instance).unwrap();

        let retrieved = store.get_instance("test").unwrap().unwrap();
        assert_eq!(retrieved.name, "test");
        assert_eq!(retrieved.port, 3000);
        assert_eq!(retrieved.status, Status::Running);
    }

    #[test]
    fn test_get_nonexistent_instance() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let result = store.get_instance("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_instances() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        store
            .save_instance(&create_test_instance("default", 3000))
            .unwrap();
        store
            .save_instance(&create_test_instance("dev", 3001))
            .unwrap();
        store
            .save_instance(&create_test_instance("staging", 3002))
            .unwrap();

        let instances = store.list_instances().unwrap();
        assert_eq!(instances.len(), 3);

        let names: Vec<_> = instances.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"default"));
        assert!(names.contains(&"dev"));
        assert!(names.contains(&"staging"));
    }

    #[test]
    fn test_remove_instance() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        store
            .save_instance(&create_test_instance("test", 3000))
            .unwrap();

        let removed = store.remove_instance("test").unwrap();
        assert!(removed);

        let result = store.get_instance("test").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_remove_nonexistent_instance() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let removed = store.remove_instance("nonexistent").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_update_instance() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let mut instance = create_test_instance("test", 3000);
        store.save_instance(&instance).unwrap();

        // Update status to crashed
        instance.status = Status::Crashed { exit_code: 1 };
        store.save_instance(&instance).unwrap();

        let retrieved = store.get_instance("test").unwrap().unwrap();
        assert_eq!(retrieved.status, Status::Crashed { exit_code: 1 });
    }

    #[test]
    fn test_status_serialization() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Test all status variants
        let mut instance = create_test_instance("test1", 3000);
        instance.status = Status::Running;
        store.save_instance(&instance).unwrap();

        let mut instance2 = create_test_instance("test2", 3001);
        instance2.status = Status::Stopped;
        store.save_instance(&instance2).unwrap();

        let mut instance3 = create_test_instance("test3", 3002);
        instance3.status = Status::Crashed { exit_code: 137 };
        store.save_instance(&instance3).unwrap();

        assert_eq!(
            store.get_instance("test1").unwrap().unwrap().status,
            Status::Running
        );
        assert_eq!(
            store.get_instance("test2").unwrap().unwrap().status,
            Status::Stopped
        );
        assert_eq!(
            store.get_instance("test3").unwrap().unwrap().status,
            Status::Crashed { exit_code: 137 }
        );
    }

    #[test]
    fn test_persistence_across_reopens() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");

        {
            let store = StateStore::open(&db_path).unwrap();
            store
                .save_instance(&create_test_instance("persistent", 3000))
                .unwrap();
        }

        // Reopen database and verify data persists
        {
            let store = StateStore::open(&db_path).unwrap();
            let instance = store.get_instance("persistent").unwrap().unwrap();
            assert_eq!(instance.name, "persistent");
            assert_eq!(instance.port, 3000);
        }
    }

    // =========================================================================
    // Job Execution History Tests
    // =========================================================================

    fn create_test_execution(job_name: &str, id: &str, success: bool) -> JobExecution {
        JobExecution {
            id: id.to_string(),
            job_name: job_name.to_string(),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: Some(100),
            success,
            error: if success {
                None
            } else {
                Some("Test error".to_string())
            },
            manual: false,
        }
    }

    #[test]
    fn test_save_and_list_job_executions() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save some executions
        let exec1 = create_test_execution("my-job", "exec-1", true);
        let exec2 = create_test_execution("my-job", "exec-2", false);

        store.save_job_execution(&exec1).unwrap();
        store.save_job_execution(&exec2).unwrap();

        // List executions
        let executions = store.list_job_executions("my-job", 100).unwrap();
        assert_eq!(executions.len(), 2);

        // Verify both are present
        let ids: Vec<_> = executions.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"exec-1"));
        assert!(ids.contains(&"exec-2"));
    }

    #[test]
    fn test_list_job_executions_empty() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let executions = store.list_job_executions("nonexistent-job", 100).unwrap();
        assert!(executions.is_empty());
    }

    #[test]
    fn test_list_job_executions_with_limit() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save 5 executions
        for i in 1..=5 {
            let mut exec = create_test_execution("my-job", &format!("exec-{i}"), true);
            // Stagger the timestamps so ordering is deterministic
            exec.started_at = Utc::now() + chrono::Duration::seconds(i64::from(i));
            store.save_job_execution(&exec).unwrap();
        }

        // List with limit of 3
        let executions = store.list_job_executions("my-job", 3).unwrap();
        assert_eq!(executions.len(), 3);
    }

    #[test]
    fn test_list_job_executions_sorted_by_most_recent() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save executions with different timestamps
        let mut exec1 = create_test_execution("my-job", "oldest", true);
        exec1.started_at = Utc::now() - chrono::Duration::hours(2);

        let mut exec2 = create_test_execution("my-job", "middle", true);
        exec2.started_at = Utc::now() - chrono::Duration::hours(1);

        let mut exec3 = create_test_execution("my-job", "newest", true);
        exec3.started_at = Utc::now();

        // Save in random order
        store.save_job_execution(&exec2).unwrap();
        store.save_job_execution(&exec1).unwrap();
        store.save_job_execution(&exec3).unwrap();

        // List should be sorted by most recent first
        let executions = store.list_job_executions("my-job", 100).unwrap();
        assert_eq!(executions.len(), 3);
        assert_eq!(executions[0].id, "newest");
        assert_eq!(executions[1].id, "middle");
        assert_eq!(executions[2].id, "oldest");
    }

    #[test]
    fn test_list_job_executions_filters_by_job_name() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save executions for different jobs
        store
            .save_job_execution(&create_test_execution("job-a", "a-1", true))
            .unwrap();
        store
            .save_job_execution(&create_test_execution("job-a", "a-2", true))
            .unwrap();
        store
            .save_job_execution(&create_test_execution("job-b", "b-1", true))
            .unwrap();
        store
            .save_job_execution(&create_test_execution("job-c", "c-1", true))
            .unwrap();

        // List for job-a only
        let executions_a = store.list_job_executions("job-a", 100).unwrap();
        assert_eq!(executions_a.len(), 2);
        assert!(executions_a.iter().all(|e| e.job_name == "job-a"));

        // List for job-b only
        let executions_b = store.list_job_executions("job-b", 100).unwrap();
        assert_eq!(executions_b.len(), 1);
        assert_eq!(executions_b[0].job_name, "job-b");

        // List for job-c only
        let executions_c = store.list_job_executions("job-c", 100).unwrap();
        assert_eq!(executions_c.len(), 1);
        assert_eq!(executions_c[0].job_name, "job-c");
    }

    #[test]
    fn test_cleanup_job_executions() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save 5 executions with staggered timestamps
        for i in 1..=5 {
            let mut exec = create_test_execution("my-job", &format!("exec-{i}"), true);
            exec.started_at = Utc::now() + chrono::Duration::seconds(i64::from(i));
            store.save_job_execution(&exec).unwrap();
        }

        // Verify 5 exist
        assert_eq!(store.list_job_executions("my-job", 100).unwrap().len(), 5);

        // Cleanup, keeping only 2
        let removed = store.cleanup_job_executions("my-job", 2).unwrap();
        assert_eq!(removed, 3);

        // Verify only 2 remain (the most recent ones)
        let remaining = store.list_job_executions("my-job", 100).unwrap();
        assert_eq!(remaining.len(), 2);

        // The remaining should be exec-5 and exec-4 (most recent)
        assert_eq!(remaining[0].id, "exec-5");
        assert_eq!(remaining[1].id, "exec-4");
    }

    #[test]
    fn test_cleanup_job_executions_nothing_to_remove() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save 2 executions
        store
            .save_job_execution(&create_test_execution("my-job", "exec-1", true))
            .unwrap();
        store
            .save_job_execution(&create_test_execution("my-job", "exec-2", true))
            .unwrap();

        // Cleanup with keep=5 should remove nothing
        let removed = store.cleanup_job_executions("my-job", 5).unwrap();
        assert_eq!(removed, 0);

        // All 2 should still exist
        assert_eq!(store.list_job_executions("my-job", 100).unwrap().len(), 2);
    }

    #[test]
    fn test_job_execution_persistence_across_reopens() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");

        // Save an execution
        {
            let store = StateStore::open(&db_path).unwrap();
            let exec = create_test_execution("persistent-job", "exec-1", true);
            store.save_job_execution(&exec).unwrap();
        }

        // Reopen and verify it persists
        {
            let store = StateStore::open(&db_path).unwrap();
            let executions = store.list_job_executions("persistent-job", 100).unwrap();
            assert_eq!(executions.len(), 1);
            assert_eq!(executions[0].id, "exec-1");
            assert_eq!(executions[0].job_name, "persistent-job");
            assert!(executions[0].success);
        }
    }

    #[test]
    fn test_job_execution_stores_error_message() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let mut exec = create_test_execution("failing-job", "exec-1", false);
        exec.error = Some("Connection refused".to_string());
        store.save_job_execution(&exec).unwrap();

        let executions = store.list_job_executions("failing-job", 100).unwrap();
        assert_eq!(executions.len(), 1);
        assert!(!executions[0].success);
        assert_eq!(executions[0].error, Some("Connection refused".to_string()));
    }

    #[test]
    fn test_job_execution_stores_manual_flag() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        let mut exec = create_test_execution("my-job", "manual-exec", true);
        exec.manual = true;
        store.save_job_execution(&exec).unwrap();

        let executions = store.list_job_executions("my-job", 100).unwrap();
        assert_eq!(executions.len(), 1);
        assert!(executions[0].manual);
    }
}
