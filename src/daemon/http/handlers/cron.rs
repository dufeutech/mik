//! Cron scheduler handlers.
//!
//! Handlers for scheduled job management including listing, creating,
//! updating, deleting, triggering, and viewing execution history.

use axum::{
    Json,
    extract::{Path, State},
};

use super::super::types::{
    CronCreateRequest, CronCreateResponse, CronDeleteResponse, CronExecutionInfo,
    CronHistoryResponse, CronJobResponse, CronTriggerResponse, CronUpdateRequest,
    CronUpdateResponse, ListCronJobsResponse,
};
use super::super::{AppError, SharedState};
use crate::daemon::cron::ScheduleConfig;

/// GET /cron - List all scheduled jobs.
pub(crate) async fn cron_list(
    State(state): State<SharedState>,
) -> Result<Json<ListCronJobsResponse>, AppError> {
    let state = state.read().await;
    let jobs = state.cron.list_jobs().await;

    let responses: Vec<CronJobResponse> = jobs
        .iter()
        .map(|job| {
            // Determine status based on last execution
            let status_str = match &job.last_execution {
                Some(exec) if exec.success => "idle",
                Some(_) => "failed",
                None => "idle",
            };

            CronJobResponse {
                name: job.name.clone(),
                cron: job.cron.clone(),
                module: job.module.clone(),
                method: job.method.clone(),
                path: job.path.clone(),
                enabled: job.enabled,
                status: status_str.to_string(),
                last_run: job
                    .last_execution
                    .as_ref()
                    .map(|e| e.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                next_run: job
                    .next_run
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            }
        })
        .collect();

    Ok(Json(ListCronJobsResponse { jobs: responses }))
}

/// POST /cron - Create a new cron job.
pub(crate) async fn cron_create(
    State(state): State<SharedState>,
    Json(req): Json<CronCreateRequest>,
) -> Result<Json<CronCreateResponse>, AppError> {
    use std::path::PathBuf;

    let state = state.read().await;

    // Create schedule config
    let config = ScheduleConfig {
        name: req.name.clone(),
        module: PathBuf::from(&req.module),
        cron: req.cron,
        method: req.method,
        path: req.path,
        enabled: req.enabled,
        port: req.port,
        body: req.body,
        headers: req.headers,
        health_path: req.health_path,
    };

    // Add the job to scheduler
    state
        .cron
        .add_job(config.clone())
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to create cron job: {e}")))?;

    // Persist to database
    state
        .store
        .save_cron_job_async(config)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to persist cron job: {e}")))?;

    tracing::info!(name = %req.name, "Cron job created and persisted");

    Ok(Json(CronCreateResponse {
        name: req.name,
        created: true,
    }))
}

/// GET /cron/:name - Get job details.
pub(crate) async fn cron_get(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronJobResponse>, AppError> {
    let state = state.read().await;
    let job = state
        .cron
        .get_job(&name)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Cron job '{name}' not found")))?;

    // Determine status based on last execution
    let status_str = match &job.last_execution {
        Some(exec) if exec.success => "idle",
        Some(_) => "failed",
        None => "idle",
    };

    Ok(Json(CronJobResponse {
        name: job.name.clone(),
        cron: job.cron.clone(),
        module: job.module.clone(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: job.enabled,
        status: status_str.to_string(),
        last_run: job
            .last_execution
            .as_ref()
            .map(|e| e.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        next_run: job
            .next_run
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
    }))
}

/// DELETE /cron/:name - Delete a cron job.
pub(crate) async fn cron_delete(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronDeleteResponse>, AppError> {
    let state = state.read().await;

    let deleted = state
        .cron
        .remove_job(&name)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to delete cron job: {e}")))?;

    if !deleted {
        return Err(AppError::NotFound(format!("Cron job '{name}' not found")));
    }

    // Remove from database
    state
        .store
        .remove_cron_job_async(name.clone())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to remove cron job from database: {e}")))?;

    tracing::info!(name = %name, "Cron job deleted and removed from database");

    Ok(Json(CronDeleteResponse {
        name,
        deleted: true,
    }))
}

/// PATCH /cron/:name - Update a cron job (pause/resume).
pub(crate) async fn cron_update(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<CronUpdateRequest>,
) -> Result<Json<CronUpdateResponse>, AppError> {
    let state = state.read().await;

    // Check if there's anything to update
    let Some(enabled) = req.enabled else {
        return Err(AppError::BadRequest(
            "No fields to update. Provide 'enabled' field.".to_string(),
        ));
    };

    // Update the enabled state
    let new_enabled = state
        .cron
        .set_enabled(&name, enabled)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Cron job '{name}' not found")))?;

    // Persist the updated config to database
    if let Some(config) = state.cron.get_config(&name).await {
        state
            .store
            .save_cron_job_async(config)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to persist cron job update: {e}")))?;
    }

    let action = if new_enabled { "resumed" } else { "paused" };
    tracing::info!(name = %name, enabled = new_enabled, "Cron job {action}");

    Ok(Json(CronUpdateResponse {
        name,
        enabled: new_enabled,
        updated: true,
    }))
}

/// POST /cron/:name/trigger - Manually trigger a job.
pub(crate) async fn cron_trigger(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronTriggerResponse>, AppError> {
    let state = state.read().await;

    // Check job exists
    if state.cron.get_job(&name).await.is_none() {
        return Err(AppError::NotFound(format!("Cron job '{name}' not found")));
    }

    let execution = state
        .cron
        .trigger_job(&name)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to trigger job: {e}")))?;

    // Persist execution to database
    if let Err(e) = state
        .store
        .save_job_execution_async(execution.clone())
        .await
    {
        tracing::warn!(job = %name, error = %e, "Failed to persist job execution");
    }

    // Cleanup old executions (keep last 100)
    if let Err(e) = state
        .store
        .cleanup_job_executions_async(name.clone(), 100)
        .await
    {
        tracing::warn!(job = %name, error = %e, "Failed to cleanup old executions");
    }

    Ok(Json(CronTriggerResponse {
        job_name: name,
        execution_id: execution.id,
        triggered: true,
    }))
}

/// GET /cron/:name/history - Get job execution history.
pub(crate) async fn cron_history(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CronHistoryResponse>, AppError> {
    let state = state.read().await;

    // Check job exists
    if state.cron.get_job(&name).await.is_none() {
        return Err(AppError::NotFound(format!("Cron job '{name}' not found")));
    }

    // Get history from database (persistent)
    let history = state
        .store
        .list_job_executions_async(name.clone(), 100)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read job history: {e}")))?;

    let executions: Vec<CronExecutionInfo> = history
        .iter()
        .map(|exec| CronExecutionInfo {
            id: exec.id.clone(),
            started_at: exec.started_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            completed_at: exec
                .completed_at
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            duration_ms: exec.duration_ms,
            success: exec.success,
            error: exec.error.clone(),
            manual: exec.manual,
        })
        .collect();

    Ok(Json(CronHistoryResponse {
        job_name: name,
        executions,
    }))
}
