//! Property-based tests for state persistence using proptest.
//!
//! These tests verify data integrity invariants for the state store:
//! - Instance state round-trip (save then load returns same data)
//! - Cron job persistence (save then load returns same config)
//! - Service registry consistency (register/unregister maintains consistency)
//! - Concurrent access safety (multiple readers/writers don't corrupt data)

use proptest::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tempfile::TempDir;

use chrono::{DateTime, TimeZone, Utc};

use super::{Instance, ServiceType, Sidecar, StateStore, Status};
use crate::daemon::cron::{JobExecution, ScheduleConfig};

// ============================================================================
// Arbitrary Implementations for Test Data Generation
// ============================================================================

/// Strategy for generating valid instance names (alphanumeric + hyphen/underscore)
fn instance_name_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_-]{0,30}".prop_filter("Name must not be empty", |s| !s.is_empty())
}

/// Strategy for generating valid port numbers (1024-65535 for non-privileged ports)
fn port_strategy() -> impl Strategy<Value = u16> {
    1024u16..=65535u16
}

/// Strategy for generating valid PIDs
fn pid_strategy() -> impl Strategy<Value = u32> {
    1u32..=u32::MAX
}

/// Strategy for generating instance status variants
fn status_strategy() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Running),
        Just(Status::Stopped),
        (-128i32..=127i32).prop_map(|code| Status::Crashed { exit_code: code }),
    ]
}

/// Strategy for generating valid module names
fn module_name_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_-]{0,20}\\.wasm"
}

/// Strategy for generating a list of module names
fn modules_strategy() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(module_name_strategy(), 0..5)
}

/// Strategy for generating valid file paths
fn path_strategy() -> impl Strategy<Value = PathBuf> {
    "[a-zA-Z0-9_/-]{1,50}".prop_map(PathBuf::from)
}

/// Strategy for generating timestamps (within reasonable bounds)
fn datetime_strategy() -> impl Strategy<Value = DateTime<Utc>> {
    // Generate timestamps from year 2020 to 2030
    (1_577_836_800_i64..1_893_456_000_i64).prop_map(|ts| Utc.timestamp_opt(ts, 0).unwrap())
}

/// Strategy for generating optional timestamps
fn optional_datetime_strategy() -> impl Strategy<Value = Option<DateTime<Utc>>> {
    prop::option::of(datetime_strategy())
}

/// Strategy for generating complete Instance objects
fn instance_strategy() -> impl Strategy<Value = Instance> {
    (
        instance_name_strategy(),
        port_strategy(),
        pid_strategy(),
        status_strategy(),
        path_strategy(),
        datetime_strategy(),
        modules_strategy(),
        any::<bool>(),
        0u32..1000u32,
        optional_datetime_strategy(),
    )
        .prop_map(
            |(
                name,
                port,
                pid,
                status,
                config,
                started_at,
                modules,
                auto_restart,
                restart_count,
                last_restart_at,
            )| {
                Instance {
                    name,
                    port,
                    pid,
                    status,
                    config,
                    started_at,
                    modules,
                    auto_restart,
                    restart_count,
                    last_restart_at,
                }
            },
        )
}

/// Strategy for generating ServiceType variants
fn service_type_strategy() -> impl Strategy<Value = ServiceType> {
    prop_oneof![
        Just(ServiceType::Kv),
        Just(ServiceType::Sql),
        Just(ServiceType::Storage),
        Just(ServiceType::Queue),
        "[a-z]{1,10}".prop_map(ServiceType::Custom),
    ]
}

/// Strategy for generating valid URLs
fn url_strategy() -> impl Strategy<Value = String> {
    (1024u16..65535u16).prop_map(|port| format!("http://localhost:{port}"))
}

/// Strategy for generating optional descriptions
fn optional_description_strategy() -> impl Strategy<Value = Option<String>> {
    prop::option::of("[a-zA-Z0-9 ]{0,50}")
}

/// Strategy for generating complete Sidecar objects
fn sidecar_strategy() -> impl Strategy<Value = Sidecar> {
    (
        instance_name_strategy(),
        service_type_strategy(),
        url_strategy(),
        optional_description_strategy(),
        datetime_strategy(),
        datetime_strategy(),
        any::<bool>(),
    )
        .prop_map(
            |(name, service_type, url, description, registered_at, last_heartbeat, healthy)| {
                Sidecar {
                    name,
                    service_type,
                    url,
                    description,
                    registered_at,
                    last_heartbeat,
                    healthy,
                }
            },
        )
}

