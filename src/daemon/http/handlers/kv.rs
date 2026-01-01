//! KV service handlers.
//!
//! Handlers for key-value store operations including listing keys,
//! getting values, setting values with optional TTL, and deleting keys.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};

use super::super::types::{KvGetResponse, KvListQuery, KvListResponse, KvSetRequest};
use super::super::{AppError, SharedState, metrics};

/// Helper to get KV store or return 503 if disabled.
async fn get_kv(state: &SharedState) -> Result<crate::daemon::services::kv::KvStore, AppError> {
    let state = state.read().await;
    state.kv.clone().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "KV service is disabled. Enable it in ~/.mik/daemon.toml".to_string(),
        )
    })
}

/// GET /kv - List all keys with optional prefix filter.
pub(crate) async fn kv_list(
    State(state): State<SharedState>,
    Query(query): Query<KvListQuery>,
) -> Result<Json<KvListResponse>, AppError> {
    metrics::record_kv_operation("list");
    let kv = get_kv(&state).await?;
    let keys = kv.list_keys_async(query.prefix).await?;
    Ok(Json(KvListResponse { keys }))
}

/// GET /kv/:key - Get a value by key.
pub(crate) async fn kv_get(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> Result<Json<KvGetResponse>, AppError> {
    metrics::record_kv_operation("get");
    let kv = get_kv(&state).await?;
    let bytes = kv
        .get_async(key.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Key '{key}' not found")))?;

    let value = String::from_utf8(bytes)
        .map_err(|_| AppError::Internal("Value is not valid UTF-8".to_string()))?;

    Ok(Json(KvGetResponse { key, value }))
}

/// PUT /kv/:key - Set a value with optional TTL.
pub(crate) async fn kv_set(
    State(state): State<SharedState>,
    Path(key): Path<String>,
    Json(req): Json<KvSetRequest>,
) -> Result<StatusCode, AppError> {
    metrics::record_kv_operation("set");
    let kv = get_kv(&state).await?;
    let value_bytes = req.value.into_bytes();

    match req.ttl {
        Some(ttl) => kv.set_with_ttl_async(key, value_bytes, ttl).await?,
        None => kv.set_async(key, value_bytes).await?,
    }

    Ok(StatusCode::OK)
}

/// DELETE /kv/:key - Delete a key.
pub(crate) async fn kv_delete(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> Result<StatusCode, AppError> {
    metrics::record_kv_operation("delete");
    let kv = get_kv(&state).await?;
    kv.delete_async(key).await?;
    Ok(StatusCode::NO_CONTENT)
}
