//! In-memory SQL storage backend.
//!
//! Provides a fast, non-persistent SQL store using SQLite's in-memory mode.
//! Ideal for testing, development, and embedded use cases.

use super::backend::SqlBackend;
use super::types::{Row, Value};
use anyhow::{Context, Result};
use async_trait::async_trait;
use rusqlite::{Connection, params_from_iter};
use std::sync::{Arc, Mutex};

/// In-memory SQL storage backend using SQLite's `:memory:` mode.
///
/// Provides fast, concurrent access without persistence. All data is lost
/// when the process exits. Ideal for:
/// - Testing and development
/// - Embedded applications (Tauri, etc.)
/// - Temporary data processing
///
/// # Thread Safety
///
/// `MemorySqlBackend` is `Clone` and uses a Mutex-protected connection.
///
/// # Example
///
/// ```ignore
/// use mik::daemon::services::sql::MemorySqlBackend;
///
/// let backend = MemorySqlBackend::new()?;
/// backend.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").await?;
/// ```
#[derive(Clone)]
pub struct MemorySqlBackend {
    conn: Arc<Mutex<Connection>>,
}

impl MemorySqlBackend {
    /// Creates a new in-memory SQLite database.
    ///
    /// # Errors
    ///
    /// Returns an error if the in-memory database cannot be created.
    pub fn new() -> Result<Self> {
        let conn =
            Connection::open(":memory:").context("Failed to create in-memory SQLite database")?;

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
impl SqlBackend for MemorySqlBackend {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_table_and_insert() {
        let backend = MemorySqlBackend::new().unwrap();

        backend
            .execute_batch(
                "CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    age INTEGER
                )",
            )
            .await
            .unwrap();

        let affected = backend
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Alice".to_string()), Value::Integer(30)],
            )
            .await
            .unwrap();

        assert_eq!(affected, 1);
    }

    #[tokio::test]
    async fn test_query_rows() {
        let backend = MemorySqlBackend::new().unwrap();

        backend
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
            .await
            .unwrap();

        backend
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Alice".to_string()), Value::Integer(30)],
            )
            .await
            .unwrap();

        backend
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Bob".to_string()), Value::Integer(25)],
            )
            .await
            .unwrap();

        let rows = backend
            .query("SELECT * FROM users ORDER BY name", &[])
            .await
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].columns, vec!["id", "name", "age"]);
        assert_eq!(rows[0].values[1], Value::Text("Alice".to_string()));
        assert_eq!(rows[1].values[1], Value::Text("Bob".to_string()));
    }

    #[tokio::test]
    async fn test_batch_atomic_rollback() {
        let backend = MemorySqlBackend::new().unwrap();

        backend
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .await
            .unwrap();

        // Try to insert with a failure in the middle
        let statements = vec![
            (
                "INSERT INTO users (name) VALUES (?)".to_string(),
                vec![Value::Text("Alice".to_string())],
            ),
            (
                "INSERT INTO users (name) VALUES (?)".to_string(),
                vec![Value::Null], // This should fail due to NOT NULL
            ),
        ];

        let result = backend.execute_batch_atomic(statements).await;
        assert!(result.is_err());

        // Verify rollback happened
        let rows = backend.query("SELECT * FROM users", &[]).await.unwrap();
        assert_eq!(rows.len(), 0);
    }
}
