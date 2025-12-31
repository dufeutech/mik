//! Tests for the HTTP API server.

use super::*;
use axum::body::Body;
use axum::http::{Method, Request};
use tower::ServiceExt;

// Note: format_duration tests are in utils.rs

/// Create a test router with all service routes.
async fn create_test_app() -> Router {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    // Leak the temp_dir so it lives for the duration of tests
    // In real tests, we'd use a proper fixture
    let data_dir: &'static std::path::Path = Box::leak(Box::new(data_dir));

    let store = StateStore::open(data_dir.join("state.redb")).unwrap();
    let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
    let sql = SqlService::open(data_dir.join("sql.db")).unwrap();
    let storage = StorageService::open(data_dir.join("storage")).unwrap();
    let cron = CronScheduler::new().await.unwrap();

    let app_state = Arc::new(RwLock::new(AppState {
        store,
        kv,
        sql,
        storage,
        cron,
    }));

    Router::new()
        // KV service
        .route("/kv", get(kv_list))
        .route("/kv/{key}", get(kv_get))
        .route("/kv/{key}", put(kv_set))
        .route("/kv/{key}", delete(kv_delete))
        // SQL service
        .route("/sql/query", post(sql_query))
        .route("/sql/execute", post(sql_execute))
        .route("/sql/batch", post(sql_batch))
        // Storage service
        .route("/storage", get(storage_list))
        .route("/storage/{*path}", get(storage_get))
        .route("/storage/{*path}", put(storage_put))
        .route("/storage/{*path}", delete(storage_delete))
        .route("/storage/{*path}", head(storage_head))
        // Service discovery
        .route("/services", get(services_list).post(services_register))
        .route(
            "/services/{name}",
            get(services_get).delete(services_delete),
        )
        .route("/services/{name}/heartbeat", post(services_heartbeat))
        // System endpoints
        .route("/health", get(health))
        .route("/version", get(version))
        .with_state(app_state)
}

// =========================================================================
// Health & Version Tests
// =========================================================================

#[tokio::test]
async fn test_health_endpoint() {
    let app = create_test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let health: HealthResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(health.status, "healthy");
}

#[tokio::test]
async fn test_version_endpoint() {
    let app = create_test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let version: VersionResponse = serde_json::from_slice(&body).unwrap();
    assert!(!version.version.is_empty());
}

// =========================================================================
// KV Service Tests
// =========================================================================

#[tokio::test]
async fn test_kv_set_and_get() {
    let app = create_test_app().await;

    // Set a value
    let set_request = Request::builder()
        .method(Method::PUT)
        .uri("/kv/test-key")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"value": "test-value"}"#))
        .unwrap();

    let response = app.clone().oneshot(set_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Get the value
    let get_request = Request::builder()
        .uri("/kv/test-key")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let kv_response: KvGetResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(kv_response.key, "test-key");
    assert_eq!(kv_response.value, "test-value");
}

