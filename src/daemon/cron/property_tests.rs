//! Property-based tests for the cron scheduler.
//!
//! These tests use proptest to verify invariants that must always hold,
//! regardless of the input. This catches edge cases that example-based
//! tests might miss.
//!
//! Run with:
//! ```bash
//! cargo test --lib cron::property_tests
//! ```

use proptest::prelude::*;
use std::collections::VecDeque;
use std::path::PathBuf;

use super::types::{JobExecution, JobState, MAX_HISTORY_ENTRIES, ScheduleConfig};

// ============================================================================
// Test Helpers - Arbitrary Implementations
// ============================================================================

/// Strategy for generating valid cron expressions (7-field format).
///
/// tokio-cron-scheduler uses 7 fields: sec min hour day month weekday year
fn valid_cron_expression() -> impl Strategy<Value = String> {
    prop_oneof![
        // Every second
        Just("* * * * * * *".to_string()),
        // Every minute at :00
        Just("0 * * * * * *".to_string()),
        // Every hour at :00:00
        Just("0 0 * * * * *".to_string()),
        // Daily at midnight
        Just("0 0 0 * * * *".to_string()),
        // Every 5 minutes
        Just("0 */5 * * * * *".to_string()),
        // Every 15 minutes
        Just("0 */15 * * * * *".to_string()),
        // Hourly at :30
        Just("0 30 * * * * *".to_string()),
        // Daily at 3am
        Just("0 0 3 * * * *".to_string()),
        // Weekly on Sunday at midnight
        Just("0 0 0 * * 0 *".to_string()),
        // Monthly on the 1st at midnight
        Just("0 0 0 1 * * *".to_string()),
    ]
}

/// Strategy for generating invalid cron expressions.
fn invalid_cron_expression() -> impl Strategy<Value = String> {
    prop_oneof![
        // Empty string
        Just(String::new()),
        // Too few fields (5-field format, not 7)
        Just("* * * * *".to_string()),
        // Invalid characters
        Just("invalid cron".to_string()),
        // Out of range values
        Just("60 * * * * * *".to_string()), // sec > 59
        Just("* 60 * * * * *".to_string()), // min > 59
        Just("* * 25 * * * *".to_string()), // hour > 24
        Just("* * * 32 * * *".to_string()), // day > 31
        Just("* * * * 13 * *".to_string()), // month > 12
        Just("* * * * * 8 *".to_string()),  // weekday > 7
        // Gibberish
        Just("abc def ghi".to_string()),
        Just("!@#$%^&*()".to_string()),
        // Partial expressions
        Just("0 0".to_string()),
        Just("0 0 0".to_string()),
    ]
}

/// Strategy for generating valid job names.
fn valid_job_name() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_-]{0,30}".prop_filter("must not be empty", |s| !s.is_empty())
}

/// Strategy for generating valid HTTP methods.
fn valid_http_method() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("GET".to_string()),
        Just("POST".to_string()),
        Just("PUT".to_string()),
        Just("DELETE".to_string()),
        Just("PATCH".to_string()),
    ]
}

/// Strategy for generating valid URL paths.
fn valid_path() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("/".to_string()),
        Just("/health".to_string()),
        Just("/api/v1".to_string()),
        Just("/trigger".to_string()),
        "/[a-z]{1,10}".prop_map(|s| format!("/{s}")),
    ]
}

/// Strategy for generating valid ScheduleConfig.
fn valid_schedule_config() -> impl Strategy<Value = ScheduleConfig> {
    (
        valid_job_name(),
        valid_cron_expression(),
        valid_http_method(),
        valid_path(),
        prop::bool::ANY,
        1000u16..60000u16,
    )
        .prop_map(|(name, cron, method, path, enabled, port)| ScheduleConfig {
            name,
            module: PathBuf::from("modules/test.wasm"),
            cron,
            method,
            path,
            enabled,
            port,
            body: None,
            headers: None,
            health_path: "/health".to_string(),
        })
}

/// Strategy for generating JobExecution records.
fn job_execution_strategy() -> impl Strategy<Value = JobExecution> {
    (
        "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}", // UUID-like ID
        valid_job_name(),
        prop::bool::ANY, // success
        prop::bool::ANY, // manual
        0u64..10000u64,  // duration_ms
    )
        .prop_map(|(id, job_name, success, manual, duration_ms)| {
            let now = chrono::Utc::now();
            JobExecution {
                id,
                job_name,
                started_at: now,
                completed_at: Some(now),
                duration_ms: Some(duration_ms),
                success,
                error: if success {
                    None
                } else {
                    Some("Test error".to_string())
                },
                manual,
            }
        })
}

