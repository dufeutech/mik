//! Persistent state management for the mik daemon using redb.
//!
//! Tracks WASM instances across daemon restarts, storing instance metadata,
//! status, and configuration in a single-file ACID database at `~/.mik/state.redb`.
//!
//! # Async Usage
//!
//! All database operations are blocking. When using from async contexts,
//! use the async methods (`save_instance_async`, `get_instance_async`, etc.)
//! which automatically wrap operations in `spawn_blocking` to avoid blocking
//! the async runtime.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::daemon::cron::{JobExecution, ScheduleConfig};

/// Table name for instance storage - centralized to avoid duplication
const INSTANCES_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("instances");

/// Table name for cron job storage
const CRON_JOBS_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("cron_jobs");

/// Table name for cron job execution history
/// Key format: `job_name:execution_id`
const CRON_HISTORY_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("cron_history");

/// Table name for sidecar service registry
const SIDECARS_TABLE: TableDefinition<'static, &'static str, &'static [u8]> =
    TableDefinition::new("sidecars");

/// Type of sidecar service
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceType {
    /// Key-value store (Redis-like)
    Kv,
    /// SQL database (SQLite/Postgres)
    Sql,
    /// Object storage (S3-like)
    Storage,
    /// Message queue
    Queue,
    /// Custom service type
    Custom(String),
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::Kv => write!(f, "kv"),
            ServiceType::Sql => write!(f, "sql"),
            ServiceType::Storage => write!(f, "storage"),
            ServiceType::Queue => write!(f, "queue"),
            ServiceType::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// Metadata for a registered sidecar service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    /// Unique service name (e.g., "mikcar-primary", "redis-cache")
    pub name: String,
    /// Type of service provided
    pub service_type: ServiceType,
    /// Base URL for the service (e.g., `http://localhost:9001`)
    pub url: String,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
    /// Timestamp when service was registered
    pub registered_at: DateTime<Utc>,
    /// Last heartbeat timestamp (updated by health checks)
    pub last_heartbeat: DateTime<Utc>,
    /// Whether the service is currently healthy
    #[serde(default = "default_healthy")]
    pub healthy: bool,
}

fn default_healthy() -> bool {
    true
}

/// Runtime status of a WASM instance
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    /// Instance is currently running
    Running,
    /// Instance was stopped gracefully
    Stopped,
    /// Instance crashed with exit code
    Crashed { exit_code: i32 },
}

