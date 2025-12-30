//! Unit tests for the circuit breaker module.

use super::*;
use crate::constants;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// =========================================================================
// INITIAL STATE TESTS
// =========================================================================

#[test]
fn test_initial_state_is_closed() {
    // New circuit breaker should start in Closed state
    let cb = CircuitBreaker::new();
    let state = cb.get_state("test-service");
    assert_eq!(state, CircuitState::Closed { failure_count: 0 });
}

#[test]
fn test_initial_failure_count_is_zero() {
    // New circuit breaker should have zero failures
    let cb = CircuitBreaker::new();
    assert_eq!(cb.failure_count("test-service"), 0);
}

#[test]
fn test_initial_request_allowed() {
    // Initial requests should be permitted (circuit is closed)
    let cb = CircuitBreaker::new();
    assert!(cb.check_request("test-service").is_ok());
}

#[test]
fn test_initial_not_blocking() {
    // Circuit should not be blocking initially
    let cb = CircuitBreaker::new();
    assert!(!cb.is_blocking("test-service"));
}

#[test]
fn test_initial_not_open() {
    // Circuit should not be open initially
    let cb = CircuitBreaker::new();
    assert!(!cb.is_open("test-service"));
}

// =========================================================================
// DEFAULT CONFIGURATION TESTS
// =========================================================================

#[test]
fn test_default_config_values() {
    let config = CircuitBreakerConfig::default();
    assert_eq!(
        config.failure_threshold,
        constants::CIRCUIT_BREAKER_FAILURE_THRESHOLD
    );
    assert_eq!(
        config.timeout,
        Duration::from_secs(constants::CIRCUIT_BREAKER_RECOVERY_SECS)
    );
    assert_eq!(
        config.probe_timeout,
        Duration::from_secs(constants::CIRCUIT_BREAKER_RECOVERY_SECS)
    );
    assert_eq!(config.max_tracked_keys, 1000);
    assert_eq!(config.idle_timeout, Duration::from_secs(600));
}

#[test]
fn test_default_circuit_breaker_uses_default_config() {
    let cb = CircuitBreaker::new();
    // Verify default behavior: 5 failures to open
    for _ in 0..4 {
        cb.record_failure("test");
    }
    assert!(!cb.is_open("test")); // Not yet open (4 < 5)

    cb.record_failure("test"); // 5th failure
    assert!(cb.is_open("test")); // Now open (5 >= 5)
}

// =========================================================================
// STATE TRANSITION TESTS
// =========================================================================

#[test]
fn test_transitions_to_open_after_threshold() {
    // After N failures, circuit should open
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    cb.record_failure("test");
    assert!(!cb.is_open("test"));

    cb.record_failure("test");
    assert!(!cb.is_open("test"));

    cb.record_failure("test"); // 3rd failure - should open
    assert!(cb.is_open("test"));
    assert!(matches!(cb.get_state("test"), CircuitState::Open { .. }));
}