/// Strategy for generating valid cron expressions
fn cron_expression_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("0 0 * * *".to_string()),   // Daily at midnight
        Just("*/5 * * * *".to_string()), // Every 5 minutes
        Just("0 */2 * * *".to_string()), // Every 2 hours
        Just("0 0 * * 0".to_string()),   // Weekly on Sunday
        Just("0 0 1 * *".to_string()),   // Monthly on 1st
        Just("30 4 * * *".to_string()),  // Daily at 4:30 AM
    ]
}

/// Strategy for generating HTTP methods
fn http_method_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("GET".to_string()),
        Just("POST".to_string()),
        Just("PUT".to_string()),
        Just("DELETE".to_string()),
    ]
}

/// Strategy for generating request paths
fn request_path_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("/".to_string()),
        Just("/health".to_string()),
        Just("/api/v1/trigger".to_string()),
        "[/a-z]{1,20}".prop_map(|s| format!("/{s}")),
    ]
}

/// Strategy for generating optional JSON body
fn optional_body_strategy() -> impl Strategy<Value = Option<serde_json::Value>> {
    prop::option::of(prop_oneof![
        Just(serde_json::json!({})),
        Just(serde_json::json!({"key": "value"})),
        Just(serde_json::json!({"action": "trigger", "force": true})),
    ])
}

/// Strategy for generating optional headers
fn optional_headers_strategy() -> impl Strategy<Value = Option<HashMap<String, String>>> {
    prop::option::of(prop_oneof![
        Just(HashMap::new()),
        Just(HashMap::from([(
            "Content-Type".to_string(),
            "application/json".to_string()
        )])),
        Just(HashMap::from([
            ("X-Custom-Header".to_string(), "custom-value".to_string()),
            ("Authorization".to_string(), "Bearer token".to_string()),
        ])),
    ])
}

/// Strategy for generating complete ScheduleConfig objects
fn schedule_config_strategy() -> impl Strategy<Value = ScheduleConfig> {
    (
        instance_name_strategy(),
        path_strategy(),
        cron_expression_strategy(),
        http_method_strategy(),
        request_path_strategy(),
        any::<bool>(),
        port_strategy(),
        optional_body_strategy(),
        optional_headers_strategy(),
        request_path_strategy(),
    )
        .prop_map(
            |(name, module, cron, method, path, enabled, port, body, headers, health_path)| {
                ScheduleConfig {
                    name,
                    module,
                    cron,
                    method,
                    path,
                    enabled,
                    port,
                    body,
                    headers,
                    health_path,
                }
            },
        )
}

/// Strategy for generating optional error messages
fn optional_error_strategy() -> impl Strategy<Value = Option<String>> {
    prop::option::of("[a-zA-Z0-9 :_-]{0,100}")
}

/// Strategy for generating complete JobExecution objects
fn job_execution_strategy() -> impl Strategy<Value = JobExecution> {
    (
        "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}", // UUID-like ID
        instance_name_strategy(),
        datetime_strategy(),
        optional_datetime_strategy(),
        prop::option::of(0u64..10000u64),
        any::<bool>(),
        optional_error_strategy(),
        any::<bool>(),
    )
        .prop_map(
            |(id, job_name, started_at, completed_at, duration_ms, success, error, manual)| {
                JobExecution {
                    id,
                    job_name,
                    started_at,
                    completed_at,
                    duration_ms,
                    success,
                    error,
                    manual,
                }
            },
        )
}

// ============================================================================
// Instance State Round-Trip Tests
// ============================================================================

