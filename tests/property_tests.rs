// Test-specific lint suppressions
#![allow(clippy::needless_borrow)]

//! Property-based tests for security-critical functions.
//!
//! These tests use proptest to verify invariants that must always hold,
//! regardless of the input. This catches edge cases that example-based
//! tests might miss.
//!
//! Run with:
//! ```bash
//! cargo test --test property_tests
//! ```

use proptest::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

// ============================================================================
// Import from the library crate
// ============================================================================

use mik::reliability::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use mik::runtime::security::{
    ModuleNameError, PathTraversalError, sanitize_file_path, sanitize_module_name,
    validate_windows_path,
};

// ============================================================================
// Path Sanitization Property Tests
// ============================================================================

proptest! {
    /// Invariant: Sanitized paths never start with ".."
    ///
    /// A path starting with ".." would indicate an attempt to escape
    /// the base directory, which is a directory traversal attack.
    #[test]
    fn sanitized_paths_never_start_with_dotdot(path in ".*") {
        if let Ok(sanitized) = sanitize_file_path(&path) {
            let path_str = sanitized.to_string_lossy();
            prop_assert!(
                !path_str.starts_with(".."),
                "Sanitized path should never start with '..': {}",
                path_str
            );
        }
    }

    /// Invariant: Sanitized paths never contain "/../"
    ///
    /// The sequence "/../" in a path indicates a parent directory reference
    /// that could be used to escape the intended directory.
    #[test]
    fn sanitized_paths_never_contain_parent_dir_sequence(path in ".*") {
        if let Ok(sanitized) = sanitize_file_path(&path) {
            let path_str = sanitized.to_string_lossy();
            prop_assert!(
                !path_str.contains("/../"),
                "Sanitized path should never contain '/../': {}",
                path_str
            );
            // Backslash is only a path separator on Windows.
            // On Unix, backslash is a valid filename character.
            #[cfg(windows)]
            prop_assert!(
                !path_str.contains("\\..\\"),
                "Sanitized path should never contain '\\..\\': {}",
                path_str
            );
        }
    }

    /// Invariant: Sanitized paths never contain null bytes
    ///
    /// Null bytes can be used in path injection attacks because some
    /// systems treat them as string terminators.
    #[test]
    fn sanitized_paths_never_contain_null_bytes(path in ".*") {
        if let Ok(sanitized) = sanitize_file_path(&path) {
            let path_str = sanitized.to_string_lossy();
            prop_assert!(
                !path_str.contains('\0'),
                "Sanitized path should never contain null bytes: {:?}",
                path_str
            );
        }
    }

    /// Invariant: Paths with null bytes are always rejected
    ///
    /// Any path containing a null byte should be rejected outright.
    #[test]
    fn paths_with_null_bytes_are_rejected(
        prefix in "[a-zA-Z0-9/._-]{0,20}",
        suffix in "[a-zA-Z0-9/._-]{0,20}"
    ) {
        let path_with_null = format!("{}\0{}", prefix, suffix);
        let result = sanitize_file_path(&path_with_null);
        prop_assert!(
            result == Err(PathTraversalError::NullByte),
            "Path with null byte should be rejected: {:?} -> {:?}",
            path_with_null, result
        );
    }

    /// Invariant: Absolute paths are always rejected (Unix-style)
    ///
    /// Absolute paths could allow access to arbitrary filesystem locations.
    /// Note: Some paths like "//" are caught as UNC paths first, which is
    /// acceptable - the important thing is they are rejected.
    #[test]
    fn absolute_unix_paths_are_rejected(path in "/[a-zA-Z0-9._-]{1,50}") {
        let result = sanitize_file_path(&path);
        prop_assert!(
            matches!(
                result,
                Err(PathTraversalError::AbsolutePath) | Err(PathTraversalError::UncPath)
            ),
            "Absolute Unix path should be rejected: {} -> {:?}",
            path, result
        );
    }

    /// Invariant: Windows reserved names are rejected
    ///
    /// Windows device names like CON, PRN, NUL, COM1-9, LPT1-9 can cause
    /// security issues and unexpected behavior on Windows systems.
    #[test]
    fn windows_reserved_names_are_rejected(
        reserved in prop_oneof![
            Just("CON"),
            Just("PRN"),
            Just("AUX"),
            Just("NUL"),
            Just("COM1"),
            Just("COM2"),
            Just("COM3"),
            Just("COM4"),
            Just("COM5"),
            Just("COM6"),
            Just("COM7"),
            Just("COM8"),
            Just("COM9"),
            Just("LPT1"),
            Just("LPT2"),
            Just("LPT3"),
            Just("LPT4"),
            Just("LPT5"),
            Just("LPT6"),
            Just("LPT7"),
            Just("LPT8"),
            Just("LPT9"),
        ],
        extension in prop_oneof![
            Just(""),
            Just(".txt"),
            Just(".wasm"),
            Just(".anything"),
        ]
    ) {
        let path = format!("{}{}", reserved, extension);
        let result = validate_windows_path(&path);
        prop_assert!(
            result == Err(PathTraversalError::ReservedWindowsName),
            "Windows reserved name should be rejected: {} -> {:?}",
            path, result
        );
    }

    /// Invariant: Windows reserved names are case-insensitive
    ///
    /// CON, con, Con, cOn should all be rejected equally.
    #[test]
    fn windows_reserved_names_case_insensitive(
        reserved in prop_oneof![
            Just("con"),
            Just("CON"),
            Just("Con"),
            Just("cOn"),
            Just("nul"),
            Just("NUL"),
            Just("Nul"),
            Just("prn"),
            Just("PRN"),
            Just("com1"),
            Just("COM1"),
            Just("lpt1"),
            Just("LPT1"),
        ]
    ) {
        let result = validate_windows_path(&reserved);
        prop_assert!(
            result == Err(PathTraversalError::ReservedWindowsName),
            "Windows reserved name (any case) should be rejected: {} -> {:?}",
            reserved, result
        );
    }

    /// Invariant: UNC paths are rejected
    ///
    /// UNC paths (\\server\share or //server/share) could be used to
    /// access network resources unexpectedly.
    #[test]
    fn unc_paths_are_rejected(server in "[a-zA-Z0-9]{1,10}", share in "[a-zA-Z0-9]{1,10}") {
        let unc_backslash = format!("\\\\{}\\{}", server, share);
        let unc_forward = format!("//{}/{}", server, share);

        prop_assert!(
            validate_windows_path(&unc_backslash) == Err(PathTraversalError::UncPath),
            "UNC path with backslashes should be rejected: {}",
            unc_backslash
        );
        prop_assert!(
            validate_windows_path(&unc_forward) == Err(PathTraversalError::UncPath),
            "UNC path with forward slashes should be rejected: {}",
            unc_forward
        );
    }

    /// Invariant: Alternate data streams are rejected
    ///
    /// Windows NTFS alternate data streams (file.txt:stream) could be used
    /// to hide data or bypass security checks.
    #[test]
    fn alternate_data_streams_are_rejected(
        filename in "[a-zA-Z0-9]{1,10}",
        extension in "[a-zA-Z0-9]{1,5}",
        stream in "[a-zA-Z0-9]{1,10}"
    ) {
        // Format: file.ext:stream (colon not at position 1)
        let path = format!("{}.{}:{}", filename, extension, stream);
        let result = validate_windows_path(&path);
        prop_assert!(
            result == Err(PathTraversalError::AlternateDataStream),
            "Alternate data stream should be rejected: {} -> {:?}",
            path, result
        );
    }

    /// Invariant: Valid simple paths remain unchanged after sanitization
    ///
    /// A simple alphanumeric path with safe characters should pass through
    /// sanitization unchanged.
    #[test]
    fn valid_simple_paths_unchanged(
        name in "[a-zA-Z][a-zA-Z0-9_-]{0,20}",
        extension in prop_oneof![Just("wasm"), Just("js"), Just("html"), Just("css")]
    ) {
        let path = format!("{}.{}", name, extension);
        let result = sanitize_file_path(&path);
        prop_assert!(
            result.is_ok(),
            "Valid simple path should be accepted: {}",
            path
        );
        if let Ok(sanitized) = result {
            let expected = PathBuf::from(&path);
            prop_assert_eq!(
                sanitized, expected,
                "Valid simple path should be unchanged"
            );
        }
    }
}

