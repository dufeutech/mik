//! Instance management handlers.
//!
//! Handlers for managing WASM instances (list, start, stop, restart, logs).

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};

use super::super::types::{
    HealthResponse, InstanceResponse, ListInstancesResponse, LogsQuery, LogsResponse,
    StartInstanceRequest, VersionResponse, validate_instance_name,
};
use super::super::{AppError, SharedState};
use crate::daemon::cron::parse_schedules_from_manifest;
use crate::daemon::metrics;
use crate::daemon::process::{self, SpawnConfig};
use crate::daemon::state::{Instance, Status};

/// GET /instances - List all instances.
pub(crate) async fn list_instances(
    State(state): State<SharedState>,
) -> Result<Json<ListInstancesResponse>, AppError> {
    // Clone store and drop lock before async operation
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instances = store.list_instances_async().await?;

    // Track counts for metrics
    let mut running_count = 0u64;
    let mut stopped_count = 0u64;
    let mut crashed_count = 0u64;

    let mut responses = Vec::with_capacity(instances.len());
    for instance in &instances {
        // Check actual running status
        let mut response = InstanceResponse::from(instance);
        let is_actually_running =
            instance.status == Status::Running && process::is_running(instance.pid)?;

        if instance.status == Status::Running && !is_actually_running {
            response.status = "crashed".to_string();
            response.uptime = None;
            crashed_count += 1;
        } else {
            match &instance.status {
                Status::Running => {
                    running_count += 1;
                    // Record uptime for running instances
                    #[allow(clippy::cast_precision_loss)] // Uptime in seconds is safe
                    let uptime = chrono::Utc::now()
                        .signed_duration_since(instance.started_at)
                        .num_seconds() as f64;
                    metrics::set_instance_uptime(&instance.name, uptime);
                },
                Status::Stopped => stopped_count += 1,
                Status::Crashed { .. } => crashed_count += 1,
            }
        }
        responses.push(response);
    }

    // Update instance count metrics
    metrics::set_instance_count("running", running_count);
    metrics::set_instance_count("stopped", stopped_count);
    metrics::set_instance_count("crashed", crashed_count);

    Ok(Json(ListInstancesResponse {
        instances: responses,
    }))
}