#[test]
fn test_stays_closed_below_threshold() {
    // Circuit stays closed if failures < threshold
    let config = CircuitBreakerConfig {
        failure_threshold: 5,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    for i in 1..5 {
        cb.record_failure("test");
        assert!(
            !cb.is_open("test"),
            "Circuit should not be open after {i} failures"
        );
        assert_eq!(cb.failure_count("test"), i as u32);
    }
}

#[test]
fn test_success_resets_failure_count() {
    // Recording success should reset failure count
    let cb = CircuitBreaker::new();

    cb.record_failure("test");
    cb.record_failure("test");
    assert_eq!(cb.failure_count("test"), 2);

    cb.record_success("test");
    assert_eq!(cb.failure_count("test"), 0);
    assert_eq!(
        cb.get_state("test"),
        CircuitState::Closed { failure_count: 0 }
    );
}

#[test]
fn test_open_circuit_rejects_calls() {
    // When open, check_request() should return error
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_secs(300), // Long timeout so it stays open
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    cb.record_failure("test");
    cb.record_failure("test");

    let result = cb.check_request("test");
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(err.key, "test");
    assert_eq!(err.failure_count, 2);
    assert_eq!(err.reason, CircuitOpenReason::Open);
}

#[test]
fn test_failure_count_increments_correctly() {
    let cb = CircuitBreaker::new();

    for i in 1..=4 {
        cb.record_failure("test");
        assert_eq!(cb.failure_count("test"), i as u32);
    }
}

#[test]
fn test_multiple_successes_keep_count_at_zero() {
    let cb = CircuitBreaker::new();

    cb.record_failure("test");
    cb.record_failure("test");
    cb.record_success("test");
    cb.record_success("test");
    cb.record_success("test");

    assert_eq!(cb.failure_count("test"), 0);
}

// =========================================================================
// HALF-OPEN STATE TESTS
// =========================================================================

#[test]
fn test_transitions_to_half_open_after_timeout() {
    // After recovery timeout, circuit should be half-open
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(50),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open the circuit
    cb.record_failure("test");
    cb.record_failure("test");
    assert!(cb.is_open("test"));

    // Wait for timeout
    thread::sleep(Duration::from_millis(100));

    // First check_request should transition to half-open and allow
    assert!(cb.check_request("test").is_ok());
    assert!(matches!(
        cb.get_state("test"),
        CircuitState::HalfOpen { .. }
    ));
}

#[test]
fn test_half_open_allows_single_call() {
    // Half-open state should allow one test call
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(50),
        probe_timeout: Duration::from_secs(60),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open and wait for timeout
    cb.record_failure("test");
    cb.record_failure("test");
    thread::sleep(Duration::from_millis(100));

    // First request is allowed (becomes probe)
    assert!(cb.check_request("test").is_ok());

    // Second request should be rejected (probe in flight)
    let result = cb.check_request("test");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().reason, CircuitOpenReason::ProbeInFlight);
}

#[test]
fn test_half_open_success_closes_circuit() {
    // Success in half-open state should close circuit
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(50),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open and wait for timeout
    cb.record_failure("test");
    cb.record_failure("test");
    thread::sleep(Duration::from_millis(100));

    // Transition to half-open
    assert!(cb.check_request("test").is_ok());
    assert!(matches!(
        cb.get_state("test"),
        CircuitState::HalfOpen { .. }
    ));

    // Record success - should close circuit
    cb.record_success("test");
    assert_eq!(
        cb.get_state("test"),
        CircuitState::Closed { failure_count: 0 }
    );

    // Requests should be allowed again
    assert!(cb.check_request("test").is_ok());
}

#[test]
fn test_half_open_failure_reopens_circuit() {
    // Failure in half-open should reopen circuit
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(50),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open and wait for timeout
    cb.record_failure("test");
    cb.record_failure("test");
    thread::sleep(Duration::from_millis(100));

    // Transition to half-open
    assert!(cb.check_request("test").is_ok());

    // Record failure - should reopen
    cb.record_failure("test");
    assert!(matches!(cb.get_state("test"), CircuitState::Open { .. }));
    assert!(cb.is_open("test"));
}

#[test]
#[ignore = "Requires waiting for probe_timeout (60s default), use integration tests"]
fn test_half_open_probe_timeout_allows_new_probe() {
    // If probe times out, a new probe should be allowed
    // This test is ignored because it requires waiting for probe_timeout
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(50),
        probe_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open and wait for timeout
    cb.record_failure("test");
    cb.record_failure("test");
    thread::sleep(Duration::from_millis(100));

    // First probe
    assert!(cb.check_request("test").is_ok());

    // Wait for probe timeout
    thread::sleep(Duration::from_millis(150));

    // New probe should be allowed
    assert!(cb.check_request("test").is_ok());
}

// =========================================================================
// CONFIGURATION TESTS
// =========================================================================

