//! Shared utility functions.

use std::fs;

/// Get project name from Cargo.toml in the current directory.
///
/// Returns `None` if Cargo.toml doesn't exist or can't be parsed.
pub fn get_cargo_name() -> Option<String> {
    let content = fs::read_to_string("Cargo.toml").ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("package")?
        .get("name")?
        .as_str()
        .map(std::string::ToString::to_string)
}

/// Get project version from Cargo.toml in the current directory.
///
/// Returns `None` if Cargo.toml doesn't exist or can't be parsed.
#[allow(dead_code)]
pub fn get_cargo_version() -> Option<String> {
    let content = fs::read_to_string("Cargo.toml").ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("package")?
        .get("version")?
        .as_str()
        .map(std::string::ToString::to_string)
}

/// Format bytes in human-readable form.
///
/// # Examples
///
/// ```
/// use mik::utils::format_bytes;
///
/// assert_eq!(format_bytes(0), "0 bytes");
/// assert_eq!(format_bytes(1024), "1.0 KB");
/// assert_eq!(format_bytes(1536), "1.5 KB");
/// assert_eq!(format_bytes(1048576), "1.0 MB");
/// ```
#[allow(clippy::cast_precision_loss)]
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    const TB: u64 = 1024 * 1024 * 1024 * 1024;

    if bytes == 0 {
        "0 bytes".to_string()
    } else if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

/// Format a duration in human-readable form.
///
/// # Examples
///
/// ```
/// use chrono::Duration;
/// use mik::utils::format_duration;
///
/// assert_eq!(format_duration(Duration::seconds(30)), "30s");
/// assert_eq!(format_duration(Duration::seconds(90)), "1m 30s");
/// assert_eq!(format_duration(Duration::seconds(3660)), "1h 1m");
/// assert_eq!(format_duration(Duration::seconds(90000)), "1d 1h");
/// ```
pub fn format_duration(duration: chrono::Duration) -> String {
    let secs = duration.num_seconds();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(chrono::Duration::seconds(30)), "30s");
        assert_eq!(format_duration(chrono::Duration::seconds(90)), "1m 30s");
        assert_eq!(format_duration(chrono::Duration::seconds(3660)), "1h 1m");
        assert_eq!(format_duration(chrono::Duration::seconds(90000)), "1d 1h");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 1024), "1.00 TB");
    }
}

#[cfg(test)]
mod property_tests {
    //! Property-based tests for utility functions.
    //!
    //! These tests verify that utility functions:
    //! - Never panic on any valid input
    //! - Produce sensible output for all inputs
    //! - Are consistent and predictable

    use proptest::prelude::*;

    use super::{format_bytes, format_duration};

    // ============================================================================
    // Test Strategies - Input Generation
    // ============================================================================

    /// Strategy for generating arbitrary byte counts.
    fn byte_count() -> impl Strategy<Value = u64> {
        any::<u64>()
    }

    /// Strategy for generating interesting byte values at boundaries.
    fn boundary_bytes() -> impl Strategy<Value = u64> {
        prop_oneof![
            // Zero and small values
            Just(0u64),
            Just(1u64),
            Just(512u64),
            Just(1023u64),
            // KB boundary
            Just(1024u64),
            Just(1025u64),
            Just(1536u64), // 1.5 KB
            // MB boundary
            Just(1024u64 * 1024),
            Just(1024u64 * 1024 + 1),
            Just(1024u64 * 1024 * 1024 / 2), // 512 MB
            // GB boundary
            Just(1024u64 * 1024 * 1024),
            Just(1024u64 * 1024 * 1024 + 1),
            // TB boundary
            Just(1024u64 * 1024 * 1024 * 1024),
            Just(1024u64 * 1024 * 1024 * 1024 + 1),
            // Large values
            Just(u64::MAX / 2),
            Just(u64::MAX - 1),
            Just(u64::MAX),
        ]
    }

    /// Strategy for generating duration in seconds.
    ///
    /// `chrono::Duration::seconds()` has a limit of approximately 2^53 nanoseconds
    /// which is around 10 million years. We use a safer upper bound.
    fn seconds() -> impl Strategy<Value = i64> {
        // chrono::Duration has a max of about 292 billion years in nanoseconds
        // but seconds() can overflow, so we limit to a safe range
        0i64..=315_360_000_000i64 // ~10,000 years in seconds
    }

    /// Strategy for generating interesting duration values.
    fn boundary_seconds() -> impl Strategy<Value = i64> {
        prop_oneof![
            // Small values
            Just(0i64),
            Just(1i64),
            Just(30i64),
            Just(59i64),
            // Minute boundary
            Just(60i64),
            Just(61i64),
            Just(90i64), // 1m 30s
            // Hour boundary
            Just(3600i64),
            Just(3601i64),
            Just(3660i64), // 1h 1m
            // Day boundary
            Just(86400i64),
            Just(86401i64),
            Just(90000i64), // 1d 1h
            // Large values
            Just(86400i64 * 365),      // 1 year
            Just(86400i64 * 365 * 10), // 10 years
        ]
    }

    // ============================================================================
    // format_bytes Invariants
    // ============================================================================