#[tokio::test]
async fn test_kv_get_not_found() {
    let app = create_test_app().await;

    let request = Request::builder()
        .uri("/kv/nonexistent-key")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_kv_delete() {
    let app = create_test_app().await;

    // First set a value
    let set_request = Request::builder()
        .method(Method::PUT)
        .uri("/kv/delete-key")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"value": "to-delete"}"#))
        .unwrap();

    let response = app.clone().oneshot(set_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Delete the key
    let delete_request = Request::builder()
        .method(Method::DELETE)
        .uri("/kv/delete-key")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(delete_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify it's gone
    let get_request = Request::builder()
        .uri("/kv/delete-key")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_kv_list_keys() {
    let app = create_test_app().await;

    // Set multiple keys
    for key in ["prefix:a", "prefix:b", "other:c"] {
        let set_request = Request::builder()
            .method(Method::PUT)
            .uri(format!("/kv/{key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"value": "test"}"#))
            .unwrap();

        app.clone().oneshot(set_request).await.unwrap();
    }

    // List all keys
    let request = Request::builder().uri("/kv").body(Body::empty()).unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_response: KvListResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list_response.keys.len(), 3);

    // List with prefix filter
    let request = Request::builder()
        .uri("/kv?prefix=prefix:")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_response: KvListResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list_response.keys.len(), 2);
}

#[tokio::test]
async fn test_kv_set_with_ttl() {
    let app = create_test_app().await;

    let set_request = Request::builder()
        .method(Method::PUT)
        .uri("/kv/ttl-key")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"value": "expires-soon", "ttl": 3600}"#))
        .unwrap();

    let response = app.clone().oneshot(set_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify it exists
    let get_request = Request::builder()
        .uri("/kv/ttl-key")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// =========================================================================
// SQL Service Tests
// =========================================================================

#[tokio::test]
async fn test_sql_execute_create_table() {
    let app = create_test_app().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/sql/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "sql": "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
        }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_sql_execute_insert_and_query() {
    let app = create_test_app().await;

    // Create table
    let create_request = Request::builder()
        .method(Method::POST)
        .uri("/sql/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "sql": "CREATE TABLE test_users (id INTEGER PRIMARY KEY, name TEXT)"
        }"#,
        ))
        .unwrap();

    app.clone().oneshot(create_request).await.unwrap();

    // Insert data
    let insert_request = Request::builder()
        .method(Method::POST)
        .uri("/sql/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "sql": "INSERT INTO test_users (name) VALUES (?)",
            "params": ["Alice"]
        }"#,
        ))
        .unwrap();

    let response = app.clone().oneshot(insert_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let execute_response: SqlExecuteResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(execute_response.rows_affected, 1);

    // Query data
    let query_request = Request::builder()
        .method(Method::POST)
        .uri("/sql/query")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "sql": "SELECT * FROM test_users WHERE name = ?",
            "params": ["Alice"]
        }"#,
        ))
        .unwrap();

    let response = app.oneshot(query_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let query_response: SqlQueryResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(query_response.rows.len(), 1);
    assert!(query_response.columns.contains(&"name".to_string()));
}

#[tokio::test]
async fn test_sql_batch_execute() {
    let app = create_test_app().await;

    // Create table first
    let create_request = Request::builder()
        .method(Method::POST)
        .uri("/sql/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "sql": "CREATE TABLE batch_test (id INTEGER PRIMARY KEY, value TEXT)"
        }"#,
        ))
        .unwrap();

    app.clone().oneshot(create_request).await.unwrap();

    // Batch insert
    let batch_request = Request::builder()
        .method(Method::POST)
        .uri("/sql/batch")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "statements": [
                {"sql": "INSERT INTO batch_test (value) VALUES (?)", "params": ["one"]},
                {"sql": "INSERT INTO batch_test (value) VALUES (?)", "params": ["two"]},
                {"sql": "INSERT INTO batch_test (value) VALUES (?)", "params": ["three"]}
            ]
        }"#,
        ))
        .unwrap();

    let response = app.oneshot(batch_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let batch_response: SqlBatchResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(batch_response.results.len(), 3);
    for result in batch_response.results {
        assert_eq!(result.rows_affected, 1);
    }
}

// =========================================================================
// Storage Service Tests
// =========================================================================

#[tokio::test]
async fn test_storage_put_and_get() {
    let app = create_test_app().await;

    let content = b"Hello, World!";

    // Put an object
    let put_request = Request::builder()
        .method(Method::PUT)
        .uri("/storage/test/hello.txt")
        .header("content-type", "text/plain")
        .body(Body::from(content.to_vec()))
        .unwrap();

    let response = app.clone().oneshot(put_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // Get the object
    let get_request = Request::builder()
        .uri("/storage/test/hello.txt")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], content);
}

