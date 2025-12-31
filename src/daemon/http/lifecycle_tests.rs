//! Daemon startup and shutdown lifecycle tests.
//!
//! Tests for:
//! - HTTP server startup and health checks
//! - Graceful shutdown
//! - Service initialization (KV, SQL, Storage, Queue)
//! - Cron scheduler lifecycle
//! - Error handling

use super::*;
use axum::body::Body;
use axum::http::{Method, Request};
use tower::ServiceExt;

// =========================================================================
// Daemon Startup Tests
// =========================================================================

/// Test that all services initialize correctly in AppState.
#[tokio::test]
async fn test_app_state_services_initialize() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    // Initialize all services - should not fail
    let store = StateStore::open(data_dir.join("state.redb")).unwrap();
    let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
    let sql = SqlService::open(data_dir.join("sql.db")).unwrap();
    let storage = StorageService::open(data_dir.join("storage")).unwrap();
    let queue = QueueService::new(&QueueConfig::default()).unwrap();
    let cron = CronScheduler::new().await.unwrap();

    // Verify services are functional
    // KV service
    kv.set("test", b"value").unwrap();
    assert_eq!(kv.get("test").unwrap(), Some(b"value".to_vec()));

    // SQL service is functional (just verify it was created)
    sql.execute_batch("CREATE TABLE IF NOT EXISTS test_lifecycle (id INTEGER PRIMARY KEY)")
        .unwrap();

    // Storage service
    storage.put_object("test.txt", b"data", None).unwrap();
    assert!(storage.get_object("test.txt").unwrap().is_some());

    // Queue service
    queue.push("test-queue", b"message").unwrap();
    assert_eq!(queue.len("test-queue").unwrap(), 1);

    // Cron scheduler
    assert!(cron.list_jobs().await.is_empty());

    // State store
    let instance = crate::daemon::state::Instance {
        name: "test".to_string(),
        port: 3000,
        pid: 12345,
        status: crate::daemon::state::Status::Running,
        config: std::path::PathBuf::from("/test/mik.toml"),
        started_at: chrono::Utc::now(),
        modules: vec![],
        auto_restart: false,
        restart_count: 0,
        last_restart_at: None,
    };
    store.save_instance(&instance).unwrap();
    assert!(store.get_instance("test").unwrap().is_some());
}

/// Helper to create a test router with all service routes.
async fn create_lifecycle_test_app() -> Router {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    // Leak the temp_dir so it lives for the duration of tests
    let data_dir: &'static std::path::Path = Box::leak(Box::new(data_dir));

    let store = StateStore::open(data_dir.join("state.redb")).unwrap();
    let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
    let sql = SqlService::open(data_dir.join("sql.db")).unwrap();
    let storage = StorageService::open(data_dir.join("storage")).unwrap();
    let queue = QueueService::new(&QueueConfig::default()).unwrap();
    let cron = CronScheduler::new().await.unwrap();

    let app_state = Arc::new(RwLock::new(AppState {
        store,
        kv,
        sql,
        storage,
        queue,
        cron,
    }));

    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/kv", get(kv_list))
        .route("/kv/{key}", get(kv_get))
        .route("/storage", get(storage_list))
        .route("/queues", get(queue_list))
        .route("/services", get(services_list))
        .with_state(app_state)
}

/// Test that HTTP server responds to health checks after startup.
#[tokio::test]
async fn test_http_server_health_check_responds() {
    let app = create_lifecycle_test_app().await;

    // Health endpoint should respond with 200
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

/// Test that all main routes are available after startup.
#[tokio::test]
async fn test_all_main_routes_available() {
    let app = create_lifecycle_test_app().await;

    // Test that main endpoints respond (not 404)
    let routes_to_check = vec![
        ("/health", Method::GET),
        ("/version", Method::GET),
        ("/kv", Method::GET),
        ("/storage", Method::GET),
        ("/queues", Method::GET),
        ("/services", Method::GET),
    ];

    for (path, method) in routes_to_check {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should not be 404 (route exists)
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "Route {method} {path} should exist"
        );
    }
}

// =========================================================================
// Service Initialization Tests
// =========================================================================