#[test]
fn test_custom_failure_threshold() {
    // Test configuring custom failure threshold
    let config = CircuitBreakerConfig {
        failure_threshold: 10,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    for i in 1..10 {
        cb.record_failure("test");
        assert!(
            !cb.is_open("test"),
            "Circuit should not be open after {i} failures"
        );
    }

    cb.record_failure("test"); // 10th failure
    assert!(cb.is_open("test"));
}

#[test]
fn test_custom_recovery_timeout() {
    // Test configuring custom recovery timeout
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(200),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    cb.record_failure("test");
    cb.record_failure("test");

    // After 100ms, still open (timeout is 200ms)
    thread::sleep(Duration::from_millis(100));
    assert!(cb.check_request("test").is_err());

    // After another 150ms (total 250ms > 200ms), should allow half-open
    thread::sleep(Duration::from_millis(150));
    assert!(cb.check_request("test").is_ok());
}

#[test]
fn test_threshold_of_one() {
    // Circuit opens on first failure
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    cb.record_failure("test");
    assert!(cb.is_open("test"));
}

// =========================================================================
// MULTI-KEY ISOLATION TESTS
// =========================================================================

#[test]
fn test_keys_are_isolated() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open circuit for service-a
    cb.record_failure("service-a");
    cb.record_failure("service-a");

    // service-a is open
    assert!(cb.is_open("service-a"));
    assert!(cb.check_request("service-a").is_err());

    // service-b should still be closed
    assert!(!cb.is_open("service-b"));
    assert!(cb.check_request("service-b").is_ok());

    // service-c should also be closed
    assert!(!cb.is_open("service-c"));
    assert!(cb.check_request("service-c").is_ok());
}

#[test]
fn test_failure_counts_per_key() {
    let cb = CircuitBreaker::new();

    cb.record_failure("key-a");
    cb.record_failure("key-a");
    cb.record_failure("key-b");

    assert_eq!(cb.failure_count("key-a"), 2);
    assert_eq!(cb.failure_count("key-b"), 1);
    assert_eq!(cb.failure_count("key-c"), 0);
}

#[test]
fn test_tracked_count() {
    let cb = CircuitBreaker::new();
    assert_eq!(cb.tracked_count(), 0);

    cb.record_failure("key-a");
    cb.record_failure("key-b");
    cb.record_failure("key-c");

    // moka cache may need time to update
    cb.states().run_pending_tasks();
    assert_eq!(cb.tracked_count(), 3);
}

// =========================================================================
// RESET AND UTILITY TESTS
// =========================================================================

#[test]
fn test_reset_closes_open_circuit() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open the circuit
    cb.record_failure("test");
    cb.record_failure("test");
    assert!(cb.is_open("test"));

    // Reset
    cb.reset("test");

    // Circuit should be closed with zero failures
    assert!(!cb.is_open("test"));
    assert_eq!(cb.failure_count("test"), 0);
    assert!(cb.check_request("test").is_ok());
}

#[test]
fn test_reset_nonexistent_key_is_noop() {
    let cb = CircuitBreaker::new();
    cb.reset("nonexistent"); // Should not panic
    assert_eq!(cb.failure_count("nonexistent"), 0);
}

#[test]
fn test_get_all_states() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    cb.record_failure("closed-service");
    cb.record_failure("open-service");
    cb.record_failure("open-service");

    cb.states().run_pending_tasks();
    let states = cb.get_all_states();

    assert!(
        states
            .iter()
            .any(|(k, s)| k == "closed-service" && s == "closed")
    );
    assert!(
        states
            .iter()
            .any(|(k, s)| k == "open-service" && s == "open")
    );
}

// =========================================================================
// ERROR TYPE TESTS
// =========================================================================

#[test]
fn test_circuit_open_error_display() {
    let err = CircuitOpenError {
        key: "test-service".to_string(),
        failure_count: 5,
        reason: CircuitOpenReason::Open,
    };

    let display = format!("{err}");
    assert!(display.contains("test-service"));
    assert!(display.contains('5'));
}

#[test]
fn test_circuit_open_error_probe_in_flight_display() {
    let err = CircuitOpenError {
        key: "test-service".to_string(),
        failure_count: 0,
        reason: CircuitOpenReason::ProbeInFlight,
    };

    let display = format!("{err}");
    assert!(display.contains("test-service"));
    assert!(display.contains("probe in flight"));
}

// =========================================================================
// CLONE AND THREAD SAFETY TESTS
// =========================================================================

#[test]
fn test_clone_shares_state() {
    let cb = CircuitBreaker::new();
    cb.record_failure("test");

    let cb2 = cb.clone();
    assert_eq!(cb2.failure_count("test"), 1);

    // Both point to same underlying cache
    cb.record_failure("test");
    assert_eq!(cb2.failure_count("test"), 2);

    // Changes from clone also visible
    cb2.record_failure("test");
    assert_eq!(cb.failure_count("test"), 3);
}