proptest! {
    /// Invariant: Saving an instance then loading it returns identical data.
    ///
    /// This tests the serialization/deserialization round-trip for Instance objects.
    #[test]
    fn instance_roundtrip_preserves_data(instance in instance_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save the instance
        store.save_instance(&instance).unwrap();

        // Load the instance back
        let loaded = store.get_instance(&instance.name).unwrap().unwrap();

        // Verify all fields match
        prop_assert_eq!(loaded.name, instance.name);
        prop_assert_eq!(loaded.port, instance.port);
        prop_assert_eq!(loaded.pid, instance.pid);
        prop_assert_eq!(loaded.status, instance.status);
        prop_assert_eq!(loaded.config, instance.config);
        prop_assert_eq!(loaded.started_at, instance.started_at);
        prop_assert_eq!(loaded.modules, instance.modules);
        prop_assert_eq!(loaded.auto_restart, instance.auto_restart);
        prop_assert_eq!(loaded.restart_count, instance.restart_count);
        prop_assert_eq!(loaded.last_restart_at, instance.last_restart_at);
    }

    /// Invariant: Multiple instances can be saved and listed correctly.
    #[test]
    fn multiple_instances_persist_correctly(
        instances in prop::collection::vec(instance_strategy(), 1..10)
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Make names unique by appending index
        let instances: Vec<Instance> = instances
            .into_iter()
            .enumerate()
            .map(|(i, mut inst)| {
                inst.name = format!("{}-{}", inst.name, i);
                inst
            })
            .collect();

        // Save all instances
        for instance in &instances {
            store.save_instance(instance).unwrap();
        }

        // List all instances
        let listed = store.list_instances().unwrap();

        // Verify count matches
        prop_assert_eq!(listed.len(), instances.len());

        // Verify each instance can be retrieved
        for instance in &instances {
            let loaded = store.get_instance(&instance.name).unwrap();
            prop_assert!(loaded.is_some(), "Instance {} should exist", instance.name);
        }
    }

    /// Invariant: Updating an instance preserves the new data.
    #[test]
    fn instance_update_preserves_changes(
        initial in instance_strategy(),
        new_status in status_strategy(),
        new_port in port_strategy()
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save initial instance
        store.save_instance(&initial).unwrap();

        // Update the instance
        let mut updated = initial.clone();
        updated.status = new_status.clone();
        updated.port = new_port;
        store.save_instance(&updated).unwrap();

        // Load and verify updates
        let loaded = store.get_instance(&initial.name).unwrap().unwrap();
        prop_assert_eq!(loaded.status, new_status);
        prop_assert_eq!(loaded.port, new_port);
    }

    /// Invariant: Removing an instance makes it unavailable.
    #[test]
    fn instance_removal_is_complete(instance in instance_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save the instance
        store.save_instance(&instance).unwrap();
        prop_assert!(store.get_instance(&instance.name).unwrap().is_some());

        // Remove the instance
        let removed = store.remove_instance(&instance.name).unwrap();
        prop_assert!(removed, "Instance should have been removed");

        // Verify it's gone
        let loaded = store.get_instance(&instance.name).unwrap();
        prop_assert!(loaded.is_none(), "Instance should not exist after removal");
    }

    /// Invariant: Data persists across database reopens.
    #[test]
    fn instance_persists_across_reopens(instance in instance_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");

        // Save instance and close
        {
            let store = StateStore::open(&db_path).unwrap();
            store.save_instance(&instance).unwrap();
        }

        // Reopen and verify
        {
            let store = StateStore::open(&db_path).unwrap();
            let loaded = store.get_instance(&instance.name).unwrap().unwrap();
            prop_assert_eq!(loaded.name, instance.name);
            prop_assert_eq!(loaded.port, instance.port);
            prop_assert_eq!(loaded.status, instance.status);
        }
    }
}

// ============================================================================
// Cron Job Persistence Tests
// ============================================================================