// ============================================================================
// Module Name Sanitization Property Tests
// ============================================================================

proptest! {
    /// Invariant: Names with forward slashes are rejected
    ///
    /// Module names should be simple identifiers, not paths.
    #[test]
    fn module_names_with_forward_slash_rejected(
        prefix in "[a-zA-Z0-9_-]{1,10}",
        suffix in "[a-zA-Z0-9_-]{1,10}"
    ) {
        let name = format!("{}/{}", prefix, suffix);
        let result = sanitize_module_name(&name);
        prop_assert!(
            result == Err(ModuleNameError::PathSeparator),
            "Module name with '/' should be rejected: {} -> {:?}",
            name, result
        );
    }

    /// Invariant: Names with backslashes are rejected
    ///
    /// Module names should be simple identifiers, not paths.
    #[test]
    fn module_names_with_backslash_rejected(
        prefix in "[a-zA-Z0-9_-]{1,10}",
        suffix in "[a-zA-Z0-9_-]{1,10}"
    ) {
        let name = format!("{}\\{}", prefix, suffix);
        let result = sanitize_module_name(&name);
        prop_assert!(
            result == Err(ModuleNameError::PathSeparator),
            "Module name with '\\' should be rejected: {} -> {:?}",
            name, result
        );
    }

    /// Invariant: Empty names are rejected
    #[test]
    fn empty_module_name_rejected(_dummy in Just(())) {
        let result = sanitize_module_name("");
        prop_assert!(
            result == Err(ModuleNameError::EmptyName),
            "Empty module name should be rejected"
        );
    }

    /// Invariant: Names with null bytes are rejected
    #[test]
    fn module_names_with_null_bytes_rejected(
        prefix in "[a-zA-Z0-9_-]{1,10}",
        suffix in "[a-zA-Z0-9_-]{0,10}"
    ) {
        let name = format!("{}\0{}", prefix, suffix);
        let result = sanitize_module_name(&name);
        prop_assert!(
            result == Err(ModuleNameError::NullByte),
            "Module name with null byte should be rejected: {:?} -> {:?}",
            name, result
        );
    }

    /// Invariant: Valid module names remain unchanged
    ///
    /// Simple alphanumeric names with underscores/hyphens should pass through
    /// unchanged.
    #[test]
    fn valid_module_names_unchanged(name in "[a-zA-Z][a-zA-Z0-9_-]{0,50}") {
        // Skip edge cases that would be rejected
        prop_assume!(name != "." && name != "..");
        prop_assume!(name.len() <= 255);

        let result = sanitize_module_name(&name);
        prop_assert!(
            result.is_ok(),
            "Valid module name should be accepted: {}",
            name
        );
        if let Ok(sanitized) = result {
            prop_assert_eq!(
                sanitized, name,
                "Valid module name should be unchanged"
            );
        }
    }

    /// Invariant: Special directory names are rejected
    #[test]
    fn special_directory_names_rejected(name in prop_oneof![Just("."), Just("..")]) {
        let result = sanitize_module_name(&name);
        prop_assert!(
            result == Err(ModuleNameError::SpecialDirectory),
            "Special directory name should be rejected: {} -> {:?}",
            name, result
        );
    }

    /// Invariant: Names with control characters are rejected
    #[test]
    fn module_names_with_control_chars_rejected(
        prefix in "[a-zA-Z0-9]{1,5}",
        control in prop::char::range('\x00', '\x1F'),
        suffix in "[a-zA-Z0-9]{0,5}"
    ) {
        // Skip null byte (tested separately)
        prop_assume!(control != '\0');

        let name = format!("{}{}{}", prefix, control, suffix);
        let result = sanitize_module_name(&name);
        prop_assert!(
            result == Err(ModuleNameError::ControlCharacter),
            "Module name with control char should be rejected: {:?} -> {:?}",
            name, result
        );
    }

    /// Invariant: Names exceeding 255 characters are rejected
    #[test]
    fn long_module_names_rejected(extra_len in 1usize..100) {
        let name = "a".repeat(256 + extra_len);
        let result = sanitize_module_name(&name);
        prop_assert!(
            result == Err(ModuleNameError::TooLong),
            "Module name over 255 chars should be rejected: len={} -> {:?}",
            name.len(), result
        );
    }
}

