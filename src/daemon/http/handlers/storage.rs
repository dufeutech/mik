//! Storage service handlers.
//!
//! Handlers for object storage operations.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
};

use crate::daemon::services::storage::StorageService;

use super::super::types::{StorageListQuery, StorageListResponse, StorageObjectInfo};
use super::super::{AppError, SharedState, metrics};

/// Helper to get Storage service or return 503 if disabled.
async fn get_storage(state: &SharedState) -> Result<StorageService, AppError> {
    let state = state.read().await;
    state.storage.clone().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "Storage service is disabled. Enable it in ~/.mik/daemon.toml".to_string(),
        )
    })
}

/// GET /storage - List objects with optional prefix.
pub(crate) async fn storage_list(
    State(state): State<SharedState>,
    Query(query): Query<StorageListQuery>,
) -> Result<Json<StorageListResponse>, AppError> {
    metrics::record_storage_operation("list", None);
    let storage = get_storage(&state).await?;
    let mut objects: Vec<StorageObjectInfo> = storage
        .list_objects_async(query.prefix)
        .await?
        .into_iter()
        .map(StorageObjectInfo::from)
        .collect();

    // Apply limit if specified
    if let Some(limit) = query.limit {
        objects.truncate(limit);
    }

    Ok(Json(StorageListResponse { objects }))
}

/// GET /storage/*path - Get an object.
pub(crate) async fn storage_get(
    State(state): State<SharedState>,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let storage = get_storage(&state).await?;
    let (data, meta) = storage
        .get_object_async(path.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Object '{path}' not found")))?;

    metrics::record_storage_operation("get", Some(data.len() as u64));
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, meta.content_type)],
        data,
    ))
}

/// PUT /storage/*path - Store an object.
pub(crate) async fn storage_put(
    State(state): State<SharedState>,
    Path(path): Path<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    metrics::record_storage_operation("put", Some(body.len() as u64));
    let storage = get_storage(&state).await?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    storage
        .put_object_async(path, body.to_vec(), content_type)
        .await?;
    Ok(StatusCode::CREATED)
}

/// DELETE /storage/*path - Delete an object.
pub(crate) async fn storage_delete(
    State(state): State<SharedState>,
    Path(path): Path<String>,
) -> Result<StatusCode, AppError> {
    metrics::record_storage_operation("delete", None);
    let storage = get_storage(&state).await?;
    storage.delete_object_async(path).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// HEAD /storage/*path - Get object metadata.
pub(crate) async fn storage_head(
    State(state): State<SharedState>,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    metrics::record_storage_operation("head", None);
    let storage = get_storage(&state).await?;
    let meta = storage
        .head_object_async(path.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Object '{path}' not found")))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, meta.content_type),
            (header::CONTENT_LENGTH, meta.size.to_string()),
        ],
    ))
}