/// Test KV service initializes and persists data.
#[tokio::test]
async fn test_kv_service_initialization() {
    let temp_dir = tempfile::tempdir().unwrap();
    let kv_path = temp_dir.path().join("kv.redb");

    // Create and use KV store
    {
        let kv = KvStore::open(&kv_path).unwrap();
        kv.set("persistent-key", b"persistent-value").unwrap();
    }

    // Reopen and verify persistence
    {
        let kv = KvStore::open(&kv_path).unwrap();
        let value = kv.get("persistent-key").unwrap();
        assert_eq!(value, Some(b"persistent-value".to_vec()));
    }
}

/// Test SQL service initializes and can execute queries.
#[tokio::test]
async fn test_sql_service_initialization() {
    let temp_dir = tempfile::tempdir().unwrap();
    let sql_path = temp_dir.path().join("sql.db");

    let sql = SqlService::open(&sql_path).unwrap();

    // Should be able to create tables
    sql.execute(
        "CREATE TABLE test_init (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();

    // Should be able to insert and query
    sql.execute("INSERT INTO test_init (name) VALUES ('test')", &[])
        .unwrap();

    let rows = sql.query("SELECT name FROM test_init", &[]).unwrap();
    assert_eq!(rows.len(), 1);
}

/// Test Storage service initializes and handles files.
#[tokio::test]
async fn test_storage_service_initialization() {
    let temp_dir = tempfile::tempdir().unwrap();
    let storage_path = temp_dir.path().join("storage");

    let storage = StorageService::open(&storage_path).unwrap();

    // Should be able to store and retrieve objects
    storage
        .put_object("init-test.txt", b"initialization test", Some("text/plain"))
        .unwrap();

    let (data, meta) = storage.get_object("init-test.txt").unwrap().unwrap();
    assert_eq!(data, b"initialization test");
    assert_eq!(meta.content_type, "text/plain");
}

/// Test Queue service initializes correctly.
#[tokio::test]
async fn test_queue_service_initialization() {
    let queue = QueueService::new(&QueueConfig::default()).unwrap();

    // Should be able to push and pop messages
    let msg_id = queue.push("init-queue", b"init message").unwrap();
    assert!(!msg_id.is_empty());

    let msg = queue.pop("init-queue").unwrap().unwrap();
    assert_eq!(msg.data, b"init message");
}

// =========================================================================
// Cron Scheduler Lifecycle Tests
// =========================================================================

/// Test cron scheduler starts cleanly.
#[tokio::test]
async fn test_cron_scheduler_starts_cleanly() {
    let scheduler = CronScheduler::new().await.unwrap();

    // Should start without error
    scheduler.start().await.unwrap();

    // Should have no jobs initially
    assert!(scheduler.list_jobs().await.is_empty());
}

/// Test cron scheduler can add and remove jobs.
#[tokio::test]
async fn test_cron_scheduler_add_remove_jobs() {
    use crate::daemon::cron::ScheduleConfig;

    let scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    // Add a job
    let config = ScheduleConfig {
        name: "lifecycle-test".to_string(),
        module: std::path::PathBuf::from("modules/test.wasm"),
        cron: "0 0 * * * * *".to_string(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: true,
        port: 3000,
        body: None,
        headers: None,
        health_path: "/health".to_string(),
    };

    scheduler.add_job(config).await.unwrap();
    assert_eq!(scheduler.list_jobs().await.len(), 1);

    // Remove the job
    let removed = scheduler.remove_job("lifecycle-test").await.unwrap();
    assert!(removed);
    assert!(scheduler.list_jobs().await.is_empty());
}

/// Test cron scheduler stops cleanly.
#[tokio::test]
async fn test_cron_scheduler_stops_cleanly() {
    use crate::daemon::cron::ScheduleConfig;

    let mut scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    // Add a job before shutdown
    let config = ScheduleConfig {
        name: "shutdown-test".to_string(),
        module: std::path::PathBuf::from("modules/test.wasm"),
        cron: "0 0 * * * * *".to_string(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: true,
        port: 3000,
        body: None,
        headers: None,
        health_path: "/health".to_string(),
    };

    scheduler.add_job(config).await.unwrap();

    // Shutdown should not error
    scheduler.shutdown().await.unwrap();
}

/// Test cron scheduler pause and resume.
#[tokio::test]
async fn test_cron_scheduler_pause_resume() {
    use crate::daemon::cron::ScheduleConfig;

    let scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    let config = ScheduleConfig {
        name: "pause-test".to_string(),
        module: std::path::PathBuf::from("modules/test.wasm"),
        cron: "0 0 * * * * *".to_string(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: true,
        port: 3000,
        body: None,
        headers: None,
        health_path: "/health".to_string(),
    };

    scheduler.add_job(config).await.unwrap();

    // Verify initially enabled
    let job = scheduler.get_job("pause-test").await.unwrap();
    assert!(job.enabled);

    // Pause the job
    let result = scheduler.set_enabled("pause-test", false).await;
    assert_eq!(result, Some(false));

    let job = scheduler.get_job("pause-test").await.unwrap();
    assert!(!job.enabled);

    // Resume the job
    let result = scheduler.set_enabled("pause-test", true).await;
    assert_eq!(result, Some(true));

    let job = scheduler.get_job("pause-test").await.unwrap();
    assert!(job.enabled);
}

// =========================================================================
// Error Handling Tests
// =========================================================================

/// Test handling of invalid state database path.
#[test]
fn test_invalid_state_db_path_handling() {
    // Try to open in a non-existent deep path that can't be created
    // Note: On most systems, this should succeed because StateStore creates dirs
    // So we test with a truly invalid path

    #[cfg(windows)]
    {
        // On Windows, a path with invalid characters should fail
        let result = StateStore::open("C:\\<>?*|invalid\\state.redb");
        assert!(result.is_err());
    }

    #[cfg(unix)]
    {
        // On Unix, try a path in /proc which is read-only
        let result = StateStore::open("/proc/self/state.redb");
        assert!(result.is_err());
    }
}

/// Test handling of invalid cron expression.
#[tokio::test]
async fn test_invalid_cron_expression_handling() {
    use crate::daemon::cron::ScheduleConfig;

    let scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    let config = ScheduleConfig {
        name: "invalid-cron".to_string(),
        module: std::path::PathBuf::from("modules/test.wasm"),
        cron: "not a valid cron expression".to_string(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: true,
        port: 3000,
        body: None,
        headers: None,
        health_path: "/health".to_string(),
    };

    // Should fail with error
    let result = scheduler.add_job(config).await;
    assert!(result.is_err());
}

/// Test handling of duplicate job names.
#[tokio::test]
async fn test_duplicate_job_name_handling() {
    use crate::daemon::cron::ScheduleConfig;

    let scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    let config = ScheduleConfig {
        name: "duplicate-test".to_string(),
        module: std::path::PathBuf::from("modules/test.wasm"),
        cron: "0 0 * * * * *".to_string(),
        method: "GET".to_string(),
        path: "/".to_string(),
        enabled: true,
        port: 3000,
        body: None,
        headers: None,
        health_path: "/health".to_string(),
    };

    // First add should succeed
    scheduler.add_job(config.clone()).await.unwrap();

    // Second add with same name should succeed (overwrites)
    // This is the expected behavior based on HashMap semantics
    scheduler.add_job(config).await.unwrap();

    // Should only have one job
    assert_eq!(scheduler.list_jobs().await.len(), 1);
}

/// Test handling of nonexistent job operations.
#[tokio::test]
async fn test_nonexistent_job_operations() {
    let scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    // Remove nonexistent job should return false
    let removed = scheduler.remove_job("nonexistent").await.unwrap();
    assert!(!removed);

    // Get nonexistent job should return None
    let job = scheduler.get_job("nonexistent").await;
    assert!(job.is_none());

    // Set enabled on nonexistent job should return None
    let result = scheduler.set_enabled("nonexistent", true).await;
    assert!(result.is_none());

    // Trigger nonexistent job should error
    let result = scheduler.trigger_job("nonexistent").await;
    assert!(result.is_err());
}

// =========================================================================
// Concurrent Access Tests
// =========================================================================

/// Test concurrent access to services doesn't cause issues.
#[tokio::test]
async fn test_concurrent_service_access() {
    let temp_dir = tempfile::tempdir().unwrap();
    let kv = std::sync::Arc::new(KvStore::open(temp_dir.path().join("kv.redb")).unwrap());

    let mut handles = vec![];

    // Spawn multiple concurrent tasks
    for i in 0..10 {
        let kv = std::sync::Arc::clone(&kv);
        handles.push(tokio::spawn(async move {
            let key = format!("concurrent-key-{i}");
            let value = format!("value-{i}");
            kv.set(&key, value.as_bytes()).unwrap();
            let result = kv.get(&key).unwrap();
            assert_eq!(result, Some(value.into_bytes()));
        }));
    }

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all keys were written
    let keys = kv.list_keys(Some("concurrent-key-")).unwrap();
    assert_eq!(keys.len(), 10);
}

/// Test state store handles concurrent instance updates.
#[tokio::test]
async fn test_concurrent_state_store_access() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(StateStore::open(temp_dir.path().join("state.redb")).unwrap());

    let mut handles = vec![];

    // Spawn multiple concurrent tasks
    for i in 0..5 {
        let store = std::sync::Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            let instance = crate::daemon::state::Instance {
                name: format!("instance-{i}"),
                port: 3000 + i as u16,
                pid: 12345 + i as u32,
                status: crate::daemon::state::Status::Running,
                config: std::path::PathBuf::from("/test/mik.toml"),
                started_at: chrono::Utc::now(),
                modules: vec![],
                auto_restart: false,
                restart_count: 0,
                last_restart_at: None,
            };
            store.save_instance(&instance).unwrap();
        }));
    }

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all instances were saved
    let instances = store.list_instances().unwrap();
    assert_eq!(instances.len(), 5);
}

// =========================================================================
// Graceful Shutdown Tests
// =========================================================================

/// Test that instance status is properly updated during shutdown.
#[tokio::test]
async fn test_instance_status_update_on_shutdown() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = StateStore::open(temp_dir.path().join("state.redb")).unwrap();

    // Create a test instance
    let instance = crate::daemon::state::Instance {
        name: "shutdown-test".to_string(),
        port: 3000,
        pid: 99999, // Non-existent process
        status: crate::daemon::state::Status::Running,
        config: std::path::PathBuf::from("/test/mik.toml"),
        started_at: chrono::Utc::now(),
        modules: vec![],
        auto_restart: false,
        restart_count: 0,
        last_restart_at: None,
    };
    store.save_instance(&instance).unwrap();

    // Update status to stopped (simulating shutdown)
    let mut updated = instance.clone();
    updated.status = crate::daemon::state::Status::Stopped;
    store.save_instance(&updated).unwrap();

    // Verify status was updated
    let retrieved = store.get_instance("shutdown-test").unwrap().unwrap();
    assert_eq!(retrieved.status, crate::daemon::state::Status::Stopped);
}