// ============================================================================
// Circuit Breaker State Transition Property Tests
// ============================================================================

proptest! {
    /// Invariant: New circuits start in Closed state with zero failures
    #[test]
    fn new_circuits_start_closed(key in "[a-zA-Z0-9_-]{1,20}") {
        let cb = CircuitBreaker::new();
        let state = cb.get_state(&key);
        prop_assert!(
            state == CircuitState::Closed { failure_count: 0 },
            "New circuit should be Closed with 0 failures: {:?}",
            state
        );
    }

    /// Invariant: Failure count never exceeds threshold before opening
    ///
    /// The circuit should open when failure count reaches threshold,
    /// not after exceeding it.
    #[test]
    fn circuit_opens_at_threshold(
        threshold in 2u32..10,  // Start from 2 to avoid edge case with threshold=1
        key in "[a-zA-Z0-9_-]{1,20}"
    ) {
        let config = CircuitBreakerConfig {
            failure_threshold: threshold,
            timeout: Duration::from_secs(60), // Long timeout to not transition during test
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        // Record failures up to threshold - 1 (circuit should stay closed)
        for i in 0..(threshold - 1) {
            cb.record_failure(&key);
            let state = cb.get_state(&key);
            match state {
                CircuitState::Closed { failure_count } => {
                    prop_assert!(
                        failure_count == i + 1,
                        "Failure count should be {} but was {}",
                        i + 1, failure_count
                    );
                }
                CircuitState::Open { .. } => {
                    prop_assert!(
                        false,
                        "Circuit opened too early at iteration {} (threshold={})",
                        i, threshold
                    );
                }
                CircuitState::HalfOpen { .. } => {
                    prop_assert!(false, "Circuit should not be HalfOpen during failure recording");
                }
            }
        }

        // The threshold-th failure should open the circuit
        cb.record_failure(&key);
        let final_state = cb.get_state(&key);
        prop_assert!(
            matches!(final_state, CircuitState::Open { .. }),
            "Circuit should be Open after {} failures: {:?}",
            threshold, final_state
        );
    }

    /// Invariant: Success in Closed state resets failure count to zero
    #[test]
    fn success_resets_failure_count(
        failures_before in 1u32..5,
        key in "[a-zA-Z0-9_-]{1,20}"
    ) {
        let config = CircuitBreakerConfig {
            failure_threshold: 10, // High threshold so we don't open
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        // Record some failures
        for _ in 0..failures_before {
            cb.record_failure(&key);
        }

        // Verify we have failures recorded
        let state_before = cb.get_state(&key);
        prop_assert!(
            matches!(state_before, CircuitState::Closed { failure_count } if failure_count > 0),
            "Should have failures before success: {:?}",
            state_before
        );

        // Record success
        cb.record_success(&key);

        // Verify failure count is reset
        let state_after = cb.get_state(&key);
        prop_assert!(
            state_after == CircuitState::Closed { failure_count: 0 },
            "Failure count should be 0 after success: {:?}",
            state_after
        );
    }

    /// Invariant: Success in HalfOpen always closes the circuit
    ///
    /// When a probe request succeeds in HalfOpen state, the circuit
    /// must transition to Closed.
    #[test]
    fn halfopen_success_closes_circuit(key in "[a-zA-Z0-9_-]{1,20}") {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            timeout: Duration::from_millis(10), // Short timeout to quickly transition
            probe_timeout: Duration::from_secs(60),
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure(&key);
        cb.record_failure(&key);

        // Wait for timeout to allow HalfOpen transition
        std::thread::sleep(Duration::from_millis(20));

        // Trigger transition to HalfOpen
        let check_result = cb.check_request(&key);
        prop_assert!(
            check_result.is_ok(),
            "First request after timeout should be allowed"
        );

        // Verify we're in HalfOpen
        let state = cb.get_state(&key);
        prop_assert!(
            matches!(state, CircuitState::HalfOpen { .. }),
            "Circuit should be in HalfOpen: {:?}",
            state
        );

        // Record success
        cb.record_success(&key);

        // Verify circuit is now Closed
        let final_state = cb.get_state(&key);
        prop_assert!(
            final_state == CircuitState::Closed { failure_count: 0 },
            "Circuit should be Closed after HalfOpen success: {:?}",
            final_state
        );
    }

    /// Invariant: Failure in HalfOpen reopens the circuit
    ///
    /// When a probe request fails in HalfOpen state, the circuit
    /// must transition back to Open.
    #[test]
    fn halfopen_failure_reopens_circuit(key in "[a-zA-Z0-9_-]{1,20}") {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            timeout: Duration::from_millis(10),
            probe_timeout: Duration::from_secs(60),
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure(&key);
        cb.record_failure(&key);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(20));

        // Trigger transition to HalfOpen
        let _ = cb.check_request(&key);

        // Record failure in HalfOpen
        cb.record_failure(&key);

        // Verify circuit is now Open again
        let final_state = cb.get_state(&key);
        prop_assert!(
            matches!(final_state, CircuitState::Open { .. }),
            "Circuit should be Open after HalfOpen failure: {:?}",
            final_state
        );
    }

    /// Invariant: Multiple keys are independent
    ///
    /// Failures on one key should not affect another key's circuit state.
    #[test]
    fn circuit_keys_are_independent(
        key1 in "[a-z]{1,10}",
        key2 in "[A-Z]{1,10}"
    ) {
        // Ensure keys are different
        prop_assume!(key1 != key2.to_lowercase());

        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        // Open circuit for key1
        cb.record_failure(&key1);
        cb.record_failure(&key1);

        // key1 should be open
        prop_assert!(
            matches!(cb.get_state(&key1), CircuitState::Open { .. }),
            "key1 circuit should be open"
        );

        // key2 should still be closed
        prop_assert!(
            cb.get_state(&key2) == CircuitState::Closed { failure_count: 0 },
            "key2 circuit should be closed"
        );
    }

    /// Invariant: HalfOpen only allows one probe at a time
    ///
    /// When in HalfOpen state, only the first request should be allowed.
    /// Subsequent requests should be rejected until the probe completes.
    #[test]
    fn halfopen_blocks_concurrent_requests(key in "[a-zA-Z0-9_-]{1,20}") {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            timeout: Duration::from_millis(10),
            probe_timeout: Duration::from_secs(60), // Long probe timeout
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure(&key);
        cb.record_failure(&key);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(20));

        // First request should be allowed (becomes the probe)
        let first = cb.check_request(&key);
        prop_assert!(first.is_ok(), "First request should be allowed");

        // Subsequent requests should be blocked
        let second = cb.check_request(&key);
        prop_assert!(second.is_err(), "Second request should be blocked");

        let third = cb.check_request(&key);
        prop_assert!(third.is_err(), "Third request should be blocked");
    }
}