    proptest! {
        /// Invariant: format_bytes never panics on any u64 input.
        #[test]
        fn format_bytes_never_panics(bytes in byte_count()) {
            // This should not panic
            let _result = format_bytes(bytes);
        }

        /// Invariant: format_bytes always returns a non-empty string.
        #[test]
        fn format_bytes_returns_nonempty(bytes in byte_count()) {
            let result = format_bytes(bytes);
            prop_assert!(!result.is_empty(), "format_bytes should return non-empty string");
        }

        /// Invariant: format_bytes output contains a unit.
        ///
        /// Output should always contain one of: "bytes", "KB", "MB", "GB", "TB"
        #[test]
        fn format_bytes_contains_unit(bytes in byte_count()) {
            let result = format_bytes(bytes);
            let has_unit = result.contains("bytes")
                || result.contains("KB")
                || result.contains("MB")
                || result.contains("GB")
                || result.contains("TB");

            prop_assert!(has_unit, "format_bytes should contain a unit: {}", result);
        }

        /// Invariant: Zero bytes formats as "0 bytes".
        #[test]
        fn format_bytes_zero(_dummy in Just(())) {
            let result = format_bytes(0);
            prop_assert_eq!(result, "0 bytes");
        }

        /// Invariant: Values under 1KB show as "X bytes".
        #[test]
        fn format_bytes_under_kb(bytes in 1u64..1024) {
            let result = format_bytes(bytes);
            prop_assert!(
                result.ends_with(" bytes"),
                "Values under 1KB should be in bytes: {} -> {}",
                bytes, result
            );
            prop_assert!(
                !result.contains("KB"),
                "Values under 1KB should not use KB"
            );
        }

        /// Invariant: Values at KB level use KB unit.
        #[test]
        fn format_bytes_kb_range(kb in 1u64..1024) {
            let bytes = kb * 1024;
            let result = format_bytes(bytes);
            prop_assert!(
                result.contains("KB"),
                "Values in KB range should use KB: {} -> {}",
                bytes, result
            );
        }

        /// Invariant: Values at MB level use MB unit.
        #[test]
        fn format_bytes_mb_range(mb in 1u64..1024) {
            let bytes = mb * 1024 * 1024;
            let result = format_bytes(bytes);
            prop_assert!(
                result.contains("MB"),
                "Values in MB range should use MB: {} -> {}",
                bytes, result
            );
        }

        /// Invariant: Values at GB level use GB unit.
        #[test]
        fn format_bytes_gb_range(gb in 1u64..1024) {
            let bytes = gb * 1024 * 1024 * 1024;
            let result = format_bytes(bytes);
            prop_assert!(
                result.contains("GB"),
                "Values in GB range should use GB: {} -> {}",
                bytes, result
            );
        }

        /// Invariant: Values at TB level use TB unit.
        #[test]
        fn format_bytes_tb_range(tb in 1u64..1000) {
            let bytes = tb * 1024 * 1024 * 1024 * 1024;
            let result = format_bytes(bytes);
            prop_assert!(
                result.contains("TB"),
                "Values in TB range should use TB: {} -> {}",
                bytes, result
            );
        }

        /// Invariant: Boundary values are handled correctly.
        #[test]
        fn format_bytes_boundaries(bytes in boundary_bytes()) {
            let result = format_bytes(bytes);
            prop_assert!(!result.is_empty());
            // Should always have a space between number and unit
            prop_assert!(
                result.contains(' '),
                "Output should have space between number and unit: {}",
                result
            );
        }
    }

    // ============================================================================
    // format_duration Invariants
    // ============================================================================

    proptest! {
        /// Invariant: format_duration never panics on valid durations.
        #[test]
        fn format_duration_never_panics(secs in seconds()) {
            let duration = chrono::Duration::seconds(secs);
            // This should not panic
            let _result = format_duration(duration);
        }

        /// Invariant: format_duration always returns a non-empty string.
        #[test]
        fn format_duration_returns_nonempty(secs in seconds()) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            prop_assert!(!result.is_empty(), "format_duration should return non-empty string");
        }

        /// Invariant: format_duration output contains a time unit.
        ///
        /// Output should always contain one of: "s", "m", "h", "d"
        #[test]
        fn format_duration_contains_unit(secs in 0i64..86400 * 365) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            let has_unit = result.contains('s')
                || result.contains('m')
                || result.contains('h')
                || result.contains('d');