/// Test that services can be cleanly reinitialized after shutdown.
#[tokio::test]
async fn test_services_reinitialize_after_shutdown() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().to_path_buf();

    // First initialization
    {
        let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
        kv.set("before-shutdown", b"value1").unwrap();

        let storage = StorageService::open(data_dir.join("storage")).unwrap();
        storage.put_object("test.txt", b"data", None).unwrap();
    }

    // Simulate shutdown and reinitialize
    {
        let kv = KvStore::open(data_dir.join("kv.redb")).unwrap();
        let storage = StorageService::open(data_dir.join("storage")).unwrap();

        // Data should persist
        assert_eq!(kv.get("before-shutdown").unwrap(), Some(b"value1".to_vec()));
        assert!(storage.get_object("test.txt").unwrap().is_some());

        // Should be able to add new data
        kv.set("after-shutdown", b"value2").unwrap();
        storage.put_object("test2.txt", b"data2", None).unwrap();
    }
}

/// Test cron scheduler properly cleans up on shutdown.
#[tokio::test]
async fn test_cron_scheduler_cleanup_on_shutdown() {
    use crate::daemon::cron::ScheduleConfig;

    let mut scheduler = CronScheduler::new().await.unwrap();
    scheduler.start().await.unwrap();

    // Add multiple jobs
    for i in 0..3 {
        let config = ScheduleConfig {
            name: format!("job-{i}"),
            module: std::path::PathBuf::from("modules/test.wasm"),
            cron: "0 0 * * * * *".to_string(),
            method: "GET".to_string(),
            path: "/".to_string(),
            enabled: true,
            port: 3000,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        };
        scheduler.add_job(config).await.unwrap();
    }

    assert_eq!(scheduler.list_jobs().await.len(), 3);

    // Shutdown should complete without error
    scheduler.shutdown().await.unwrap();
}

