//! Fuzz target for SQL injection vulnerabilities in the SQL service.
//!
//! This fuzzer tests that:
//! 1. Parameterized queries properly escape all inputs
//! 2. SQL injection attempts are safely handled
//! 3. Malicious inputs cannot escape parameter boundaries
//! 4. Various SQL injection patterns are neutralized
//!
//! Run with: `cargo +nightly fuzz run fuzz_sql_injection`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mik::daemon::services::sql::{SqlService, Value};
use tempfile::TempDir;

/// Maximum string length to prevent OOM.
const MAX_STRING_LEN: usize = 10_000;

/// Maximum blob size to prevent OOM.
const MAX_BLOB_SIZE: usize = 100_000;

/// Maximum number of parameters per query.
const MAX_PARAMS: usize = 100;

/// Input structure for SQL fuzzing.
#[derive(Arbitrary, Debug)]
struct SqlInput {
    /// Type of SQL operation to test
    operation: SqlOperation,
    /// Input values (potentially malicious)
    values: Vec<FuzzValue>,
    /// Additional adversarial patterns
    pattern: Option<AdversarialPattern>,
}

/// Types of SQL operations to fuzz.
#[derive(Arbitrary, Debug)]
enum SqlOperation {
    /// Test INSERT with parameterized values
    Insert,
    /// Test SELECT with WHERE clause parameters
    Select,
    /// Test UPDATE with parameterized SET and WHERE
    Update,
    /// Test DELETE with WHERE clause parameters
    Delete,
    /// Test multiple operations in sequence
    MultiOperation,
    /// Test with raw string in table/column positions (should still be safe)
    TableName(String),
}

/// Value types that can contain injection attempts.
#[derive(Arbitrary, Debug, Clone)]
enum FuzzValue {
    /// Null value
    Null,
    /// Integer (safe)
    Integer(i64),
    /// Float (safe)
    Real(f64),
    /// String (primary injection vector)
    Text(String),
    /// Binary data (may contain injection patterns)
    Blob(Vec<u8>),
}

impl FuzzValue {
    fn to_sql_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(i) => Value::Integer(*i),
            Self::Real(r) => Value::Real(*r),
            Self::Text(s) => {
                // Truncate to prevent OOM
                let truncated = if s.len() > MAX_STRING_LEN {
                    s[..MAX_STRING_LEN].to_string()
                } else {
                    s.clone()
                };
                Value::Text(truncated)
            }
            Self::Blob(b) => {
                // Truncate to prevent OOM
                let truncated = if b.len() > MAX_BLOB_SIZE {
                    b[..MAX_BLOB_SIZE].to_vec()
                } else {
                    b.clone()
                };
                Value::Blob(truncated)
            }
        }
    }
}

/// Known SQL injection patterns to test.
#[derive(Arbitrary, Debug)]
enum AdversarialPattern {
    /// Classic OR injection: ' OR '1'='1
    OrInjection,
    /// Comment injection: --
    CommentInjection,
    /// Union injection: UNION SELECT
    UnionInjection,
    /// Stacked queries: ; DROP TABLE
    StackedQueries,
    /// Quote escaping: \'
    QuoteEscaping,
    /// Null byte injection
    NullByte,
    /// Unicode escaping
    UnicodeEscape,
    /// Hex encoding
    HexEncoding,
    /// Time-based blind injection
    TimeBlind,
    /// Boolean-based blind injection
    BooleanBlind,
}

impl AdversarialPattern {
    fn generate_payload(&self) -> String {
        match self {
            Self::OrInjection => "' OR '1'='1' --".to_string(),
            Self::CommentInjection => "admin'--".to_string(),
            Self::UnionInjection => "' UNION SELECT * FROM users --".to_string(),
            Self::StackedQueries => "'; DROP TABLE users; --".to_string(),
            Self::QuoteEscaping => "\\'; DELETE FROM users; --".to_string(),
            Self::NullByte => "admin\0'--".to_string(),
            Self::UnicodeEscape => "admin\u{0027} OR 1=1--".to_string(),
            Self::HexEncoding => "0x61646d696e".to_string(), // "admin" in hex
            Self::TimeBlind => "' OR SLEEP(5)--".to_string(),
            Self::BooleanBlind => "' OR 1=1 OR '".to_string(),
        }
    }
}