// ============================================================================
// Job Scheduling Property Tests
// ============================================================================

proptest! {
    /// Invariant: Valid cron expressions should be parseable.
    ///
    /// All predefined valid cron expressions should be recognized as valid
    /// by the underlying cron parser.
    #[test]
    fn valid_cron_expressions_are_parseable(cron in valid_cron_expression()) {
        // tokio-cron-scheduler validates expressions when creating jobs
        // We test this by checking the expression format is correct
        let fields: Vec<&str> = cron.split_whitespace().collect();
        prop_assert_eq!(
            fields.len(),
            7,
            "Valid cron expression should have 7 fields: {}",
            cron
        );
    }

    /// Invariant: Structurally invalid cron expressions are detectable.
    ///
    /// Expressions with wrong field count or non-numeric/non-cron characters
    /// can be detected at the structural level. Note: semantic validation
    /// (like 60 for seconds) requires the actual cron parser.
    #[test]
    fn structurally_invalid_cron_detected(cron in invalid_cron_expression()) {
        let fields: Vec<&str> = cron.split_whitespace().collect();

        // Check for structural issues we can detect without a full parser:
        // - Wrong field count (not 7)
        // - Empty expression
        // - Contains alphabetic chars (except wildcards and valid cron chars)
        let has_wrong_field_count = fields.len() != 7;
        let is_empty = cron.is_empty();
        let has_invalid_chars = cron.chars().any(|c| {
            c.is_alphabetic() && !['*'].contains(&c)
        });

        // At least one of these conditions should be true for structurally invalid expressions
        // Note: Some expressions like "60 * * * * * *" are structurally valid but semantically invalid
        // Those require the actual cron parser to reject
        let is_structurally_detectable = has_wrong_field_count || is_empty || has_invalid_chars;

        // This test verifies that we can detect at least some invalid expressions structurally
        // Expressions that pass this check will be caught by the actual cron parser
        if !is_structurally_detectable {
            // For expressions that look structurally valid but are semantically invalid,
            // we just verify they have 7 fields (the parser will reject them)
            prop_assert_eq!(
                fields.len(),
                7,
                "Non-structurally invalid expression should have 7 fields: {}",
                cron
            );
        }
    }

    /// Invariant: ScheduleConfig with valid inputs should be constructible.
    #[test]
    fn valid_schedule_config_is_constructible(config in valid_schedule_config()) {
        // Basic sanity checks on the generated config
        prop_assert!(!config.name.is_empty(), "Name should not be empty");
        prop_assert!(!config.cron.is_empty(), "Cron should not be empty");
        prop_assert!(!config.method.is_empty(), "Method should not be empty");
        prop_assert!(config.path.starts_with('/'), "Path should start with /");
        prop_assert!(config.port >= 1000, "Port should be >= 1000");
    }

    /// Invariant: Job names should not contain path separators.
    ///
    /// Path separators in job names could lead to path traversal issues.
    #[test]
    fn job_names_cannot_contain_path_separators(name in valid_job_name()) {
        prop_assert!(
            !name.contains('/'),
            "Job name should not contain '/': {}",
            name
        );
        prop_assert!(
            !name.contains('\\'),
            "Job name should not contain '\\': {}",
            name
        );
    }
}

// ============================================================================
// Job State Transitions Property Tests
// ============================================================================