/// Test that queue service handles shutdown gracefully.
#[tokio::test]
async fn test_queue_service_shutdown_handling() {
    let queue = QueueService::new(&QueueConfig::default()).unwrap();

    // Add some messages
    for i in 0..5 {
        queue
            .push("shutdown-queue", format!("message-{i}").as_bytes())
            .unwrap();
    }

    // Verify messages are there
    assert_eq!(queue.len("shutdown-queue").unwrap(), 5);

    // Note: QueueService doesn't have an explicit shutdown method,
    // but it should drop cleanly
    drop(queue);

    // Create new queue service and verify it starts clean (in-memory queue)
    let queue2 = QueueService::new(&QueueConfig::default()).unwrap();
    assert_eq!(queue2.len("shutdown-queue").unwrap(), 0);
}

// =========================================================================
// State Persistence Tests
// =========================================================================

/// Test that all state persists across service restarts.
#[tokio::test]
async fn test_state_persistence_across_restarts() {
    let temp_dir = tempfile::tempdir().unwrap();
    let state_path = temp_dir.path().join("state.redb");

    // First session: create and save data
    {
        let store = StateStore::open(&state_path).unwrap();

        let instance = crate::daemon::state::Instance {
            name: "persistent-instance".to_string(),
            port: 3000,
            pid: 12345,
            status: crate::daemon::state::Status::Running,
            config: std::path::PathBuf::from("/test/mik.toml"),
            started_at: chrono::Utc::now(),
            modules: vec!["api.wasm".to_string()],
            auto_restart: true,
            restart_count: 2,
            last_restart_at: Some(chrono::Utc::now()),
        };
        store.save_instance(&instance).unwrap();
    }

    // Second session: verify data persisted
    {
        let store = StateStore::open(&state_path).unwrap();
        let instance = store.get_instance("persistent-instance").unwrap().unwrap();

        assert_eq!(instance.name, "persistent-instance");
        assert_eq!(instance.port, 3000);
        assert!(instance.auto_restart);
        assert_eq!(instance.restart_count, 2);
        assert!(instance.last_restart_at.is_some());
    }
}