            prop_assert!(has_unit, "format_duration should contain a unit: {}", result);
        }

        /// Invariant: Zero duration formats as "0s".
        #[test]
        fn format_duration_zero(_dummy in Just(())) {
            let result = format_duration(chrono::Duration::seconds(0));
            prop_assert_eq!(result, "0s");
        }

        /// Invariant: Values under 60s show only seconds.
        #[test]
        fn format_duration_under_minute(secs in 1i64..60) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            prop_assert!(
                result.ends_with('s'),
                "Under 60s should show seconds: {} -> {}",
                secs, result
            );
            prop_assert!(
                !result.contains('m'),
                "Under 60s should not show minutes: {} -> {}",
                secs, result
            );
        }

        /// Invariant: Values 60s-3599s show minutes and seconds.
        #[test]
        fn format_duration_minute_range(secs in 60i64..3600) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            prop_assert!(
                result.contains('m'),
                "1-59 minutes should show minutes: {} -> {}",
                secs, result
            );
            prop_assert!(
                result.contains('s'),
                "Minutes range should include seconds: {} -> {}",
                secs, result
            );
        }

        /// Invariant: Values 3600s-86399s show hours and minutes.
        #[test]
        fn format_duration_hour_range(secs in 3600i64..86400) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            prop_assert!(
                result.contains('h'),
                "Hour range should show hours: {} -> {}",
                secs, result
            );
            prop_assert!(
                result.contains('m'),
                "Hour range should include minutes: {} -> {}",
                secs, result
            );
        }

        /// Invariant: Values >= 86400s show days and hours.
        #[test]
        fn format_duration_day_range(days in 1i64..365) {
            let secs = days * 86400 + 3600; // days + 1 hour
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            prop_assert!(
                result.contains('d'),
                "Day range should show days: {} -> {}",
                secs, result
            );
            prop_assert!(
                result.contains('h'),
                "Day range should include hours: {} -> {}",
                secs, result
            );
        }

        /// Invariant: Boundary values are handled correctly.
        #[test]
        fn format_duration_boundaries(secs in boundary_seconds()) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            prop_assert!(!result.is_empty());
        }

        /// Invariant: Exact boundaries produce clean output.
        #[test]
        fn format_duration_exact_boundaries(_dummy in Just(())) {
            // Exactly 30 seconds
            prop_assert_eq!(format_duration(chrono::Duration::seconds(30)), "30s");

            // Exactly 1 minute 30 seconds
            prop_assert_eq!(format_duration(chrono::Duration::seconds(90)), "1m 30s");

            // Exactly 1 hour 1 minute
            prop_assert_eq!(format_duration(chrono::Duration::seconds(3660)), "1h 1m");

            // Exactly 1 day 1 hour
            prop_assert_eq!(format_duration(chrono::Duration::seconds(90000)), "1d 1h");
        }
    }

    // ============================================================================
    // Consistency Tests
    // ============================================================================

    proptest! {
        /// Invariant: format_bytes is deterministic.
        #[test]
        fn format_bytes_deterministic(bytes in byte_count()) {
            let result1 = format_bytes(bytes);
            let result2 = format_bytes(bytes);
            prop_assert_eq!(result1, result2, "format_bytes should be deterministic");
        }

        /// Invariant: format_duration is deterministic.
        #[test]
        fn format_duration_deterministic(secs in seconds()) {
            let duration = chrono::Duration::seconds(secs);
            let result1 = format_duration(duration);
            let result2 = format_duration(duration);
            prop_assert_eq!(result1, result2, "format_duration should be deterministic");
        }

        /// Invariant: Larger byte values don't produce shorter strings (generally).
        ///
        /// This is a rough heuristic - larger values should use larger units
        /// but the numeric part might vary. We check that the output doesn't
        /// get unexpectedly short.
        #[test]
        fn format_bytes_reasonable_length(bytes in 1u64..u64::MAX / 2) {
            let result = format_bytes(bytes);
            // At minimum: "1 B" or similar - at least 3 chars
            prop_assert!(
                result.len() >= 3,
                "Output should be at least 3 chars: {} -> {} (len={})",
                bytes, result, result.len()
            );
        }

        /// Invariant: format_duration output length is reasonable.
        #[test]
        fn format_duration_reasonable_length(secs in 0i64..86400 * 365 * 100) {
            let duration = chrono::Duration::seconds(secs);
            let result = format_duration(duration);
            // At minimum: "0s" - at least 2 chars
            prop_assert!(
                result.len() >= 2,
                "Output should be at least 2 chars: {} -> {} (len={})",
                secs, result, result.len()
            );
        }
    }

    // ============================================================================
    // Edge Case Tests
    // ============================================================================

    proptest! {
        /// Invariant: format_bytes handles max u64.
        #[test]
        fn format_bytes_max_value(_dummy in Just(())) {
            let result = format_bytes(u64::MAX);
            prop_assert!(!result.is_empty());
            // Should use TB for such a large value
            prop_assert!(result.contains("TB"), "Max u64 should use TB unit: {}", result);
        }

        /// Invariant: format_duration handles large values without panic.
        #[test]
        fn format_duration_large_values(_dummy in Just(())) {
            // 100 years in seconds
            let hundred_years = 100i64 * 365 * 86400;
            let duration = chrono::Duration::seconds(hundred_years);
            let result = format_duration(duration);

            prop_assert!(!result.is_empty());
            prop_assert!(result.contains('d'), "Large durations should use days: {}", result);
        }

        /// Invariant: Negative durations don't panic.
        #[test]
        fn format_duration_negative_handles(secs in -86400i64..0) {
            let duration = chrono::Duration::seconds(secs);
            // Should not panic - behavior for negative is implementation-defined
            // but shouldn't crash
            let _result = format_duration(duration);
        }
    }
}