#[tokio::test]
async fn test_storage_get_not_found() {
    let app = create_test_app().await;

    let request = Request::builder()
        .uri("/storage/nonexistent/file.txt")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_storage_head() {
    let app = create_test_app().await;

    let content = b"Test content for head request";

    // Put an object
    let put_request = Request::builder()
        .method(Method::PUT)
        .uri("/storage/head-test.txt")
        .header("content-type", "text/plain")
        .body(Body::from(content.to_vec()))
        .unwrap();

    app.clone().oneshot(put_request).await.unwrap();

    // Head request
    let head_request = Request::builder()
        .method(Method::HEAD)
        .uri("/storage/head-test.txt")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(head_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain"
    );
    assert_eq!(
        response.headers().get("content-length").unwrap(),
        &content.len().to_string()
    );
}

#[tokio::test]
async fn test_storage_delete() {
    let app = create_test_app().await;

    // Put an object
    let put_request = Request::builder()
        .method(Method::PUT)
        .uri("/storage/to-delete.txt")
        .header("content-type", "text/plain")
        .body(Body::from("delete me"))
        .unwrap();

    app.clone().oneshot(put_request).await.unwrap();

    // Delete the object
    let delete_request = Request::builder()
        .method(Method::DELETE)
        .uri("/storage/to-delete.txt")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(delete_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify it's gone
    let get_request = Request::builder()
        .uri("/storage/to-delete.txt")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_storage_list() {
    let app = create_test_app().await;

    // Put multiple objects
    for path in ["docs/a.txt", "docs/b.txt", "images/c.png"] {
        let put_request = Request::builder()
            .method(Method::PUT)
            .uri(format!("/storage/{path}"))
            .header("content-type", "text/plain")
            .body(Body::from("content"))
            .unwrap();

        app.clone().oneshot(put_request).await.unwrap();
    }

    // List all objects
    let request = Request::builder()
        .uri("/storage")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_response: StorageListResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list_response.objects.len(), 3);

    // List with prefix
    let request = Request::builder()
        .uri("/storage?prefix=docs/")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_response: StorageListResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list_response.objects.len(), 2);
}

// =========================================================================
// Error Handling Tests
// =========================================================================

#[tokio::test]
async fn test_sql_invalid_query() {
    let app = create_test_app().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/sql/query")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"sql": "SELECT * FROM nonexistent_table"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn test_kv_invalid_json() {
    let app = create_test_app().await;

    let request = Request::builder()
        .method(Method::PUT)
        .uri("/kv/test-key")
        .header("content-type", "application/json")
        .body(Body::from("not valid json"))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    // Should return 422 Unprocessable Entity or 400 Bad Request
    assert!(response.status().is_client_error());
}

// =========================================================================
// Service Discovery Tests
// =========================================================================

#[tokio::test]
async fn test_services_list_empty() {
    let app = create_test_app().await;

    let request = Request::builder()
        .uri("/services")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
    assert!(list.services.is_empty());
}

#[tokio::test]
async fn test_services_register_and_get() {
    let app = create_test_app().await;

    // Register a service
    let register_request = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "test-sidecar",
            "service_type": "sql",
            "url": "http://localhost:9001",
            "description": "Test SQL sidecar"
        }"#,
        ))
        .unwrap();

    let response = app.clone().oneshot(register_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let register_resp: RegisterServiceResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(register_resp.name, "test-sidecar");
    assert!(register_resp.registered);

    // Get the service
    let get_request = Request::builder()
        .uri("/services/test-sidecar")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let service: ServiceResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(service.name, "test-sidecar");
    assert_eq!(service.service_type, "sql");
    assert_eq!(service.url, "http://localhost:9001");
    assert_eq!(service.description, Some("Test SQL sidecar".to_string()));
    assert!(service.healthy);
}

#[tokio::test]
async fn test_services_list_with_filter() {
    let app = create_test_app().await;

    // Register a SQL service
    let register_sql = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "sql-sidecar",
            "service_type": "sql",
            "url": "http://localhost:9001"
        }"#,
        ))
        .unwrap();
    app.clone().oneshot(register_sql).await.unwrap();

    // Register a KV service
    let register_kv = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "kv-sidecar",
            "service_type": "kv",
            "url": "http://localhost:9002"
        }"#,
        ))
        .unwrap();
    app.clone().oneshot(register_kv).await.unwrap();

    // List all services
    let list_all = Request::builder()
        .uri("/services")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(list_all).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.services.len(), 2);

    // Filter by SQL type
    let list_sql = Request::builder()
        .uri("/services?service_type=sql")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(list_sql).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.services.len(), 1);
    assert_eq!(list.services[0].service_type, "sql");

    // Filter by KV type
    let list_kv = Request::builder()
        .uri("/services?service_type=kv")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(list_kv).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: ListServicesResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.services.len(), 1);
    assert_eq!(list.services[0].service_type, "kv");
}