/// Metadata for a WASM instance managed by the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Unique instance name (e.g., "default", "staging")
    pub name: String,
    /// HTTP port the instance listens on
    pub port: u16,
    /// Operating system process ID
    pub pid: u32,
    /// Current runtime status
    pub status: Status,
    /// Path to the mik.toml configuration file used
    pub config: PathBuf,
    /// Timestamp when instance was started
    pub started_at: DateTime<Utc>,
    /// List of loaded WASM modules (relative paths from config)
    pub modules: Vec<String>,
    /// Whether auto-restart on crash is enabled (default: false)
    #[serde(default)]
    pub auto_restart: bool,
    /// Number of times this instance has been auto-restarted
    #[serde(default)]
    pub restart_count: u32,
    /// Timestamp of last auto-restart (for backoff calculation)
    #[serde(default)]
    pub last_restart_at: Option<DateTime<Utc>>,
}

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
    db: Arc<Database>,
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

    /// Persists an instance to the database.
    ///
    /// Overwrites existing instance with same name. Serializes to JSON
    /// for compatibility with debugging tools and future schema evolution.
    pub fn save_instance(&self, instance: &Instance) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(INSTANCES_TABLE)
                .context("Failed to open instances table")?;

            let json =
                serde_json::to_vec(instance).context("Failed to serialize instance to JSON")?;

            table
                .insert(instance.name.as_str(), json.as_slice())
                .with_context(|| format!("Failed to insert instance '{}'", instance.name))?;
        }

        write_txn
            .commit()
            .context("Failed to commit instance save transaction")?;

        Ok(())
    }

    /// Retrieves an instance by name.
    ///
    /// Returns None if instance doesn't exist. Deserializes from JSON
    /// and validates structure matches current Instance schema.
    pub fn get_instance(&self, name: &str) -> Result<Option<Instance>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(INSTANCES_TABLE)
            .context("Failed to open instances table")?;

        let result = table
            .get(name)
            .with_context(|| format!("Failed to read instance '{name}'"))?;

        match result {
            Some(guard) => {
                let json = guard.value();
                let instance = serde_json::from_slice(json)
                    .with_context(|| format!("Failed to deserialize instance '{name}'"))?;
                Ok(Some(instance))
            },
            None => Ok(None),
        }
    }

    /// Lists all instances in the database.
    ///
    /// Returns empty vec if no instances exist. Skips instances that fail
    /// deserialization to prevent corruption from blocking reads.
    pub fn list_instances(&self) -> Result<Vec<Instance>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(INSTANCES_TABLE)
            .context("Failed to open instances table")?;

        let mut instances = Vec::new();

        for item in table.iter().context("Failed to iterate instances table")? {
            let (_, value) = item.context("Failed to read instance entry")?;

            // Skip corrupted entries instead of failing the entire list operation
            if let Ok(instance) = serde_json::from_slice::<Instance>(value.value()) {
                instances.push(instance);
            }
        }

        Ok(instances)
    }

    /// Removes an instance from the database.
    ///
    /// Returns Ok(true) if instance existed and was removed, Ok(false) if
    /// it didn't exist. Idempotent - safe to call multiple times.
    pub fn remove_instance(&self, name: &str) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        let removed = {
            let mut table = write_txn
                .open_table(INSTANCES_TABLE)
                .context("Failed to open instances table")?;

            table
                .remove(name)
                .with_context(|| format!("Failed to remove instance '{name}'"))?
                .is_some()
        };

        write_txn
            .commit()
            .context("Failed to commit instance removal transaction")?;

        Ok(removed)
    }

    // ========================================================================
    // Async Methods
    //
    // These methods wrap the synchronous operations in `spawn_blocking` to
    // avoid blocking the async runtime. Use these when calling from async
    // contexts (HTTP handlers, etc.).
    //
    // Note: These are intentionally provided for API completeness and for
    // use in async HTTP handlers. Currently the daemon uses synchronous
    // methods but these are available for future use.
    // ========================================================================

    /// Persists an instance to the database asynchronously.
    ///
    /// Async version of `save_instance` that uses `spawn_blocking`.
    #[allow(dead_code)]
    pub async fn save_instance_async(&self, instance: Instance) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.save_instance(&instance))
            .await
            .context("Task join error")?
    }

    /// Retrieves an instance by name asynchronously.
    ///
    /// Async version of `get_instance` that uses `spawn_blocking`.
    #[allow(dead_code)]
    pub async fn get_instance_async(&self, name: String) -> Result<Option<Instance>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.get_instance(&name))
            .await
            .context("Task join error")?
    }

    /// Lists all instances asynchronously.
    ///
    /// Async version of `list_instances` that uses `spawn_blocking`.
    #[allow(dead_code)]
    pub async fn list_instances_async(&self) -> Result<Vec<Instance>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list_instances())
            .await
            .context("Task join error")?
    }

    /// Removes an instance asynchronously.
    ///
    /// Async version of `remove_instance` that uses `spawn_blocking`.
    #[allow(dead_code)]
    pub async fn remove_instance_async(&self, name: String) -> Result<bool> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.remove_instance(&name))
            .await
            .context("Task join error")?
    }

    // ========================================================================
    // Cron Job Methods
    // ========================================================================

    /// Persists a cron job to the database.
    ///
    /// Overwrites existing job with same name.
    pub fn save_cron_job(&self, config: &ScheduleConfig) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(CRON_JOBS_TABLE)
                .context("Failed to open cron_jobs table")?;

            let json =
                serde_json::to_vec(config).context("Failed to serialize cron job to JSON")?;

            table
                .insert(config.name.as_str(), json.as_slice())
                .with_context(|| format!("Failed to insert cron job '{}'", config.name))?;
        }

        write_txn
            .commit()
            .context("Failed to commit cron job save transaction")?;

        Ok(())
    }

    /// Retrieves a cron job by name.
    #[allow(dead_code)]
    pub fn get_cron_job(&self, name: &str) -> Result<Option<ScheduleConfig>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(CRON_JOBS_TABLE)
            .context("Failed to open cron_jobs table")?;

        let result = table
            .get(name)
            .with_context(|| format!("Failed to read cron job '{name}'"))?;

        match result {
            Some(guard) => {
                let json = guard.value();
                let config = serde_json::from_slice(json)
                    .with_context(|| format!("Failed to deserialize cron job '{name}'"))?;
                Ok(Some(config))
            },
            None => Ok(None),
        }
    }

    /// Lists all cron jobs in the database.
    pub fn list_cron_jobs(&self) -> Result<Vec<ScheduleConfig>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(CRON_JOBS_TABLE)
            .context("Failed to open cron_jobs table")?;

        let mut jobs = Vec::new();

        for item in table.iter().context("Failed to iterate cron_jobs table")? {
            let (_, value) = item.context("Failed to read cron job entry")?;

            if let Ok(config) = serde_json::from_slice::<ScheduleConfig>(value.value()) {
                jobs.push(config);
            }
        }

        Ok(jobs)
    }

    /// Removes a cron job from the database.
    pub fn remove_cron_job(&self, name: &str) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        let removed = {
            let mut table = write_txn
                .open_table(CRON_JOBS_TABLE)
                .context("Failed to open cron_jobs table")?;

            table
                .remove(name)
                .with_context(|| format!("Failed to remove cron job '{name}'"))?
                .is_some()
        };

        write_txn
            .commit()
            .context("Failed to commit cron job remove transaction")?;

        Ok(removed)
    }

    /// Persists a cron job asynchronously.
    pub async fn save_cron_job_async(&self, config: ScheduleConfig) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.save_cron_job(&config))
            .await
            .context("Task join error")?
    }

    /// Lists all cron jobs asynchronously.
    pub async fn list_cron_jobs_async(&self) -> Result<Vec<ScheduleConfig>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list_cron_jobs())
            .await
            .context("Task join error")?
    }

    /// Removes a cron job asynchronously.
    pub async fn remove_cron_job_async(&self, name: String) -> Result<bool> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.remove_cron_job(&name))
            .await
            .context("Task join error")?
    }

    // =========================================================================
    // Cron Job Execution History
    // =========================================================================

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
    #[allow(dead_code)]
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

    // ========================================================================
    // Sidecar Service Discovery Methods
    // ========================================================================

    /// Registers a sidecar service.
    ///
    /// Overwrites existing sidecar with same name.
    pub fn save_sidecar(&self, sidecar: &Sidecar) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        {
            let mut table = write_txn
                .open_table(SIDECARS_TABLE)
                .context("Failed to open sidecars table")?;

            let json =
                serde_json::to_vec(sidecar).context("Failed to serialize sidecar to JSON")?;

            table
                .insert(sidecar.name.as_str(), json.as_slice())
                .with_context(|| format!("Failed to insert sidecar '{}'", sidecar.name))?;
        }

        write_txn
            .commit()
            .context("Failed to commit sidecar save transaction")?;

        Ok(())
    }

    /// Retrieves a sidecar by name.
    pub fn get_sidecar(&self, name: &str) -> Result<Option<Sidecar>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(SIDECARS_TABLE)
            .context("Failed to open sidecars table")?;

        let result = table
            .get(name)
            .with_context(|| format!("Failed to read sidecar '{name}'"))?;

        match result {
            Some(guard) => {
                let json = guard.value();
                let sidecar = serde_json::from_slice(json)
                    .with_context(|| format!("Failed to deserialize sidecar '{name}'"))?;
                Ok(Some(sidecar))
            },
            None => Ok(None),
        }
    }

    /// Lists all registered sidecars.
    pub fn list_sidecars(&self) -> Result<Vec<Sidecar>> {
        let read_txn = self
            .db
            .begin_read()
            .context("Failed to begin read transaction")?;

        let table = read_txn
            .open_table(SIDECARS_TABLE)
            .context("Failed to open sidecars table")?;

        let mut sidecars = Vec::new();

        for item in table.iter().context("Failed to iterate sidecars table")? {
            let (_, value) = item.context("Failed to read sidecar entry")?;

            if let Ok(sidecar) = serde_json::from_slice::<Sidecar>(value.value()) {
                sidecars.push(sidecar);
            }
        }

        Ok(sidecars)
    }

    /// Lists sidecars by service type.
    pub fn list_sidecars_by_type(&self, service_type: &ServiceType) -> Result<Vec<Sidecar>> {
        let all = self.list_sidecars()?;
        Ok(all
            .into_iter()
            .filter(|s| &s.service_type == service_type)
            .collect())
    }

    /// Removes a sidecar from the registry.
    pub fn remove_sidecar(&self, name: &str) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .context("Failed to begin write transaction")?;

        let removed = {
            let mut table = write_txn
                .open_table(SIDECARS_TABLE)
                .context("Failed to open sidecars table")?;

            table
                .remove(name)
                .with_context(|| format!("Failed to remove sidecar '{name}'"))?
                .is_some()
        };

        write_txn
            .commit()
            .context("Failed to commit sidecar removal transaction")?;

        Ok(removed)
    }

    /// Updates the heartbeat timestamp for a sidecar.
    pub fn update_sidecar_heartbeat(&self, name: &str, healthy: bool) -> Result<bool> {
        if let Some(mut sidecar) = self.get_sidecar(name)? {
            sidecar.last_heartbeat = Utc::now();
            sidecar.healthy = healthy;
            self.save_sidecar(&sidecar)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // ========================================================================
    // Async Sidecar Methods
    // ========================================================================

    /// Registers a sidecar service asynchronously.
    pub async fn save_sidecar_async(&self, sidecar: Sidecar) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.save_sidecar(&sidecar))
            .await
            .context("Task join error")?
    }

    /// Retrieves a sidecar by name asynchronously.
    pub async fn get_sidecar_async(&self, name: String) -> Result<Option<Sidecar>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.get_sidecar(&name))
            .await
            .context("Task join error")?
    }

    /// Lists all registered sidecars asynchronously.
    pub async fn list_sidecars_async(&self) -> Result<Vec<Sidecar>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list_sidecars())
            .await
            .context("Task join error")?
    }

    /// Lists sidecars by service type asynchronously.
    pub async fn list_sidecars_by_type_async(
        &self,
        service_type: ServiceType,
    ) -> Result<Vec<Sidecar>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list_sidecars_by_type(&service_type))
            .await
            .context("Task join error")?
    }

    /// Removes a sidecar asynchronously.
    pub async fn remove_sidecar_async(&self, name: String) -> Result<bool> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.remove_sidecar(&name))
            .await
            .context("Task join error")?
    }

    /// Updates the heartbeat timestamp for a sidecar asynchronously.
    pub async fn update_sidecar_heartbeat_async(
        &self,
        name: String,
        healthy: bool,
    ) -> Result<bool> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.update_sidecar_heartbeat(&name, healthy))
            .await
            .context("Task join error")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