/// Test cron job config persistence via state store.
#[tokio::test]
async fn test_cron_job_config_persistence() {
    use crate::daemon::cron::ScheduleConfig;

    let temp_dir = tempfile::tempdir().unwrap();
    let state_path = temp_dir.path().join("state.redb");

    let config = ScheduleConfig {
        name: "persistent-job".to_string(),
        module: std::path::PathBuf::from("modules/test.wasm"),
        cron: "0 0 * * * * *".to_string(),
        method: "POST".to_string(),
        path: "/trigger".to_string(),
        enabled: true,
        port: 3000,
        body: Some(serde_json::json!({"key": "value"})),
        headers: None,
        health_path: "/health".to_string(),
    };

    // First session: save config
    {
        let store = StateStore::open(&state_path).unwrap();
        store.save_cron_job(&config).unwrap();
    }

    // Second session: verify config persisted
    {
        let store = StateStore::open(&state_path).unwrap();
        let jobs = store.list_cron_jobs().unwrap();
        assert_eq!(jobs.len(), 1);

        let retrieved = &jobs[0];
        assert_eq!(retrieved.name, "persistent-job");
        assert_eq!(retrieved.cron, "0 0 * * * * *");
        assert_eq!(retrieved.method, "POST");
        assert_eq!(retrieved.path, "/trigger");
        assert_eq!(retrieved.body, Some(serde_json::json!({"key": "value"})));
    }
}
