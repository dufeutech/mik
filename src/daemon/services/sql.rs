//! Embedded SQL service using `SQLite` via rusqlite.
//!
//! Provides a simple SQL interface backed by `SQLite` for WASM instances to persist
//! structured data. Database file is stored at `~/.mik/data.db` and created lazily
//! on first use.
//!
//! # Example
//!
//! ```no_run
//! use mik::daemon::services::sql::{SqlService, Value};
//!
//! let service = SqlService::open("~/.mik/data.db")?;
//!
//! // Create table
//! service.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
//!
//! // Insert data
//! service.execute(
//!     "INSERT INTO users (name) VALUES (?)",
//!     &[Value::Text("Alice".to_string())]
//! )?;
//!
//! // Query data
//! let rows = service.query("SELECT * FROM users WHERE id = ?", &[Value::Integer(1)])?;
//! ```
//!
//! # Async Usage
//!
//! All database operations are blocking. When using from async contexts,
//! use the async methods (`query_async`, `execute_async`, etc.) which automatically
//! wrap operations in `spawn_blocking` to avoid blocking the async runtime.

// Allow unused - SQL service for future sidecar integration
#![allow(dead_code)]

use anyhow::{Context, Result};
use rusqlite::{Connection, params_from_iter, types::ValueRef};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// SQL value types that can be stored in `SQLite`.
///
/// Mirrors `SQLite`'s type system for seamless conversion. All types
/// are JSON-serializable for API responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum Value {
    /// SQL NULL value
    Null,
    /// 64-bit signed integer
    Integer(i64),
    /// 64-bit floating point number
    Real(f64),
    /// UTF-8 text string
    Text(String),
    /// Binary blob data
    Blob(Vec<u8>),
}

impl From<ValueRef<'_>> for Value {
    fn from(value_ref: ValueRef<'_>) -> Self {
        match value_ref {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(i) => Self::Integer(i),
            ValueRef::Real(r) => Self::Real(r),
            ValueRef::Text(t) => Self::Text(String::from_utf8_lossy(t).to_string()),
            ValueRef::Blob(b) => Self::Blob(b.to_vec()),
        }
    }
}

/// A single row returned from a SQL query.
///
/// Contains column names and their corresponding values in order.
/// JSON-serializable for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    /// Column names in order
    pub columns: Vec<String>,
    /// Values in same order as columns
    pub values: Vec<Value>,
}

impl Row {
    /// Creates a new row with the given columns and values.
    ///
    /// # Panics
    ///
    /// Panics if `columns.len()` != `values.len()`. This validation runs in
    /// both debug and release builds to prevent data corruption.
    pub fn new(columns: Vec<String>, values: Vec<Value>) -> Self {
        assert_eq!(
            columns.len(),
            values.len(),
            "Column count ({}) must match value count ({})",
            columns.len(),
            values.len()
        );
        Self { columns, values }
    }

    /// Creates a new row, returning an error if column/value counts don't match.
    ///
    /// This is the fallible alternative to `new()` for cases where panicking
    /// is not acceptable.
    ///
    /// # Errors
    ///
    /// Returns an error if `columns.len()` does not equal `values.len()`.
    pub fn try_new(columns: Vec<String>, values: Vec<Value>) -> Result<Self> {
        if columns.len() != values.len() {
            anyhow::bail!(
                "Column count ({}) does not match value count ({})",
                columns.len(),
                values.len()
            );
        }
        Ok(Self { columns, values })
    }

    /// Gets the number of columns in this row.
    pub const fn len(&self) -> usize {
        self.columns.len()
    }

    /// Returns true if the row has no columns.
    pub const fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Gets a value by column name, returning None if not found.
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .position(|c| c == column)
            .and_then(|idx| self.values.get(idx))
    }
}

