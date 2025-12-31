//! SQL service handlers.
//!
//! Handlers for SQL database operations including queries, executes, and batch operations.

use std::time::Instant;

use axum::{Json, extract::State};

use crate::daemon::metrics;
use crate::daemon::services::sql::Value as SqlValue;

use super::super::{
    AppError, SharedState,
    types::{
        SqlBatchRequest, SqlBatchResponse, SqlExecuteRequest, SqlExecuteResponse, SqlQueryRequest,
        SqlQueryResponse,
    },
};

// =============================================================================
// Conversion Helpers
// =============================================================================

/// Convert JSON value to SQL value.
fn json_to_sql_value(v: &serde_json::Value) -> SqlValue {
    match v {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || {
                n.as_f64()
                    .map_or_else(|| SqlValue::Text(n.to_string()), SqlValue::Real)
            },
            SqlValue::Integer,
        ),
        serde_json::Value::String(s) => SqlValue::Text(s.clone()),
        _ => SqlValue::Text(v.to_string()),
    }
}

/// Convert SQL value to JSON value.
fn sql_to_json_value(v: &SqlValue) -> serde_json::Value {
    match v {
        SqlValue::Null => serde_json::Value::Null,
        SqlValue::Integer(i) => serde_json::json!(*i),
        SqlValue::Real(f) => serde_json::json!(*f),
        SqlValue::Text(s) => serde_json::json!(s),
        SqlValue::Blob(b) => {
            // Encode blob as hex string for JSON safety
            let hex = b.iter().fold(String::new(), |mut acc, byte| {
                use std::fmt::Write;
                let _ = write!(acc, "{byte:02x}");
                acc
            });
            serde_json::json!({ "hex": hex })
        },
    }
}

// =============================================================================
// HTTP Handlers
// =============================================================================

/// POST /sql/query - Execute a SELECT query.
pub(crate) async fn sql_query(
    State(state): State<SharedState>,
    Json(req): Json<SqlQueryRequest>,
) -> Result<Json<SqlQueryResponse>, AppError> {
    let start = Instant::now();
    // Clone SQL service and drop lock before async operation
    let sql = {
        let state = state.read().await;
        state.sql.clone()
    };
    let params: Vec<SqlValue> = req.params.iter().map(json_to_sql_value).collect();

    let rows = sql.query_async(req.sql, params).await?;
    metrics::record_sql_query("query", start.elapsed().as_secs_f64());

    // Extract column names from the first row (if available)
    let columns = rows.first().map(|r| r.columns.clone()).unwrap_or_default();

    let json_rows: Vec<Vec<serde_json::Value>> = rows
        .iter()
        .map(|row| row.values.iter().map(sql_to_json_value).collect())
        .collect();

    Ok(Json(SqlQueryResponse {
        columns,
        rows: json_rows,
    }))
}

/// POST /sql/execute - Execute an INSERT/UPDATE/DELETE statement.
pub(crate) async fn sql_execute(
    State(state): State<SharedState>,
    Json(req): Json<SqlExecuteRequest>,
) -> Result<Json<SqlExecuteResponse>, AppError> {
    let start = Instant::now();
    // Clone SQL service and drop lock before async operation
    let sql = {
        let state = state.read().await;
        state.sql.clone()
    };
    let params: Vec<SqlValue> = req.params.iter().map(json_to_sql_value).collect();

    let rows_affected = sql.execute_async(req.sql, params).await?;
    metrics::record_sql_query("execute", start.elapsed().as_secs_f64());

    Ok(Json(SqlExecuteResponse {
        rows_affected: rows_affected as u64,
    }))
}

/// POST /sql/batch - Execute a batch of statements.
pub(crate) async fn sql_batch(
    State(state): State<SharedState>,
    Json(req): Json<SqlBatchRequest>,
) -> Result<Json<SqlBatchResponse>, AppError> {
    let start = Instant::now();
    // Clone SQL service and drop lock before async operation
    let sql = {
        let state = state.read().await;
        state.sql.clone()
    };

    let mut results = Vec::with_capacity(req.statements.len());
    for stmt in &req.statements {
        let params: Vec<SqlValue> = stmt.params.iter().map(json_to_sql_value).collect();
        let rows_affected = sql.execute_async(stmt.sql.clone(), params).await?;
        results.push(SqlExecuteResponse {
            rows_affected: rows_affected as u64,
        });
    }
    metrics::record_sql_query("batch", start.elapsed().as_secs_f64());

    Ok(Json(SqlBatchResponse { results }))
}
