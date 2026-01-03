//! Built-in HTTP endpoints for health and metrics.
//!
//! This module provides the standard operational endpoints:
//! - `/health`: Health check with optional verbose mode
//! - `/metrics`: Prometheus-format metrics

use crate::runtime::SharedState;
use crate::runtime::compression::maybe_compress_response;
use crate::runtime::types::HealthDetail;
use anyhow::Result;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response};
use std::sync::Arc;
use std::time::Instant;
use tracing::info;
use uuid::Uuid;

/// Handle health check endpoint.
pub(crate) fn handle_health_endpoint(
    shared: &Arc<SharedState>,
    req: &Request<hyper::body::Incoming>,
    request_id: &Uuid,
    traceparent: &str,
    start_time: Instant,
    client_accepts_gzip: bool,
) -> Result<Response<Full<Bytes>>> {
    let duration = start_time.elapsed();
    info!(duration_ms = duration.as_millis() as u64, "Health check");

    let detail = if req
        .uri()
        .query()
        .and_then(|q| q.split('&').find(|s| s.starts_with("verbose=")))
        .and_then(|s| s.strip_prefix("verbose="))
        .is_some_and(|v| v == "true" || v == "1")
    {
        HealthDetail::Full
    } else {
        HealthDetail::Summary
    };

    let health = shared.get_health_status(detail);
    let body = serde_json::to_string_pretty(&health).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize health status to JSON");
        r#"{"status":"error"}"#.to_string()
    });

    let response = Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .header("X-Request-ID", request_id.to_string())
        .header("traceparent", traceparent)
        .body(Full::new(Bytes::from(body)))?;

    Ok(maybe_compress_response(response, client_accepts_gzip))
}

/// Handle metrics endpoint (Prometheus format).
pub(crate) fn handle_metrics_endpoint(
    shared: &Arc<SharedState>,
    request_id: &Uuid,
    traceparent: &str,
    start_time: Instant,
    client_accepts_gzip: bool,
) -> Result<Response<Full<Bytes>>> {
    let duration = start_time.elapsed();
    info!(duration_ms = duration.as_millis() as u64, "Metrics request");

    let metrics = shared.get_prometheus_metrics();

    let response = Response::builder()
        .status(200)
        .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        .header("X-Request-ID", request_id.to_string())
        .header("traceparent", traceparent)
        .body(Full::new(Bytes::from(metrics)))?;

    Ok(maybe_compress_response(response, client_accepts_gzip))
}