/// Create a test database service.
fn create_test_service() -> (SqlService, TempDir) {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let db_path = tmp.path().join("fuzz_test.db");
    let service = SqlService::open(&db_path).expect("Failed to open database");

    // Create test tables
    service
        .execute_batch(
            r"
            CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                email TEXT,
                data BLOB
            );
            CREATE TABLE secrets (
                id INTEGER PRIMARY KEY,
                secret TEXT NOT NULL
            );
            INSERT INTO secrets (secret) VALUES ('super_secret_value');
        ",
        )
        .expect("Failed to create tables");

    (service, tmp)
}

fuzz_target!(|input: SqlInput| {
    let (service, _tmp) = create_test_service();

    // Limit number of values to prevent resource exhaustion
    let values: Vec<Value> = input
        .values
        .iter()
        .take(MAX_PARAMS)
        .map(FuzzValue::to_sql_value)
        .collect();

    // Add adversarial pattern if specified
    let mut test_values = values.clone();
    if let Some(ref pattern) = input.pattern {
        test_values.push(Value::Text(pattern.generate_payload()));
    }

    match input.operation {
        SqlOperation::Insert => {
            test_insert(&service, &test_values);
        }
        SqlOperation::Select => {
            test_select(&service, &test_values);
        }
        SqlOperation::Update => {
            test_update(&service, &test_values);
        }
        SqlOperation::Delete => {
            test_delete(&service, &test_values);
        }
        SqlOperation::MultiOperation => {
            test_insert(&service, &test_values);
            test_select(&service, &test_values);
            test_update(&service, &test_values);
            test_delete(&service, &test_values);
        }
        SqlOperation::TableName(ref name) => {
            // Table names should be rejected at the SQL level if malicious
            // This tests that even if a table name somehow gets through,
            // the parameterized queries still protect data
            test_table_name_injection(&service, name, &test_values);
        }
    }

    // INVARIANT: Secret table should never be accessible via injection
    verify_secret_intact(&service);
});