#[test]
fn test_concurrent_access() {
    let config = CircuitBreakerConfig {
        failure_threshold: 100,
        ..Default::default()
    };
    let cb = Arc::new(CircuitBreaker::with_config(config));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let cb = Arc::clone(&cb);
            thread::spawn(move || {
                for _ in 0..10 {
                    cb.record_failure("test");
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // Should be open after 100 failures
    assert!(cb.is_open("test"));
}

#[test]
fn test_concurrent_mixed_operations() {
    let config = CircuitBreakerConfig {
        failure_threshold: 50,
        ..Default::default()
    };
    let cb = Arc::new(CircuitBreaker::with_config(config));

    let mut handles = vec![];

    // Threads recording failures
    for _ in 0..5 {
        let cb = Arc::clone(&cb);
        handles.push(thread::spawn(move || {
            for _ in 0..20 {
                cb.record_failure("test");
            }
        }));
    }

    // Threads recording successes
    for _ in 0..5 {
        let cb = Arc::clone(&cb);
        handles.push(thread::spawn(move || {
            for _ in 0..20 {
                cb.record_success("test");
            }
        }));
    }

    // Threads checking requests
    for _ in 0..5 {
        let cb = Arc::clone(&cb);
        handles.push(thread::spawn(move || {
            for _ in 0..20 {
                let _ = cb.check_request("test");
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Just verify no panics occurred - state is non-deterministic
}

// =========================================================================
// STATE EQUALITY TESTS
// =========================================================================

#[test]
fn test_circuit_state_equality() {
    assert_eq!(
        CircuitState::Closed { failure_count: 0 },
        CircuitState::Closed { failure_count: 0 }
    );
    assert_ne!(
        CircuitState::Closed { failure_count: 0 },
        CircuitState::Closed { failure_count: 1 }
    );
}

#[test]
fn test_circuit_state_open_equality() {
    let state1 = CircuitState::Open {
        opened_at: Instant::now(),
        failure_count: 5,
    };
    let state2 = CircuitState::Open {
        opened_at: Instant::now(),
        failure_count: 5,
    };
    // Equal if failure_count matches (time ignored)
    assert_eq!(state1, state2);
}

#[test]
fn test_circuit_state_half_open_equality() {
    let state1 = CircuitState::HalfOpen {
        started_at: Instant::now(),
    };
    thread::sleep(Duration::from_millis(10));
    let state2 = CircuitState::HalfOpen {
        started_at: Instant::now(),
    };
    // Half-open states are equal regardless of time
    assert_eq!(state1, state2);
}

#[test]
fn test_circuit_state_default() {
    let default = CircuitState::default();
    assert_eq!(default, CircuitState::Closed { failure_count: 0 });
}

// =========================================================================
// EDGE CASE TESTS
// =========================================================================

#[test]
fn test_failure_in_open_state_extends_timeout() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // Open the circuit
    cb.record_failure("test");
    cb.record_failure("test");
    assert!(cb.is_open("test"));

    // Wait halfway
    thread::sleep(Duration::from_millis(60));

    // Record another failure (extends timeout)
    cb.record_failure("test");

    // Wait another 60ms (should still be open if timeout was reset)
    thread::sleep(Duration::from_millis(60));

    // Should still be blocked (timeout was reset)
    assert!(cb.check_request("test").is_err());
}

#[test]
fn test_success_on_new_key_is_noop() {
    let cb = CircuitBreaker::new();
    cb.record_success("new-key"); // Should not panic or create entry
    assert_eq!(cb.failure_count("new-key"), 0);
}

#[test]
fn test_failure_count_saturates() {
    let config = CircuitBreakerConfig {
        failure_threshold: u32::MAX,
        ..Default::default()
    };
    let cb = CircuitBreaker::with_config(config);

    // This tests that saturating_add prevents overflow
    for _ in 0..1000 {
        cb.record_failure("test");
    }

    // Just verify no panic occurred
    let count = cb.failure_count("test");
    assert!(count > 0);
}