#[tokio::test]
async fn test_services_get_not_found() {
    let app = create_test_app().await;

    let request = Request::builder()
        .uri("/services/nonexistent")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_services_delete() {
    let app = create_test_app().await;

    // Register a service
    let register_request = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "to-delete",
            "service_type": "storage",
            "url": "http://localhost:9003"
        }"#,
        ))
        .unwrap();
    app.clone().oneshot(register_request).await.unwrap();

    // Verify it exists
    let get_request = Request::builder()
        .uri("/services/to-delete")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Delete the service
    let delete_request = Request::builder()
        .method(Method::DELETE)
        .uri("/services/to-delete")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(delete_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let delete_resp: DeleteServiceResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(delete_resp.name, "to-delete");
    assert!(delete_resp.deleted);

    // Verify it's gone
    let get_request = Request::builder()
        .uri("/services/to-delete")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(get_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_services_delete_nonexistent() {
    let app = create_test_app().await;

    let request = Request::builder()
        .method(Method::DELETE)
        .uri("/services/nonexistent")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let delete_resp: DeleteServiceResponse = serde_json::from_slice(&body).unwrap();
    assert!(!delete_resp.deleted);
}

#[tokio::test]
async fn test_services_heartbeat() {
    let app = create_test_app().await;

    // Register a service
    let register_request = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "heartbeat-test",
            "service_type": "storage",
            "url": "http://localhost:9004"
        }"#,
        ))
        .unwrap();
    app.clone().oneshot(register_request).await.unwrap();

    // Send heartbeat
    let heartbeat_request = Request::builder()
        .method(Method::POST)
        .uri("/services/heartbeat-test/heartbeat")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(heartbeat_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let heartbeat_resp: HeartbeatResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(heartbeat_resp.name, "heartbeat-test");
    assert!(heartbeat_resp.updated);
}

#[tokio::test]
async fn test_services_heartbeat_not_found() {
    let app = create_test_app().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/services/nonexistent/heartbeat")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_services_register_invalid_url() {
    let app = create_test_app().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "bad-url",
            "service_type": "sql",
            "url": "not-a-valid-url"
        }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_services_custom_type() {
    let app = create_test_app().await;

    // Register with custom type
    let register_request = Request::builder()
        .method(Method::POST)
        .uri("/services")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
            "name": "custom-sidecar",
            "service_type": "custom:metrics",
            "url": "http://localhost:9005"
        }"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(register_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the type
    let get_request = Request::builder()
        .uri("/services/custom-sidecar")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(get_request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let service: ServiceResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(service.service_type, "custom:metrics");
}

// =========================================================================
// API Key Authentication Tests
// =========================================================================

/// Create a test app with the API key auth middleware layer.
async fn create_test_app_with_auth() -> Router {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().to_path_buf();
    let data_dir: &'static std::path::Path = Box::leak(Box::new(data_dir));

    let store = StateStore::open(data_dir.join("state.redb")).unwrap();
    let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
    let sql = SqlService::open(data_dir.join("sql.db")).unwrap();
    let storage = StorageService::open(data_dir.join("storage")).unwrap();
    let cron = CronScheduler::new().await.unwrap();

    let app_state = Arc::new(RwLock::new(AppState {
        store,
        kv,
        sql,
        storage,
        cron,
    }));

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_endpoint))
        .route("/kv/{key}", get(kv_get))
        .with_state(app_state)
        .layer(middleware::from_fn(api_key_auth_middleware))
}

#[tokio::test]
async fn test_auth_health_endpoint_always_accessible() {
    // Health endpoint should always be accessible, even with auth enabled
    let app = create_test_app_with_auth().await;

    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    // Should be OK regardless of API key configuration
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_auth_metrics_endpoint_always_accessible() {
    // Metrics endpoint should always be accessible for Prometheus scraping
    let app = create_test_app_with_auth().await;

    let request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    // Should be OK regardless of API key configuration
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_auth_static_api_key_loading() {
    // Verify the API_KEY static loads correctly (None when env not set)
    // This test documents the expected behavior
    let key = API_KEY.as_ref();

    // In test environment, MIK_API_KEY is typically not set
    // This validates the LazyLock initializes without panic
    if key.is_some() {
        // If set, ensure it's not empty (our filter)
        assert!(!key.unwrap().is_empty());
    }
}