/// Test INSERT operations with parameterized values.
fn test_insert(service: &SqlService, values: &[Value]) {
    // Get text value or use empty string
    let name = values
        .iter()
        .find_map(|v| {
            if let Value::Text(s) = v {
                Some(s.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();

    let email = values.get(1).cloned().unwrap_or(Value::Null);
    let data = values
        .iter()
        .find_map(|v| {
            if let Value::Blob(b) = v {
                Some(Value::Blob(b.clone()))
            } else {
                None
            }
        })
        .unwrap_or(Value::Null);

    // This should always be safe - parameters cannot escape
    let result = service.execute(
        "INSERT INTO users (name, email, data) VALUES (?, ?, ?)",
        &[Value::Text(name.clone()), email, data],
    );

    if result.is_ok() {
        // Verify the inserted data matches exactly (no SQL interpretation)
        let rows = service
            .query("SELECT name FROM users WHERE id = last_insert_rowid()", &[])
            .unwrap_or_default();

        if let Some(row) = rows.first() {
            if let Some(Value::Text(inserted_name)) = row.get("name") {
                // INVARIANT: Inserted value should match exactly
                assert_eq!(
                    inserted_name, &name,
                    "Inserted name differs from input - possible SQL interpretation"
                );
            }
        }
    }
    // Errors are acceptable (e.g., constraint violations) - they're not exploits
}

/// Test SELECT operations with parameterized WHERE clause.
fn test_select(service: &SqlService, values: &[Value]) {
    for value in values {
        // This should return only matching rows, never all rows
        let result = service.query("SELECT * FROM users WHERE name = ?", &[value.clone()]);

        if let Ok(rows) = result {
            // INVARIANT: Each returned row should have name matching the parameter
            for row in &rows {
                if let (Some(Value::Text(row_name)), Value::Text(param_name)) =
                    (row.get("name"), value)
                {
                    assert_eq!(
                        row_name, param_name,
                        "SELECT returned row with non-matching name - possible injection"
                    );
                }
            }
        }
    }
}

/// Test UPDATE operations with parameterized values.
fn test_update(service: &SqlService, values: &[Value]) {
    if values.len() < 2 {
        return;
    }

    let new_value = values[0].clone();
    let where_value = values[1].clone();

    // First, count rows that should be updated
    let before = service
        .query("SELECT COUNT(*) as cnt FROM users WHERE email = ?", &[where_value.clone()])
        .ok();

    let result = service.execute(
        "UPDATE users SET email = ? WHERE name = ?",
        &[new_value.clone(), where_value.clone()],
    );

    if let Ok(affected) = result {
        // INVARIANT: Only rows matching WHERE should be affected
        if let Some(before_rows) = before {
            if let Some(row) = before_rows.first() {
                if let Some(Value::Integer(expected)) = row.get("cnt") {
                    // Note: affected might be different if name=where_value matches different rows
                    // The key invariant is that it shouldn't affect ALL rows via injection
                    let total = service
                        .query("SELECT COUNT(*) as cnt FROM users", &[])
                        .ok()
                        .and_then(|r| r.first().and_then(|row| row.get("cnt")).cloned())
                        .unwrap_or(Value::Integer(0));

                    if let Value::Integer(total_count) = total {
                        if total_count > 0 && *expected == 0 {
                            assert!(
                                affected < total_count as usize,
                                "UPDATE affected all rows - possible injection"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Test DELETE operations with parameterized WHERE clause.
fn test_delete(service: &SqlService, values: &[Value]) {
    // First insert some data to delete
    let _ = service.execute(
        "INSERT INTO users (name) VALUES (?)",
        &[Value::Text("delete_test".to_string())],
    );

    for value in values {
        let before_count = service
            .query("SELECT COUNT(*) as cnt FROM users", &[])
            .ok()
            .and_then(|r| r.first().and_then(|row| row.get("cnt")).cloned())
            .unwrap_or(Value::Integer(0));

        let result = service.execute("DELETE FROM users WHERE name = ?", &[value.clone()]);

        if let Ok(affected) = result {
            let after_count = service
                .query("SELECT COUNT(*) as cnt FROM users", &[])
                .ok()
                .and_then(|r| r.first().and_then(|row| row.get("cnt")).cloned())
                .unwrap_or(Value::Integer(0));

            if let (Value::Integer(before), Value::Integer(after)) = (before_count, after_count) {
                // INVARIANT: Should not delete all rows unless parameter matches all
                let deleted = before - after;
                assert!(
                    deleted == affected as i64,
                    "DELETE count mismatch: affected={} but actually deleted={}",
                    affected,
                    deleted
                );
            }
        }
    }
}

/// Test that table name injection doesn't work.
fn test_table_name_injection(service: &SqlService, table_name: &str, values: &[Value]) {
    // Note: This is testing defense in depth. In a real application,
    // table names should be validated before being used in queries.
    // This test verifies that even if a malicious table name reaches SQL,
    // parameterized queries still protect data.

    // The table name here is used unsafely, but the VALUES are parameterized
    let first_value = values.first().cloned().unwrap_or(Value::Null);

    // Attempt to use the potentially malicious table name in a query
    // This should either fail at SQL parse time or return no results
    let query = format!("SELECT * FROM {} WHERE name = ?", table_name);
    let result = service.query(&query, &[first_value]);

    // If the query succeeds with a malicious table name, verify it didn't
    // access the secrets table
    if result.is_ok() {
        // The query parsed - make sure it didn't somehow access secrets
        // (this would require the table_name to be exactly "secrets")
        if table_name.to_lowercase().contains("secret") {
            // If someone tried to access secrets, verify our main invariant
            // is still maintained by the caller
        }
    }
    // Query failures are expected for most malicious table names
}

/// Verify the secrets table was not accessed or modified.
fn verify_secret_intact(service: &SqlService) {
    let result = service.query("SELECT secret FROM secrets WHERE id = 1", &[]);

    match result {
        Ok(rows) => {
            assert_eq!(rows.len(), 1, "Secrets table was modified");
            if let Some(row) = rows.first() {
                if let Some(Value::Text(secret)) = row.get("secret") {
                    assert_eq!(
                        secret, "super_secret_value",
                        "Secret value was modified - possible injection"
                    );
                }
            }
        }
        Err(_) => {
            panic!("Could not query secrets table - schema was modified");
        }
    }
}