/// POST /instances - Start a new instance.
pub(crate) async fn start_instance(
    State(state): State<SharedState>,
    Json(req): Json<StartInstanceRequest>,
) -> Result<(StatusCode, Json<InstanceResponse>), AppError> {
    // Validate instance name to prevent path traversal and other security issues
    if let Err(e) = validate_instance_name(&req.name) {
        return Err(AppError::BadRequest(e));
    }

    // Clone store for async check, then drop lock
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    // Check if instance already exists and is running (non-blocking)
    if let Some(existing) = store.get_instance_async(req.name.clone()).await?
        && existing.status == Status::Running
        && process::is_running(existing.pid)?
    {
        return Err(AppError::Conflict(format!(
            "Instance '{}' is already running on port {}",
            req.name, existing.port
        )));
    }

    // Resolve config path
    let config_path = match req.config {
        Some(ref path) => PathBuf::from(path),
        None => std::env::current_dir()?.join("mik.toml"),
    };

    if !config_path.exists() {
        return Err(AppError::BadRequest(format!(
            "Config file not found: {}",
            config_path.display()
        )));
    }

    let working_dir = match req.working_dir {
        Some(ref path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };

    let port = req.port.unwrap_or(3000);

    let spawn_config = SpawnConfig {
        name: req.name.clone(),
        port,
        config_path: config_path.clone(),
        working_dir: working_dir.clone(),
        hot_reload: false,
    };

    let info = process::spawn_instance(&spawn_config)?;

    // Save instance state
    let instance = Instance {
        name: req.name.clone(),
        port,
        pid: info.pid,
        status: Status::Running,
        config: config_path,
        started_at: chrono::Utc::now(),
        modules: vec![],
        auto_restart: req.auto_restart,
        restart_count: 0,
        last_restart_at: None,
    };

    // Save instance asynchronously (non-blocking)
    store.save_instance_async(instance.clone()).await?;

    // Parse and register schedules from mik.toml
    // Need to hold lock for cron operations
    let state = state.write().await;
    match parse_schedules_from_manifest(&instance.config) {
        Ok(schedules) if !schedules.is_empty() => {
            tracing::info!(
                instance = %req.name,
                count = schedules.len(),
                "Loading schedules from mik.toml"
            );

            for mut schedule in schedules {
                // Scope job name to instance: "instance_name:job_name"
                let scoped_name = format!("{}:{}", req.name, schedule.name);
                schedule.name.clone_from(&scoped_name);

                // Use instance port if schedule doesn't specify one
                if schedule.port == 3000 {
                    schedule.port = port;
                }

                // Add to scheduler
                match state.cron.add_job(schedule.clone()).await {
                    Ok(()) => {
                        // Persist to database (already async)
                        if let Err(e) = state.store.save_cron_job_async(schedule.clone()).await {
                            tracing::warn!(
                                job = %scoped_name,
                                error = %e,
                                "Failed to persist schedule job"
                            );
                        } else {
                            tracing::info!(job = %scoped_name, "Registered schedule from mik.toml");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            job = %scoped_name,
                            error = %e,
                            "Failed to register schedule from mik.toml"
                        );
                    },
                }
            }
        },
        Ok(_) => {
            // No schedules in config, that's fine
        },
        Err(e) => {
            tracing::warn!(
                instance = %req.name,
                error = %e,
                "Failed to parse schedules from mik.toml"
            );
        },
    }

    Ok((StatusCode::CREATED, Json(InstanceResponse::from(&instance))))
}

/// GET /instances/:name - Get instance details.
pub(crate) async fn get_instance(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<InstanceResponse>, AppError> {
    // Clone store and drop lock before async operation
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instance = store
        .get_instance_async(name.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Instance '{name}' not found")))?;

    let mut response = InstanceResponse::from(&instance);

    // Check actual running status
    if instance.status == Status::Running && !process::is_running(instance.pid)? {
        response.status = "crashed".to_string();
        response.uptime = None;
    }

    Ok(Json(response))
}

/// DELETE /instances/:name - Stop an instance.
pub(crate) async fn stop_instance(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<InstanceResponse>, AppError> {
    // Clone store for async operations
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instance = store
        .get_instance_async(name.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Instance '{name}' not found")))?;

    if instance.status != Status::Running {
        return Err(AppError::BadRequest(format!(
            "Instance '{}' is not running (status: {:?})",
            name, instance.status
        )));
    }

    // Kill the process
    if process::is_running(instance.pid)? {
        process::kill_instance(instance.pid)?;
    }

    // Remove instance-scoped cron jobs (those starting with "instance_name:")
    // Need to hold write lock for cron operations
    let state = state.write().await;
    let prefix = format!("{name}:");
    let jobs = state.cron.list_jobs().await;
    for job in jobs {
        if job.name.starts_with(&prefix) {
            if let Err(e) = state.cron.remove_job(&job.name).await {
                tracing::warn!(job = %job.name, error = %e, "Failed to remove cron job on instance stop");
            } else {
                // Also remove from database (already async)
                if let Err(e) = state.store.remove_cron_job_async(job.name.clone()).await {
                    tracing::warn!(job = %job.name, error = %e, "Failed to remove cron job from database");
                } else {
                    tracing::info!(job = %job.name, "Removed cron job on instance stop");
                }
            }
        }
    }

    // Update state asynchronously
    let mut updated = instance;
    updated.status = Status::Stopped;
    store.save_instance_async(updated.clone()).await?;

    Ok(Json(InstanceResponse::from(&updated)))
}

/// POST /instances/:name/restart - Restart an instance.
pub(crate) async fn restart_instance(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<InstanceResponse>, AppError> {
    // Clone store for async operations
    let store = {
        let state = state.read().await;
        state.store.clone()
    };

    let instance = store
        .get_instance_async(name.clone())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Instance '{name}' not found")))?;

    // Stop if running
    if instance.status == Status::Running && process::is_running(instance.pid)? {
        process::kill_instance(instance.pid)?;
    }

    // Remove old instance-scoped cron jobs before restart
    // Need write lock for cron operations
    let state_guard = state.write().await;
    let prefix = format!("{name}:");
    let jobs = state_guard.cron.list_jobs().await;
    for job in jobs {
        if job.name.starts_with(&prefix) {
            if let Err(e) = state_guard.cron.remove_job(&job.name).await {
                tracing::warn!(job = %job.name, error = %e, "Failed to remove cron job on restart");
            } else {
                let _ = state_guard
                    .store
                    .remove_cron_job_async(job.name.clone())
                    .await;
            }
        }
    }
    // Drop write lock before spawn_blocking operations
    drop(state_guard);

    // Restart with same config
    let spawn_config = SpawnConfig {
        name: instance.name.clone(),
        port: instance.port,
        config_path: instance.config.clone(),
        working_dir: instance
            .config
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf(),
        hot_reload: false,
    };

    let info = process::spawn_instance(&spawn_config)?;

    // Update instance state (preserve auto_restart settings)
    let updated = Instance {
        name: instance.name.clone(),
        port: instance.port,
        pid: info.pid,
        status: Status::Running,
        config: instance.config.clone(),
        started_at: chrono::Utc::now(),
        modules: instance.modules.clone(),
        auto_restart: instance.auto_restart,
        restart_count: instance.restart_count,
        last_restart_at: instance.last_restart_at,
    };

    // Save asynchronously
    store.save_instance_async(updated.clone()).await?;

    // Re-parse and register schedules from mik.toml
    // Need write lock again for cron operations
    let state_guard = state.write().await;
    if let Ok(schedules) = parse_schedules_from_manifest(&instance.config) {
        for mut schedule in schedules {
            let scoped_name = format!("{name}:{}", schedule.name);
            schedule.name.clone_from(&scoped_name);
            if schedule.port == 3000 {
                schedule.port = instance.port;
            }

            if let Ok(()) = state_guard.cron.add_job(schedule.clone()).await {
                let _ = state_guard.store.save_cron_job_async(schedule).await;
                tracing::info!(job = %scoped_name, "Re-registered schedule on restart");
            }
        }
    }

    Ok(Json(InstanceResponse::from(&updated)))
}

/// GET /instances/:name/logs - Get instance logs.
pub(crate) async fn get_logs(
    Path(name): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, AppError> {
    let home = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to get home directory".to_string()))?;

    let log_path = home.join(".mik").join("logs").join(format!("{name}.log"));

    if !log_path.exists() {
        return Ok(Json(LogsResponse {
            name,
            lines: vec![],
        }));
    }

    let lines_count = query.lines.unwrap_or(50);
    let lines = process::tail_log(&log_path, lines_count)?;

    Ok(Json(LogsResponse { name, lines }))
}

/// GET /health - Health check.
pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime: "running".to_string(),
    })
}

/// GET /version - Version info.
pub(crate) async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: "release".to_string(),
    })
}