proptest! {
    /// Invariant: Saving a cron job then loading it returns identical config.
    #[test]
    fn cron_job_roundtrip_preserves_data(config in schedule_config_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save the cron job
        store.save_cron_job(&config).unwrap();

        // Load the cron job back
        let loaded = store.get_cron_job(&config.name).unwrap().unwrap();

        // Verify all fields match
        prop_assert_eq!(loaded.name, config.name);
        prop_assert_eq!(loaded.module, config.module);
        prop_assert_eq!(loaded.cron, config.cron);
        prop_assert_eq!(loaded.method, config.method);
        prop_assert_eq!(loaded.path, config.path);
        prop_assert_eq!(loaded.enabled, config.enabled);
        prop_assert_eq!(loaded.port, config.port);
        prop_assert_eq!(loaded.body, config.body);
        prop_assert_eq!(loaded.headers, config.headers);
        prop_assert_eq!(loaded.health_path, config.health_path);
    }

    /// Invariant: Multiple cron jobs can be listed correctly.
    #[test]
    fn multiple_cron_jobs_persist_correctly(
        configs in prop::collection::vec(schedule_config_strategy(), 1..10)
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Make names unique
        let configs: Vec<ScheduleConfig> = configs
            .into_iter()
            .enumerate()
            .map(|(i, mut cfg)| {
                cfg.name = format!("{}-{}", cfg.name, i);
                cfg
            })
            .collect();

        // Save all configs
        for config in &configs {
            store.save_cron_job(config).unwrap();
        }

        // List all jobs
        let listed = store.list_cron_jobs().unwrap();
        prop_assert_eq!(listed.len(), configs.len());
    }

    /// Invariant: Job execution history round-trip preserves data.
    #[test]
    fn job_execution_roundtrip_preserves_data(execution in job_execution_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save the execution
        store.save_job_execution(&execution).unwrap();

        // Load executions for this job
        let executions = store.list_job_executions(&execution.job_name, 100).unwrap();

        // Find our execution
        let loaded = executions.iter().find(|e| e.id == execution.id);
        prop_assert!(loaded.is_some(), "Execution should be found");

        let loaded = loaded.unwrap();
        prop_assert_eq!(&loaded.id, &execution.id);
        prop_assert_eq!(&loaded.job_name, &execution.job_name);
        prop_assert_eq!(loaded.success, execution.success);
        prop_assert_eq!(&loaded.error, &execution.error);
        prop_assert_eq!(loaded.manual, execution.manual);
    }

    /// Invariant: Cron job removal is complete.
    #[test]
    fn cron_job_removal_is_complete(config in schedule_config_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save the job
        store.save_cron_job(&config).unwrap();
        prop_assert!(store.get_cron_job(&config.name).unwrap().is_some());

        // Remove the job
        let removed = store.remove_cron_job(&config.name).unwrap();
        prop_assert!(removed);

        // Verify it's gone
        prop_assert!(store.get_cron_job(&config.name).unwrap().is_none());
    }
}

// ============================================================================
// Service Registry Tests
// ============================================================================

proptest! {
    /// Invariant: Saving a sidecar then loading it returns identical data.
    #[test]
    fn sidecar_roundtrip_preserves_data(sidecar in sidecar_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Save the sidecar
        store.save_sidecar(&sidecar).unwrap();

        // Load the sidecar back
        let loaded = store.get_sidecar(&sidecar.name).unwrap().unwrap();

        // Verify all fields match
        prop_assert_eq!(loaded.name, sidecar.name);
        prop_assert_eq!(loaded.service_type, sidecar.service_type);
        prop_assert_eq!(loaded.url, sidecar.url);
        prop_assert_eq!(loaded.description, sidecar.description);
        prop_assert_eq!(loaded.registered_at, sidecar.registered_at);
        prop_assert_eq!(loaded.last_heartbeat, sidecar.last_heartbeat);
        prop_assert_eq!(loaded.healthy, sidecar.healthy);
    }

    /// Invariant: Register/unregister maintains consistency.
    #[test]
    fn sidecar_register_unregister_consistency(
        sidecars in prop::collection::vec(sidecar_strategy(), 1..10)
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Make names unique
        let sidecars: Vec<Sidecar> = sidecars
            .into_iter()
            .enumerate()
            .map(|(i, mut s)| {
                s.name = format!("{}-{}", s.name, i);
                s
            })
            .collect();

        // Register all sidecars
        for sidecar in &sidecars {
            store.save_sidecar(sidecar).unwrap();
        }

        // Verify all are registered
        let listed = store.list_sidecars().unwrap();
        prop_assert_eq!(listed.len(), sidecars.len());

        // Unregister half
        let to_remove = sidecars.len() / 2;
        for sidecar in sidecars.iter().take(to_remove) {
            let removed = store.remove_sidecar(&sidecar.name).unwrap();
            prop_assert!(removed);
        }

        // Verify remaining count
        let remaining = store.list_sidecars().unwrap();
        prop_assert_eq!(remaining.len(), sidecars.len() - to_remove);

        // Verify removed ones are gone
        for sidecar in sidecars.iter().take(to_remove) {
            prop_assert!(store.get_sidecar(&sidecar.name).unwrap().is_none());
        }

        // Verify remaining ones still exist
        for sidecar in sidecars.iter().skip(to_remove) {
            prop_assert!(store.get_sidecar(&sidecar.name).unwrap().is_some());
        }
    }

    /// Invariant: List by type returns correct sidecars.
    #[test]
    fn sidecar_list_by_type_is_accurate(
        service_type in service_type_strategy()
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Create sidecars with mixed types
        let now = Utc::now();
        let kv_sidecar = Sidecar {
            name: "kv-service".to_string(),
            service_type: ServiceType::Kv,
            url: "http://localhost:9001".to_string(),
            description: None,
            registered_at: now,
            last_heartbeat: now,
            healthy: true,
        };
        let sql_sidecar = Sidecar {
            name: "sql-service".to_string(),
            service_type: ServiceType::Sql,
            url: "http://localhost:9002".to_string(),
            description: None,
            registered_at: now,
            last_heartbeat: now,
            healthy: true,
        };
        let custom_sidecar = Sidecar {
            name: "custom-service".to_string(),
            service_type: service_type.clone(),
            url: "http://localhost:9003".to_string(),
            description: None,
            registered_at: now,
            last_heartbeat: now,
            healthy: true,
        };

        store.save_sidecar(&kv_sidecar).unwrap();
        store.save_sidecar(&sql_sidecar).unwrap();
        store.save_sidecar(&custom_sidecar).unwrap();

        // List by the generated service type
        let by_type = store.list_sidecars_by_type(&service_type).unwrap();

        // All returned sidecars should have the correct type
        for sidecar in &by_type {
            prop_assert_eq!(&sidecar.service_type, &service_type);
        }

        // The custom sidecar should always be in the list
        prop_assert!(by_type.iter().any(|s| s.name == "custom-service"));
    }

    /// Invariant: Heartbeat update changes timestamp and health status.
    #[test]
    fn sidecar_heartbeat_update_works(
        sidecar in sidecar_strategy(),
        new_healthy in any::<bool>()
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Register the sidecar
        store.save_sidecar(&sidecar).unwrap();

        // Update heartbeat
        let updated = store.update_sidecar_heartbeat(&sidecar.name, new_healthy).unwrap();
        prop_assert!(updated);

        // Verify health status changed
        let loaded = store.get_sidecar(&sidecar.name).unwrap().unwrap();
        prop_assert_eq!(loaded.healthy, new_healthy);
    }
}