/// Embedded SQL service backed by `SQLite`.
///
/// Wraps a rusqlite Connection with thread-safe access via Arc<Mutex>.
/// Database is created lazily on first open and persists across service restarts.
///
/// # Thread Safety
///
/// `SqlService` is `Clone` and can be shared across threads. The underlying
/// connection is protected by a Mutex.
#[derive(Clone)]
pub struct SqlService {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl SqlService {
    /// Opens or creates a `SQLite` database at the given path.
    ///
    /// Creates parent directories if needed. Database file is created lazily
    /// on first access. Uses `SQLite`'s default journaling mode for ACID guarantees.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Parent directory cannot be created
    /// - Database file cannot be opened (permissions, corruption, etc.)
    /// - Foreign key pragma cannot be enabled
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::daemon::services::sql::SqlService;
    ///
    /// let service = SqlService::open("~/.mik/data.db")?;
    /// ```
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
            path: path.to_path_buf(),
        })
    }

    /// Returns the path to the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Executes a SELECT query and returns matching rows.
    ///
    /// Accepts parameterized queries to prevent SQL injection. Parameters
    /// are bound in order using ? placeholders.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Database lock cannot be acquired (poisoned mutex)
    /// - Query preparation fails (SQL syntax error)
    /// - Query execution or result fetching fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::daemon::services::sql::{SqlService, Value};
    ///
    /// let service = SqlService::open("~/.mik/data.db")?;
    /// let rows = service.query(
    ///     "SELECT * FROM users WHERE age > ?",
    ///     &[Value::Integer(18)]
    /// )?;
    /// ```
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        let mut stmt = conn
            .prepare(sql)
            .with_context(|| format!("Failed to prepare query: {sql}"))?;

        // Convert Value params to rusqlite::types::Value
        let rusqlite_params: Vec<rusqlite::types::Value> = params
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Integer(i) => rusqlite::types::Value::Integer(*i),
                Value::Real(r) => rusqlite::types::Value::Real(*r),
                Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
            })
            .collect();

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

    /// Executes an INSERT, UPDATE, or DELETE statement.
    ///
    /// Returns the number of rows affected. Accepts parameterized queries
    /// to prevent SQL injection.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Database lock cannot be acquired (poisoned mutex)
    /// - Statement execution fails (constraint violation, SQL error)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::daemon::services::sql::{SqlService, Value};
    ///
    /// let service = SqlService::open("~/.mik/data.db")?;
    /// let affected = service.execute(
    ///     "INSERT INTO users (name, age) VALUES (?, ?)",
    ///     &[Value::Text("Bob".to_string()), Value::Integer(25)]
    /// )?;
    /// assert_eq!(affected, 1);
    /// ```
    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        // Convert Value params to rusqlite::types::Value
        let rusqlite_params: Vec<rusqlite::types::Value> = params
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Integer(i) => rusqlite::types::Value::Integer(*i),
                Value::Real(r) => rusqlite::types::Value::Real(*r),
                Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
            })
            .collect();

        let affected = conn
            .execute(sql, params_from_iter(rusqlite_params.iter()))
            .with_context(|| format!("Failed to execute statement: {sql}"))?;

        Ok(affected)
    }

    /// Executes multiple SQL statements in a batch.
    ///
    /// Useful for creating tables or running migration scripts. Statements
    /// are separated by semicolons. Does not support parameterization -
    /// use only with trusted SQL (e.g., schema definitions).
    ///
    /// # Errors
    ///
    /// Returns an error if the database lock cannot be acquired or any
    /// statement in the batch fails to execute.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::daemon::services::sql::SqlService;
    ///
    /// let service = SqlService::open("~/.mik/data.db")?;
    /// service.execute_batch(r#"
    ///     CREATE TABLE users (
    ///         id INTEGER PRIMARY KEY,
    ///         name TEXT NOT NULL
    ///     );
    ///     CREATE INDEX idx_users_name ON users(name);
    /// "#)?;
    /// ```
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        conn.execute_batch(sql)
            .with_context(|| format!("Failed to execute batch: {sql}"))?;

        Ok(())
    }

    /// Begins a transaction for multiple operations.
    ///
    /// Returns a transaction handle that can be used to execute multiple
    /// operations atomically. The transaction holds the database lock for
    /// its entire duration to ensure isolation. Call `commit()` to persist
    /// changes or drop the transaction to rollback.
    ///
    /// # Errors
    ///
    /// Returns an error if the database lock cannot be acquired or the
    /// BEGIN TRANSACTION statement fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mik::daemon::services::sql::{SqlService, Value};
    ///
    /// let service = SqlService::open("~/.mik/data.db")?;
    /// let tx = service.transaction()?;
    /// tx.execute("INSERT INTO users (name) VALUES (?)", &[Value::Text("Alice".into())])?;
    /// tx.execute("INSERT INTO users (name) VALUES (?)", &[Value::Text("Bob".into())])?;
    /// tx.commit()?;
    /// ```
    ///
    /// # Note
    ///
    /// While a transaction is active, other threads cannot access the database.
    /// Keep transactions short to avoid blocking.
    pub fn transaction(&self) -> Result<Transaction<'_>> {
        let guard = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire database lock: {e}"))?;

        // Begin the transaction immediately
        guard
            .execute("BEGIN TRANSACTION", [])
            .context("Failed to begin transaction")?;

        Ok(Transaction {
            guard,
            committed: false,
        })
    }

    // ========================================================================
    // Async Methods
    //
    // These methods wrap the synchronous operations in `spawn_blocking` to
    // avoid blocking the async runtime. Use these when calling from async
    // contexts (HTTP handlers, etc.).
    // ========================================================================

    /// Executes a SELECT query asynchronously.
    ///
    /// Async version of `query` that uses `spawn_blocking`.
    pub async fn query_async(&self, sql: String, params: Vec<Value>) -> Result<Vec<Row>> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.query(&sql, &params))
            .await
            .context("Task join error")?
    }

    /// Executes an INSERT/UPDATE/DELETE statement asynchronously.
    ///
    /// Async version of `execute` that uses `spawn_blocking`.
    pub async fn execute_async(&self, sql: String, params: Vec<Value>) -> Result<usize> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.execute(&sql, &params))
            .await
            .context("Task join error")?
    }

    /// Executes a batch of SQL statements asynchronously.
    ///
    /// Async version of `execute_batch` that uses `spawn_blocking`.
    pub async fn execute_batch_async(&self, sql: String) -> Result<()> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.execute_batch(&sql))
            .await
            .context("Task join error")?
    }
}