// ============================================================================
// Additional Edge Case Tests
// ============================================================================

proptest! {
    /// Invariant: Path traversal with encoded characters is handled
    ///
    /// Even if ".." is somehow embedded in a path, it should be handled safely.
    /// Note: Backslash variants are only path separators on Windows.
    /// On Unix, backslashes are literal filename characters.
    #[test]
    fn path_traversal_variants_rejected(
        prefix in "[a-zA-Z0-9]{0,5}",
        suffix in "[a-zA-Z0-9]{0,5}"
    ) {
        // Forward slash variants work on all platforms
        let mut test_cases = vec![
            format!("{}/../{}", prefix, suffix),
            format!("../{}", suffix),
            format!("{}/../../{}", prefix, suffix),
        ];

        // Backslash variants are only path separators on Windows
        #[cfg(windows)]
        {
            test_cases.push(format!("{}\\..\\{}", prefix, suffix));
            test_cases.push(format!("..\\{}", suffix));
        }

        for path in &test_cases {
            let result = sanitize_file_path(path);
            // Should either reject or sanitize safely
            if let Ok(sanitized) = &result {
                let sanitized_str = sanitized.to_string_lossy();
                prop_assert!(
                    !sanitized_str.starts_with(".."),
                    "Path {} should not sanitize to start with '..': {}",
                    path, sanitized_str
                );
                prop_assert!(
                    !sanitized_str.contains("/../"),
                    "Path {} should not contain '/../' after sanitization: {}",
                    path, sanitized_str
                );
            }
        }
    }

    /// Invariant: Empty path is rejected
    #[test]
    fn empty_path_rejected(_dummy in Just(())) {
        let result = sanitize_file_path("");
        prop_assert!(
            result == Err(PathTraversalError::EmptyPath),
            "Empty path should be rejected"
        );
    }

    /// Invariant: Circuit breaker clone shares state
    ///
    /// Cloning a circuit breaker should share the underlying state.
    #[test]
    fn circuit_breaker_clone_shares_state(key in "[a-zA-Z0-9_-]{1,20}") {
        let cb1 = CircuitBreaker::new();
        let cb2 = cb1.clone();

        // Record failure on cb1
        cb1.record_failure(&key);

        // Should be visible on cb2
        let cb2_count = cb1.failure_count(&key);
        prop_assert!(
            cb2_count == 1,
            "Clone should share state, expected failure_count=1, got {}",
            cb2_count
        );

        // Record failure on cb2
        cb2.record_failure(&key);

        // Should be visible on cb1
        let cb1_count = cb1.failure_count(&key);
        prop_assert!(
            cb1_count == 2,
            "Clone should share state, expected failure_count=2, got {}",
            cb1_count
        );
    }
}