// ============================================================================
// Concurrent Access Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    /// Invariant: Multiple concurrent readers don't corrupt data.
    #[test]
    fn concurrent_readers_dont_corrupt(instance in instance_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = Arc::new(StateStore::open(&db_path).unwrap());

        // Save initial data
        store.save_instance(&instance).unwrap();

        // Spawn multiple reader threads
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                let name = instance.name.clone();
                thread::spawn(move || {
                    for _ in 0..100 {
                        let result = store.get_instance(&name).unwrap();
                        assert!(result.is_some());
                    }
                })
            })
            .collect();

        // Wait for all readers
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify data is still intact
        let loaded = store.get_instance(&instance.name).unwrap().unwrap();
        prop_assert_eq!(loaded.name, instance.name);
        prop_assert_eq!(loaded.port, instance.port);
    }

    /// Invariant: Multiple concurrent writers eventually converge to consistent state.
    #[test]
    fn concurrent_writers_maintain_consistency(base_name in instance_name_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = Arc::new(StateStore::open(&db_path).unwrap());

        // Spawn multiple writer threads, each writing different instances
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let store = Arc::clone(&store);
                let name_prefix = base_name.clone();
                thread::spawn(move || {
                    for i in 0..10 {
                        let instance = Instance {
                            name: format!("{name_prefix}-{thread_id}-{i}"),
                            port: 3000 + (thread_id as u16 * 10) + (i as u16),
                            pid: 1000 + thread_id + i,
                            status: Status::Running,
                            config: PathBuf::from("/test/config.toml"),
                            started_at: Utc::now(),
                            modules: vec!["test.wasm".to_string()],
                            auto_restart: false,
                            restart_count: 0,
                            last_restart_at: None,
                        };
                        store.save_instance(&instance).unwrap();
                    }
                })
            })
            .collect();

        // Wait for all writers
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all instances were written (4 threads * 10 instances each)
        let instances = store.list_instances().unwrap();
        prop_assert_eq!(instances.len(), 40);
    }

    /// Invariant: Mixed readers and writers don't deadlock or corrupt.
    #[test]
    fn mixed_readers_writers_safe(instance in instance_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = Arc::new(StateStore::open(&db_path).unwrap());

        // Save initial data
        store.save_instance(&instance).unwrap();

        // Spawn reader threads
        let reader_handles: Vec<_> = (0..2)
            .map(|_| {
                let store = Arc::clone(&store);
                let name = instance.name.clone();
                thread::spawn(move || {
                    for _ in 0..50 {
                        let _ = store.get_instance(&name);
                        let _ = store.list_instances();
                    }
                })
            })
            .collect();

        // Spawn writer threads that update the same instance
        let writer_handles: Vec<_> = (0..2)
            .map(|thread_id| {
                let store = Arc::clone(&store);
                let mut inst = instance.clone();
                thread::spawn(move || {
                    for i in 0..50 {
                        inst.restart_count = (thread_id * 100 + i) as u32;
                        let _ = store.save_instance(&inst);
                    }
                })
            })
            .collect();

        // Wait for all threads
        for handle in reader_handles {
            handle.join().unwrap();
        }
        for handle in writer_handles {
            handle.join().unwrap();
        }

        // Verify the instance still exists and is valid
        let loaded = store.get_instance(&instance.name).unwrap();
        prop_assert!(loaded.is_some());
    }

    /// Invariant: Concurrent register/unregister of sidecars maintains consistency.
    #[test]
    fn concurrent_sidecar_operations_safe(base_name in instance_name_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = Arc::new(StateStore::open(&db_path).unwrap());
        let now = Utc::now();

        // Spawn threads that register and unregister sidecars
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let store = Arc::clone(&store);
                let name_prefix = base_name.clone();
                thread::spawn(move || {
                    for i in 0..10 {
                        let sidecar = Sidecar {
                            name: format!("{name_prefix}-{thread_id}-{i}"),
                            service_type: ServiceType::Kv,
                            url: format!("http://localhost:{}", 9000 + thread_id * 10 + i),
                            description: None,
                            registered_at: now,
                            last_heartbeat: now,
                            healthy: true,
                        };
                        store.save_sidecar(&sidecar).unwrap();

                        // Every other one, also test removal
                        if i % 2 == 0 {
                            let _ = store.remove_sidecar(&sidecar.name);
                        }
                    }
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify list_sidecars works and returns valid data
        let sidecars = store.list_sidecars().unwrap();

        // All returned sidecars should have valid names
        for sidecar in &sidecars {
            prop_assert!(!sidecar.name.is_empty());
            prop_assert!(!sidecar.url.is_empty());
        }
    }
}

// ============================================================================
// Edge Cases and Boundary Tests
// ============================================================================

proptest! {
    /// Invariant: Empty database returns empty lists.
    #[test]
    fn empty_database_returns_empty_lists(_dummy in Just(())) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        prop_assert!(store.list_instances().unwrap().is_empty());
        prop_assert!(store.list_cron_jobs().unwrap().is_empty());
        prop_assert!(store.list_sidecars().unwrap().is_empty());
    }

    /// Invariant: Getting non-existent items returns None.
    #[test]
    fn nonexistent_items_return_none(name in instance_name_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        prop_assert!(store.get_instance(&name).unwrap().is_none());
        prop_assert!(store.get_cron_job(&name).unwrap().is_none());
        prop_assert!(store.get_sidecar(&name).unwrap().is_none());
    }

    /// Invariant: Removing non-existent items returns false.
    #[test]
    fn removing_nonexistent_items_returns_false(name in instance_name_strategy()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        prop_assert!(!store.remove_instance(&name).unwrap());
        prop_assert!(!store.remove_cron_job(&name).unwrap());
        prop_assert!(!store.remove_sidecar(&name).unwrap());
    }

    /// Invariant: Overwriting an item updates it completely.
    #[test]
    fn overwrite_updates_completely(
        instance1 in instance_strategy(),
        instance2 in instance_strategy()
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.redb");
        let store = StateStore::open(&db_path).unwrap();

        // Use same name for both
        let inst1 = instance1;
        let mut inst2 = instance2;
        inst2.name = inst1.name.clone();

        // Save first
        store.save_instance(&inst1).unwrap();

        // Overwrite with second
        store.save_instance(&inst2).unwrap();

        // Load should return second
        let loaded = store.get_instance(&inst1.name).unwrap().unwrap();
        prop_assert_eq!(loaded.port, inst2.port);
        prop_assert_eq!(loaded.pid, inst2.pid);
        prop_assert_eq!(loaded.status, inst2.status);

        // Should only be one instance
        let all = store.list_instances().unwrap();
        prop_assert_eq!(all.len(), 1);
    }
}