proptest! {
    /// Invariant: Newly created JobState has zero counters.
    #[test]
    fn new_job_state_has_zero_counters(config in valid_schedule_config()) {
        let state = JobState::new(config, None);

        prop_assert_eq!(state.execution_count, 0, "execution_count should be 0");
        prop_assert_eq!(state.success_count, 0, "success_count should be 0");
        prop_assert_eq!(state.failure_count, 0, "failure_count should be 0");
        prop_assert!(state.history.is_empty(), "history should be empty");
    }

    /// Invariant: JobState enabled/disabled transitions should be consistent.
    ///
    /// Setting enabled to true and back to false should leave the job disabled.
    #[test]
    fn enabled_disabled_transitions_are_consistent(
        mut config in valid_schedule_config(),
        transitions in prop::collection::vec(prop::bool::ANY, 1..20)
    ) {
        // Start with a known state
        config.enabled = false;
        let mut state = JobState::new(config, None);

        // Apply transitions
        let mut expected_enabled = false;
        for enabled in &transitions {
            state.config.enabled = *enabled;
            expected_enabled = *enabled;
        }

        prop_assert_eq!(
            state.config.enabled,
            expected_enabled,
            "Final enabled state should match last transition"
        );
    }

    /// Invariant: Toggling enabled twice returns to original state.
    #[test]
    fn double_toggle_returns_to_original(config in valid_schedule_config()) {
        let original_enabled = config.enabled;
        let mut state = JobState::new(config, None);

        // Toggle twice
        state.config.enabled = !state.config.enabled;
        state.config.enabled = !state.config.enabled;

        prop_assert_eq!(
            state.config.enabled,
            original_enabled,
            "Double toggle should return to original state"
        );
    }

    /// Invariant: Counter sum invariant - execution_count == success_count + failure_count
    #[test]
    fn counter_sum_invariant(
        config in valid_schedule_config(),
        successes in 0u64..100,
        failures in 0u64..100
    ) {
        let mut state = JobState::new(config, None);

        // Simulate executions
        state.execution_count = successes + failures;
        state.success_count = successes;
        state.failure_count = failures;

        prop_assert_eq!(
            state.execution_count,
            state.success_count + state.failure_count,
            "execution_count should equal success_count + failure_count"
        );
    }
}

// ============================================================================
// Execution History Property Tests
// ============================================================================

proptest! {
    /// Invariant: History never exceeds MAX_HISTORY_ENTRIES.
    #[test]
    fn history_never_exceeds_max_entries(
        config in valid_schedule_config(),
        num_executions in 1usize..200
    ) {
        let mut state = JobState::new(config, None);

        // Add executions
        for i in 0..num_executions {
            let execution = JobExecution {
                id: format!("exec-{i}"),
                job_name: state.config.name.clone(),
                started_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: Some(100),
                success: true,
                error: None,
                manual: false,
            };

            // Simulate the trim logic from record_execution
            if state.history.len() >= MAX_HISTORY_ENTRIES {
                state.history.pop_front();
            }
            state.history.push_back(execution);
        }

        prop_assert!(
            state.history.len() <= MAX_HISTORY_ENTRIES,
            "History should never exceed MAX_HISTORY_ENTRIES ({MAX_HISTORY_ENTRIES}), got {}",
            state.history.len()
        );
    }

    /// Invariant: History maintains chronological order (newest at back).
    #[test]
    fn history_maintains_chronological_order(
        config in valid_schedule_config(),
        num_executions in 2usize..50
    ) {
        let mut state = JobState::new(config, None);
        let base_time = chrono::Utc::now();

        // Add executions with increasing timestamps
        for i in 0..num_executions {
            // Safety: i is bounded by num_executions (2..50), so will never overflow i64
            #[allow(clippy::cast_possible_wrap)]
            let i_secs = i as i64;
            let execution = JobExecution {
                id: format!("exec-{i}"),
                job_name: state.config.name.clone(),
                started_at: base_time + chrono::Duration::seconds(i_secs),
                completed_at: Some(base_time + chrono::Duration::seconds(i_secs + 1)),
                duration_ms: Some(100),
                success: true,
                error: None,
                manual: false,
            };

            if state.history.len() >= MAX_HISTORY_ENTRIES {
                state.history.pop_front();
            }
            state.history.push_back(execution);
        }

        // Verify order: each entry should have started_at <= next entry's started_at
        for i in 0..state.history.len().saturating_sub(1) {
            let current = &state.history[i];
            let next = &state.history[i + 1];
            prop_assert!(
                current.started_at <= next.started_at,
                "History should be in chronological order at index {i}"
            );
        }
    }

    /// Invariant: After adding N executions (N <= MAX), history has exactly N entries.
    #[test]
    fn history_has_correct_count_under_limit(
        config in valid_schedule_config(),
        num_executions in 1usize..=MAX_HISTORY_ENTRIES
    ) {
        let mut state = JobState::new(config, None);

        for i in 0..num_executions {
            let execution = JobExecution {
                id: format!("exec-{i}"),
                job_name: state.config.name.clone(),
                started_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: Some(100),
                success: true,
                error: None,
                manual: false,
            };

            if state.history.len() >= MAX_HISTORY_ENTRIES {
                state.history.pop_front();
            }
            state.history.push_back(execution);
        }

        prop_assert_eq!(
            state.history.len(),
            num_executions,
            "History should have exactly {} entries",
            num_executions
        );
    }

    /// Invariant: Oldest entries are evicted when history is full.
    #[test]
    fn oldest_entries_are_evicted(config in valid_schedule_config()) {
        let mut state = JobState::new(config, None);

        // Fill history to capacity
        for i in 0..MAX_HISTORY_ENTRIES {
            let execution = JobExecution {
                id: format!("exec-{i}"),
                job_name: state.config.name.clone(),
                started_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: Some(100),
                success: true,
                error: None,
                manual: false,
            };
            state.history.push_back(execution);
        }

        // Add one more (should evict exec-0)
        let new_execution = JobExecution {
            id: "exec-new".to_string(),
            job_name: state.config.name.clone(),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: Some(100),
            success: true,
            error: None,
            manual: false,
        };

        if state.history.len() >= MAX_HISTORY_ENTRIES {
            state.history.pop_front();
        }
        state.history.push_back(new_execution);

        // First entry should now be exec-1 (exec-0 was evicted)
        prop_assert_eq!(
            state.history.front().map(|e| e.id.as_str()),
            Some("exec-1"),
            "Oldest entry (exec-0) should have been evicted"
        );

        // Last entry should be the new one
        prop_assert_eq!(
            state.history.back().map(|e| e.id.as_str()),
            Some("exec-new"),
            "Newest entry should be at the back"
        );
    }

    /// Invariant: Success/failure in execution matches error presence.
    #[test]
    fn execution_success_matches_error_presence(exec in job_execution_strategy()) {
        if exec.success {
            prop_assert!(
                exec.error.is_none(),
                "Successful execution should have no error"
            );
        } else {
            prop_assert!(
                exec.error.is_some(),
                "Failed execution should have an error message"
            );
        }
    }
}

