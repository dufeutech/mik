//! Service discovery handlers.
//!
//! Handlers for sidecar service registration and discovery.

use axum::{
    Json,
    extract::{Path, Query, State},
};

use super::super::types::{
    DeleteServiceResponse, HeartbeatResponse, ListServicesQuery, ListServicesResponse,
    RegisterServiceRequest, RegisterServiceResponse, ServiceResponse,
};
use super::super::{AppError, SharedState};
use crate::daemon::state::{ServiceType, Sidecar};

/// Parse a service type string into a `ServiceType` enum.
fn parse_service_type(s: &str) -> ServiceType {
    match s.to_lowercase().as_str() {
        "kv" => ServiceType::Kv,
        "sql" => ServiceType::Sql,
        "storage" => ServiceType::Storage,
        "queue" => ServiceType::Queue,
        other => other.strip_prefix("custom:").map_or_else(
            || ServiceType::Custom(other.to_string()),
            |name| ServiceType::Custom(name.to_string()),
        ),
    }
}

/// GET /services - List registered services.
pub(crate) async fn services_list(
    State(state): State<SharedState>,
    Query(query): Query<ListServicesQuery>,
) -> Result<Json<ListServicesResponse>, AppError> {
    let state = state.read().await;

    let sidecars = if let Some(ref type_filter) = query.service_type {
        let service_type = parse_service_type(type_filter);
        state
            .store
            .list_sidecars_by_type_async(service_type)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to list services: {e}")))?
    } else {
        state
            .store
            .list_sidecars_async()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to list services: {e}")))?
    };

    let services: Vec<ServiceResponse> = sidecars.iter().map(ServiceResponse::from).collect();

    Ok(Json(ListServicesResponse { services }))
}

/// POST /services - Register a new service.
pub(crate) async fn services_register(
    State(state): State<SharedState>,
    Json(req): Json<RegisterServiceRequest>,
) -> Result<Json<RegisterServiceResponse>, AppError> {
    let state = state.read().await;

    // Validate URL format
    if !req.url.starts_with("http://") && !req.url.starts_with("https://") {
        return Err(AppError::BadRequest(
            "URL must start with http:// or https://".to_string(),
        ));
    }

    let now = chrono::Utc::now();
    let sidecar = Sidecar {
        name: req.name.clone(),
        service_type: parse_service_type(&req.service_type),
        url: req.url,
        description: req.description,
        registered_at: now,
        last_heartbeat: now,
        healthy: true,
    };

    state
        .store
        .save_sidecar_async(sidecar)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to register service: {e}")))?;

    tracing::info!(name = %req.name, "Service registered");

    Ok(Json(RegisterServiceResponse {
        name: req.name,
        registered: true,
    }))
}

/// GET /services/{name} - Get a specific service.
pub(crate) async fn services_get(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<ServiceResponse>, AppError> {
    let state = state.read().await;

    let sidecar = state
        .store
        .get_sidecar_async(name.clone())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to get service: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Service '{name}' not found")))?;

    Ok(Json(ServiceResponse::from(&sidecar)))
}

/// DELETE /services/{name} - Unregister a service.
pub(crate) async fn services_delete(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<DeleteServiceResponse>, AppError> {
    let state = state.read().await;

    let deleted = state
        .store
        .remove_sidecar_async(name.clone())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to delete service: {e}")))?;

    if deleted {
        tracing::info!(name = %name, "Service unregistered");
    }

    Ok(Json(DeleteServiceResponse { name, deleted }))
}

/// POST /services/{name}/heartbeat - Update service heartbeat.
pub(crate) async fn services_heartbeat(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<HeartbeatResponse>, AppError> {
    let state = state.read().await;

    let updated = state
        .store
        .update_sidecar_heartbeat_async(name.clone(), true)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to update heartbeat: {e}")))?;

    if !updated {
        return Err(AppError::NotFound(format!("Service '{name}' not found")));
    }

    Ok(Json(HeartbeatResponse { name, updated }))
}