// ============================================================================
// Instance Name Validation Property Tests
// ============================================================================

/// Validates an instance name (mirrors daemon/http.rs logic)
fn validate_instance_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Instance name cannot be empty".into());
    }
    if name.len() > 64 {
        return Err("Instance name must be 64 characters or less".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Instance name can only contain alphanumeric characters, hyphens, and underscores"
                .into(),
        );
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err("Instance name cannot start with a hyphen or underscore".into());
    }
    Ok(())
}

proptest! {
    /// Invariant: Valid instance names are accepted
    #[test]
    fn valid_instance_names_accepted(
        name in "[a-zA-Z][a-zA-Z0-9_-]{0,63}"
    ) {
        let result = validate_instance_name(&name);
        prop_assert!(
            result.is_ok(),
            "Valid instance name should be accepted: {} -> {:?}",
            name, result
        );
    }

    /// Invariant: Empty names are rejected
    #[test]
    fn empty_instance_names_rejected(_dummy in 0..1i32) {
        let result = validate_instance_name("");
        prop_assert!(
            result.is_err(),
            "Empty instance name should be rejected"
        );
    }

    /// Invariant: Names over 64 chars are rejected
    #[test]
    fn long_instance_names_rejected(extra in 1usize..100) {
        let name = "a".repeat(65 + extra);
        let result = validate_instance_name(&name);
        prop_assert!(
            result.is_err(),
            "Instance name over 64 chars should be rejected: len={}",
            name.len()
        );
    }

    /// Invariant: Names with special characters are rejected
    #[test]
    fn instance_names_with_special_chars_rejected(
        prefix in "[a-zA-Z]{1,5}",
        special in prop::sample::select(vec!['/', '\\', '.', ':', '*', '?', '"', '<', '>', '|', ' ', '\t', '\n']),
        suffix in "[a-zA-Z0-9]{0,5}"
    ) {
        let name = format!("{}{}{}", prefix, special, suffix);
        let result = validate_instance_name(&name);
        prop_assert!(
            result.is_err(),
            "Instance name with special char '{}' should be rejected: {}",
            special, name
        );
    }

    /// Invariant: Names starting with hyphen/underscore are rejected
    #[test]
    fn instance_names_starting_with_special_rejected(
        prefix in prop::sample::select(vec!['-', '_']),
        suffix in "[a-zA-Z0-9]{1,10}"
    ) {
        let name = format!("{}{}", prefix, suffix);
        let result = validate_instance_name(&name);
        prop_assert!(
            result.is_err(),
            "Instance name starting with '{}' should be rejected: {}",
            prefix, name
        );
    }
}