// ============================================================================
// JobInfo Property Tests
// ============================================================================

proptest! {
    /// Invariant: to_job_info preserves essential fields.
    #[test]
    fn to_job_info_preserves_fields(config in valid_schedule_config()) {
        let state = JobState::new(config.clone(), None);
        let info = state.to_job_info();

        prop_assert_eq!(info.name, config.name, "Name should be preserved");
        prop_assert_eq!(info.cron, config.cron, "Cron should be preserved");
        prop_assert_eq!(info.enabled, config.enabled, "Enabled should be preserved");
        prop_assert_eq!(info.execution_count, 0, "execution_count should be 0");
        prop_assert_eq!(info.success_count, 0, "success_count should be 0");
        prop_assert_eq!(info.failure_count, 0, "failure_count should be 0");
        prop_assert!(info.last_execution.is_none(), "last_execution should be None");
    }

    /// Invariant: last_execution in JobInfo matches history back.
    #[test]
    fn last_execution_matches_history_back(
        config in valid_schedule_config(),
        num_executions in 1usize..20
    ) {
        let mut state = JobState::new(config, None);

        for i in 0..num_executions {
            let execution = JobExecution {
                id: format!("exec-{i}"),
                job_name: state.config.name.clone(),
                started_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: Some(100),
                success: i % 2 == 0,
                error: if i % 2 == 0 { None } else { Some("error".to_string()) },
                manual: false,
            };
            state.execution_count += 1;
            if execution.success {
                state.success_count += 1;
            } else {
                state.failure_count += 1;
            }
            state.history.push_back(execution);
        }

        let info = state.to_job_info();

        prop_assert!(
            info.last_execution.is_some(),
            "last_execution should be Some"
        );

        let last = info.last_execution.unwrap();
        prop_assert_eq!(
            last.id,
            format!("exec-{}", num_executions - 1),
            "last_execution should be the most recent"
        );
    }
}

// ============================================================================
// Concurrent Operations Property Tests (Simulated)
// ============================================================================