/// A database transaction for atomic operations.
///
/// Holds the database lock for its entire lifetime to ensure isolation.
/// Automatically rolls back on drop unless `commit()` is called.
pub struct Transaction<'a> {
    /// Holds the connection lock for the transaction duration
    guard: std::sync::MutexGuard<'a, Connection>,
    committed: bool,
}

impl Transaction<'_> {
    /// Executes a statement within the transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement fails (constraint violation, SQL error).
    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        // Convert Value params to rusqlite::types::Value
        let rusqlite_params: Vec<rusqlite::types::Value> = params
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Integer(i) => rusqlite::types::Value::Integer(*i),
                Value::Real(r) => rusqlite::types::Value::Real(*r),
                Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
            })
            .collect();

        let affected = self
            .guard
            .execute(sql, params_from_iter(rusqlite_params.iter()))
            .with_context(|| format!("Failed to execute statement: {sql}"))?;

        Ok(affected)
    }

    /// Executes a query within the transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if query preparation or execution fails.
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let mut stmt = self
            .guard
            .prepare(sql)
            .with_context(|| format!("Failed to prepare query: {sql}"))?;

        // Convert Value params to rusqlite::types::Value
        let rusqlite_params: Vec<rusqlite::types::Value> = params
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Integer(i) => rusqlite::types::Value::Integer(*i),
                Value::Real(r) => rusqlite::types::Value::Real(*r),
                Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
                Value::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
            })
            .collect();

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

    /// Commits the transaction, making all changes permanent.
    ///
    /// # Errors
    ///
    /// Returns an error if the COMMIT statement fails.
    pub fn commit(mut self) -> Result<()> {
        self.guard
            .execute("COMMIT", [])
            .context("Failed to commit transaction")?;

        self.committed = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Rollback on drop if not committed
            let _ = self.guard.execute("ROLLBACK", []);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_service() -> (SqlService, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let service = SqlService::open(&db_path).unwrap();
        (service, tmp)
    }

    #[test]
    fn test_create_table_and_insert() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch(
                "CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    age INTEGER
                )",
            )
            .unwrap();

        let affected = service
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Alice".to_string()), Value::Integer(30)],
            )
            .unwrap();

        assert_eq!(affected, 1);
    }

    #[test]
    fn test_query_rows() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
            .unwrap();

        service
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Alice".to_string()), Value::Integer(30)],
            )
            .unwrap();

        service
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Bob".to_string()), Value::Integer(25)],
            )
            .unwrap();

        let rows = service
            .query("SELECT * FROM users ORDER BY name", &[])
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].columns, vec!["id", "name", "age"]);
        assert_eq!(rows[0].values[1], Value::Text("Alice".to_string()));
        assert_eq!(rows[1].values[1], Value::Text("Bob".to_string()));
    }

    #[test]
    fn test_query_with_params() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
            .unwrap();

        service
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Alice".to_string()), Value::Integer(30)],
            )
            .unwrap();

        service
            .execute(
                "INSERT INTO users (name, age) VALUES (?, ?)",
                &[Value::Text("Bob".to_string()), Value::Integer(25)],
            )
            .unwrap();

        let rows = service
            .query("SELECT * FROM users WHERE age > ?", &[Value::Integer(26)])
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[1], Value::Text("Alice".to_string()));
    }

    #[test]
    fn test_update_and_delete() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();

        service
            .execute(
                "INSERT INTO users (name) VALUES (?)",
                &[Value::Text("Alice".to_string())],
            )
            .unwrap();

        // Update
        let affected = service
            .execute(
                "UPDATE users SET name = ? WHERE name = ?",
                &[
                    Value::Text("Alicia".to_string()),
                    Value::Text("Alice".to_string()),
                ],
            )
            .unwrap();
        assert_eq!(affected, 1);

        // Delete
        let affected = service
            .execute(
                "DELETE FROM users WHERE name = ?",
                &[Value::Text("Alicia".to_string())],
            )
            .unwrap();
        assert_eq!(affected, 1);

        let rows = service.query("SELECT * FROM users", &[]).unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn test_all_value_types() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch(
                "CREATE TABLE types_test (
                    id INTEGER PRIMARY KEY,
                    null_val NULL,
                    int_val INTEGER,
                    real_val REAL,
                    text_val TEXT,
                    blob_val BLOB
                )",
            )
            .unwrap();

        service
            .execute(
                "INSERT INTO types_test (null_val, int_val, real_val, text_val, blob_val)
                 VALUES (?, ?, ?, ?, ?)",
                &[
                    Value::Null,
                    Value::Integer(42),
                    Value::Real(1.234),
                    Value::Text("hello".to_string()),
                    Value::Blob(vec![1, 2, 3, 4]),
                ],
            )
            .unwrap();

        let rows = service.query("SELECT * FROM types_test", &[]).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[1], Value::Null);
        assert_eq!(rows[0].values[2], Value::Integer(42));
        assert_eq!(rows[0].values[3], Value::Real(1.234));
        assert_eq!(rows[0].values[4], Value::Text("hello".to_string()));
        assert_eq!(rows[0].values[5], Value::Blob(vec![1, 2, 3, 4]));
    }

    #[test]
    fn test_row_get_by_column() {
        let row = Row::new(
            vec!["id".to_string(), "name".to_string()],
            vec![Value::Integer(1), Value::Text("Alice".to_string())],
        );

        assert_eq!(row.get("id"), Some(&Value::Integer(1)));
        assert_eq!(row.get("name"), Some(&Value::Text("Alice".to_string())));
        assert_eq!(row.get("nonexistent"), None);
    }

    #[test]
    fn test_execute_batch() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch(
                r"
                CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
                CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER, title TEXT);
                CREATE INDEX idx_posts_user ON posts(user_id);
            ",
            )
            .unwrap();

        // Verify tables exist by inserting data
        service
            .execute(
                "INSERT INTO users (name) VALUES (?)",
                &[Value::Text("Alice".to_string())],
            )
            .unwrap();

        service
            .execute(
                "INSERT INTO posts (user_id, title) VALUES (?, ?)",
                &[Value::Integer(1), Value::Text("Hello World".to_string())],
            )
            .unwrap();

        let users = service.query("SELECT * FROM users", &[]).unwrap();
        let posts = service.query("SELECT * FROM posts", &[]).unwrap();

        assert_eq!(users.len(), 1);
        assert_eq!(posts.len(), 1);
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let (service, _tmp) = create_test_service();

        service
            .execute_batch(
                r"
                CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
                CREATE TABLE posts (
                    id INTEGER PRIMARY KEY,
                    user_id INTEGER NOT NULL,
                    title TEXT,
                    FOREIGN KEY (user_id) REFERENCES users(id)
                );
            ",
            )
            .unwrap();

        // Should fail because foreign key constraint is violated
        let result = service.execute(
            "INSERT INTO posts (user_id, title) VALUES (?, ?)",
            &[Value::Integer(999), Value::Text("Invalid".to_string())],
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_persistence_across_reopens() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("persist.db");

        {
            let service = SqlService::open(&db_path).unwrap();
            service
                .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            service
                .execute(
                    "INSERT INTO users (name) VALUES (?)",
                    &[Value::Text("Alice".to_string())],
                )
                .unwrap();
        }

        // Reopen and verify data persists
        {
            let service = SqlService::open(&db_path).unwrap();
            let rows = service.query("SELECT * FROM users", &[]).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[1], Value::Text("Alice".to_string()));
        }
    }

    #[test]
    fn test_value_serialization() {
        // Test that Value types can be serialized to/from JSON
        let values = vec![
            Value::Null,
            Value::Integer(42),
            Value::Real(1.234),
            Value::Text("hello".to_string()),
            Value::Blob(vec![1, 2, 3]),
        ];

        for value in values {
            let json = serde_json::to_string(&value).unwrap();
            let deserialized: Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value, deserialized);
        }
    }

    #[test]
    fn test_row_serialization() {
        let row = Row::new(
            vec!["id".to_string(), "name".to_string()],
            vec![Value::Integer(1), Value::Text("Alice".to_string())],
        );

        let json = serde_json::to_string(&row).unwrap();
        let deserialized: Row = serde_json::from_str(&json).unwrap();

        assert_eq!(row.columns, deserialized.columns);
        assert_eq!(row.values, deserialized.values);
    }
}