// ============================================================================
// KV Key Validation Property Tests
// ============================================================================

/// Validates a KV key (reasonable constraints for keys)
fn validate_kv_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("Key cannot be empty".into());
    }
    if key.len() > 1024 {
        return Err("Key must be 1024 characters or less".into());
    }
    if key.contains('\0') {
        return Err("Key cannot contain null bytes".into());
    }
    Ok(())
}

proptest! {
    /// Invariant: Valid KV keys are accepted
    #[test]
    fn valid_kv_keys_accepted(key in "[a-zA-Z0-9:/_.-]{1,100}") {
        let result = validate_kv_key(&key);
        prop_assert!(
            result.is_ok(),
            "Valid KV key should be accepted: {} -> {:?}",
            key, result
        );
    }

    /// Invariant: Empty keys are rejected
    #[test]
    fn empty_kv_keys_rejected(_dummy in 0..1i32) {
        let result = validate_kv_key("");
        prop_assert!(
            result.is_err(),
            "Empty KV key should be rejected"
        );
    }

    /// Invariant: Keys with null bytes are rejected
    #[test]
    fn kv_keys_with_null_rejected(
        prefix in "[a-zA-Z]{1,10}",
        suffix in "[a-zA-Z]{0,10}"
    ) {
        let key = format!("{}\0{}", prefix, suffix);
        let result = validate_kv_key(&key);
        prop_assert!(
            result.is_err(),
            "KV key with null byte should be rejected"
        );
    }

    /// Invariant: Very long keys are rejected
    #[test]
    fn long_kv_keys_rejected(extra in 1usize..100) {
        let key = "k".repeat(1025 + extra);
        let result = validate_kv_key(&key);
        prop_assert!(
            result.is_err(),
            "KV key over 1024 chars should be rejected: len={}",
            key.len()
        );
    }
}
