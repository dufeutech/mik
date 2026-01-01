//! SQLite-backed SQL storage backend.
//!
//! Provides persistent SQL storage using rusqlite with ACID guarantees.

use super::backend::SqlBackend;
use super::types::{Row, Value};
use anyhow::{Context, Result};
use async_trait::async_trait;
use rusqlite::{Connection, params_from_iter};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// SQLite-backed SQL storage backend.
///
/// Provides persistent storage with ACID guarantees. Suitable for
/// production use where durability is required.
///
/// # Thread Safety
///
/// `SqliteBackend` is `Clone` and can be shared across threads. The underlying
/// connection is protected by a Mutex.
#[derive(Clone)]
pub struct SqliteBackend {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBackend {
    /// Opens or creates a SQLite database at the given path.
    ///
    /// Creates parent directories if needed. Database file is created lazily
    /// on first access. Uses SQLite's default journaling mode for ACID guarantees.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Parent directory cannot be created
    /// - Database file cannot be opened (permissions, corruption, etc.)
    /// - Foreign key pragma cannot be enabled
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists before opening database
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create database directory: {}", parent.display())
            })?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open SQLite database: {}", path.display()))?;

        // Enable foreign keys by default for referential integrity
        conn.execute("PRAGMA foreign_keys = ON", [])
            .context("Failed to enable foreign keys")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Internal helper for synchronous query execution.
    fn query_sync(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        let mut stmt = conn
            .prepare(sql)
            .with_context(|| format!("Failed to prepare query: {sql}"))?;

        let rusqlite_params: Vec<rusqlite::types::Value> =
            params.iter().map(Value::to_rusqlite).collect();

        let column_count = stmt.column_count();
        let column_names: Vec<String> = (0..column_count)
            .map(|i| stmt.column_name(i).unwrap_or("unknown").to_string())
            .collect();

        let rows = stmt
            .query_map(params_from_iter(rusqlite_params.iter()), |row| {
                let mut values = Vec::with_capacity(column_count);
                for i in 0..column_count {
                    let value_ref = row.get_ref(i)?;
                    values.push(Value::from(value_ref));
                }
                Ok(Row::new(column_names.clone(), values))
            })
            .with_context(|| format!("Failed to execute query: {sql}"))?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to fetch query results")?;

        Ok(rows)
    }

    /// Internal helper for synchronous execute.
    fn execute_sync(&self, sql: &str, params: &[Value]) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        let rusqlite_params: Vec<rusqlite::types::Value> =
            params.iter().map(Value::to_rusqlite).collect();

        let affected = conn
            .execute(sql, params_from_iter(rusqlite_params.iter()))
            .with_context(|| format!("Failed to execute statement: {sql}"))?;

        Ok(affected)
    }

    /// Internal helper for synchronous batch execution.
    fn execute_batch_sync(&self, sql: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        conn.execute_batch(sql)
            .with_context(|| format!("Failed to execute batch: {sql}"))?;

        Ok(())
    }

    /// Internal helper for synchronous atomic batch execution.
    fn execute_batch_atomic_sync(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<usize>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        // Begin transaction
        conn.execute("BEGIN TRANSACTION", [])
            .context("Failed to begin transaction")?;

        let mut results = Vec::with_capacity(statements.len());

        for (sql, params) in statements {
            let rusqlite_params: Vec<rusqlite::types::Value> =
                params.iter().map(Value::to_rusqlite).collect();

            match conn.execute(sql, params_from_iter(rusqlite_params.iter())) {
                Ok(affected) => results.push(affected),
                Err(e) => {
                    // Rollback on error
                    let _ = conn.execute("ROLLBACK", []);
                    return Err(anyhow::anyhow!("Failed to execute statement: {sql}").context(e));
                },
            }
        }

        // Commit transaction
        conn.execute("COMMIT", [])
            .context("Failed to commit transaction")?;

        Ok(results)
    }
}

#[async_trait]
impl SqlBackend for SqliteBackend {
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let backend = self.clone();
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || backend.query_sync(&sql, &params))
            .await
            .context("Task join error")?
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        let backend = self.clone();
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || backend.execute_sync(&sql, &params))
            .await
            .context("Task join error")?
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        let backend = self.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || backend.execute_batch_sync(&sql))
            .await
            .context("Task join error")?
    }

    async fn execute_batch_atomic(
        &self,
        statements: Vec<(String, Vec<Value>)>,
    ) -> Result<Vec<usize>> {
        let backend = self.clone();
        tokio::task::spawn_blocking(move || backend.execute_batch_atomic_sync(&statements))
            .await
            .context("Task join error")?
    }
}