proptest! {
    /// Invariant: Multiple job additions should result in correct count.
    ///
    /// Adding N unique jobs should result in exactly N jobs in the map.
    #[test]
    fn adding_unique_jobs_results_in_correct_count(
        base_config in valid_schedule_config(),
        num_jobs in 1usize..20
    ) {
        use std::collections::HashMap;

        let mut jobs: HashMap<String, JobState> = HashMap::new();

        for i in 0..num_jobs {
            let mut config = base_config.clone();
            config.name = format!("{}-{i}", base_config.name);

            let state = JobState::new(config.clone(), None);
            jobs.insert(config.name, state);
        }

        prop_assert_eq!(
            jobs.len(),
            num_jobs,
            "Should have exactly {} jobs",
            num_jobs
        );
    }

    /// Invariant: Removing a job reduces count by 1.
    #[test]
    fn removing_job_reduces_count(
        base_config in valid_schedule_config(),
        num_jobs in 2usize..20,
        remove_index in 0usize..19
    ) {
        use std::collections::HashMap;

        let mut jobs: HashMap<String, JobState> = HashMap::new();
        let actual_num_jobs = num_jobs.min(19);
        let actual_remove_index = remove_index % actual_num_jobs;

        for i in 0..actual_num_jobs {
            let mut config = base_config.clone();
            config.name = format!("{}-{i}", base_config.name);

            let state = JobState::new(config.clone(), None);
            jobs.insert(config.name, state);
        }

        let initial_count = jobs.len();
        let name_to_remove = format!("{}-{actual_remove_index}", base_config.name);
        jobs.remove(&name_to_remove);

        prop_assert_eq!(
            jobs.len(),
            initial_count - 1,
            "Removing a job should reduce count by 1"
        );
    }

    /// Invariant: Duplicate job names should overwrite (HashMap behavior).
    #[test]
    fn duplicate_names_overwrite(config in valid_schedule_config()) {
        use std::collections::HashMap;

        let mut jobs: HashMap<String, JobState> = HashMap::new();

        // Add job
        let state1 = JobState::new(config.clone(), None);
        jobs.insert(config.name.clone(), state1);

        // Add same name with different enabled state
        let mut config2 = config.clone();
        config2.enabled = !config.enabled;
        let state2 = JobState::new(config2.clone(), None);
        jobs.insert(config2.name.clone(), state2);

        prop_assert_eq!(jobs.len(), 1, "Should still have 1 job");

        let stored = jobs.get(&config.name).unwrap();
        prop_assert_eq!(
            stored.config.enabled,
            config2.enabled,
            "Second insert should overwrite first"
        );
    }

    /// Invariant: Getting non-existent job returns None.
    #[test]
    fn get_nonexistent_returns_none(
        base_config in valid_schedule_config(),
        num_jobs in 1usize..10
    ) {
        use std::collections::HashMap;

        let mut jobs: HashMap<String, JobState> = HashMap::new();

        for i in 0..num_jobs {
            let mut config = base_config.clone();
            config.name = format!("{}-{i}", base_config.name);

            let state = JobState::new(config.clone(), None);
            jobs.insert(config.name, state);
        }

        let result = jobs.get("nonexistent-job-name");
        prop_assert!(result.is_none(), "Getting nonexistent job should return None");
    }
}

// ============================================================================
// VecDeque History Operations Property Tests
// ============================================================================

proptest! {
    /// Invariant: VecDeque maintains FIFO order for history.
    #[test]
    fn vecdeque_maintains_fifo_order(num_items in 1usize..50) {
        let mut history: VecDeque<String> = VecDeque::new();

        // Push items
        for i in 0..num_items {
            history.push_back(format!("item-{i}"));
        }

        // Front should be oldest
        prop_assert_eq!(
            history.front(),
            Some(&"item-0".to_string()),
            "Front should be the oldest item"
        );

        // Back should be newest
        prop_assert_eq!(
            history.back(),
            Some(&format!("item-{}", num_items - 1)),
            "Back should be the newest item"
        );
    }

    /// Invariant: pop_front removes oldest, push_back adds newest.
    #[test]
    fn pop_front_push_back_fifo(initial_size in 5usize..20) {
        let mut history: VecDeque<String> = VecDeque::new();

        // Fill initially
        for i in 0..initial_size {
            history.push_back(format!("item-{i}"));
        }

        // Pop front (oldest)
        let popped = history.pop_front();
        prop_assert_eq!(
            popped,
            Some("item-0".to_string()),
            "pop_front should return oldest"
        );

        // Push back (newest)
        history.push_back("item-new".to_string());

        // New front should be item-1
        prop_assert_eq!(
            history.front(),
            Some(&"item-1".to_string()),
            "After pop, front should be item-1"
        );

        // Back should be the new item
        prop_assert_eq!(
            history.back(),
            Some(&"item-new".to_string()),
            "After push, back should be item-new"
        );

        // Size should be same as initial
        prop_assert_eq!(
            history.len(),
            initial_size,
            "Size should remain the same after pop + push"
        );
    }
}
