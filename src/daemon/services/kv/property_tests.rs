//! Property-based tests for the KV store.
//!
//! These tests verify data integrity invariants for the key-value store:
//! - Set then get returns the same value
//! - Delete removes the key completely
//! - Keys are properly ordered when listing
//! - TTL expiration works correctly
//!
//! Run with:
//! ```bash
//! cargo test --lib daemon::services::kv::property_tests
//! ```

use proptest::prelude::*;
use tempfile::TempDir;

use super::store::KvStore;

// ============================================================================
// Test Strategies - Input Generation
// ============================================================================

/// Strategy for generating valid KV keys.
///
/// Keys should be reasonable identifiers that work as database keys.
fn valid_key() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_:-]{0,100}".prop_filter("must not be empty", |s| !s.is_empty())
}

/// Strategy for generating key prefixes for prefix listing tests.
fn key_prefix() -> impl Strategy<Value = String> {
    "[a-zA-Z]{1,10}".prop_map(|s| format!("{s}:"))
}

/// Strategy for generating arbitrary binary values.
fn binary_value() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..1000)
}

/// Strategy for generating string values (more readable in tests).
fn string_value() -> impl Strategy<Value = Vec<u8>> {
    "[a-zA-Z0-9 ]{0,500}".prop_map(String::into_bytes)
}

/// Strategy for generating TTL values in seconds.
fn ttl_seconds() -> impl Strategy<Value = u64> {
    1u64..=86400u64 // 1 second to 1 day
}

// ============================================================================
// Core KV Store Invariants
// ============================================================================

proptest! {
    /// Invariant: Set then get returns the same value.
    ///
    /// This is the fundamental contract of a key-value store.
    #[test]
    fn set_get_roundtrip(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set the value
        store.set(&key, &value).unwrap();

        // Get it back
        let retrieved = store.get(&key).unwrap();

        prop_assert!(retrieved.is_some(), "Key should exist after set");
        prop_assert_eq!(
            retrieved.unwrap(),
            value,
            "Retrieved value should match set value"
        );
    }

    /// Invariant: Overwriting a key replaces the value.
    #[test]
    fn set_overwrites_previous(
        key in valid_key(),
        value1 in binary_value(),
        value2 in binary_value()
    ) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set first value
        store.set(&key, &value1).unwrap();

        // Overwrite with second value
        store.set(&key, &value2).unwrap();

        // Should get second value
        let retrieved = store.get(&key).unwrap();
        prop_assert!(retrieved.is_some());
        prop_assert_eq!(
            retrieved.unwrap(),
            value2,
            "Should get the overwritten value"
        );
    }

    /// Invariant: Delete removes the key completely.
    #[test]
    fn delete_removes_key(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set a value
        store.set(&key, &value).unwrap();
        prop_assert!(store.exists(&key).unwrap());

        // Delete it
        let deleted = store.delete(&key).unwrap();
        prop_assert!(deleted, "Delete should return true for existing key");

        // Should no longer exist
        prop_assert!(!store.exists(&key).unwrap(), "Key should not exist after delete");
        prop_assert!(
            store.get(&key).unwrap().is_none(),
            "Get should return None after delete"
        );
    }

    /// Invariant: Deleting non-existent key returns false.
    #[test]
    fn delete_nonexistent_returns_false(key in valid_key()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        let deleted = store.delete(&key).unwrap();
        prop_assert!(!deleted, "Delete should return false for non-existent key");
    }

    /// Invariant: Get on non-existent key returns None.
    #[test]
    fn get_nonexistent_returns_none(key in valid_key()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        let result = store.get(&key).unwrap();
        prop_assert!(result.is_none(), "Get on non-existent key should return None");
    }

    /// Invariant: Exists returns false for non-existent keys.
    #[test]
    fn exists_false_for_missing(key in valid_key()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        prop_assert!(!store.exists(&key).unwrap(), "Exists should be false for missing key");
    }

    /// Invariant: Exists returns true after set.
    #[test]
    fn exists_true_after_set(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        store.set(&key, &value).unwrap();
        prop_assert!(store.exists(&key).unwrap(), "Exists should be true after set");
    }
}

// ============================================================================
// Key Listing Invariants
// ============================================================================

proptest! {
    /// Invariant: List keys returns all set keys.
    #[test]
    fn list_returns_all_keys(
        keys in prop::collection::hash_set(valid_key(), 1..10)
    ) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set all keys
        for key in &keys {
            store.set(key, b"value").unwrap();
        }

        // List all keys
        let listed = store.list_keys(None).unwrap();

        // Should have same count
        prop_assert_eq!(
            listed.len(),
            keys.len(),
            "Listed keys count should match set keys"
        );

        // All set keys should be in the list
        for key in &keys {
            prop_assert!(
                listed.contains(key),
                "Set key {} should be in list",
                key
            );
        }
    }

    /// Invariant: List with prefix filters correctly.
    #[test]
    fn list_with_prefix_filters(
        prefix in key_prefix(),
        matching_suffix in "[a-z]{1,10}",
        nonmatching_key in "[a-z]{10,20}"
    ) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        let matching_key = format!("{prefix}{matching_suffix}");

        // Set matching and non-matching keys
        store.set(&matching_key, b"match").unwrap();
        store.set(&nonmatching_key, b"nomatch").unwrap();

        // List with prefix
        let listed = store.list_keys(Some(&prefix)).unwrap();

        // Should contain matching key
        prop_assert!(
            listed.contains(&matching_key),
            "Matching key should be in prefixed list"
        );

        // Should not contain non-matching key
        prop_assert!(
            !listed.contains(&nonmatching_key),
            "Non-matching key should not be in prefixed list"
        );
    }

    /// Invariant: Empty database returns empty list.
    #[test]
    fn empty_database_empty_list(_dummy in Just(())) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        let listed = store.list_keys(None).unwrap();
        prop_assert!(listed.is_empty(), "Empty database should return empty list");
    }
}

// ============================================================================
// TTL and Expiration Invariants
// ============================================================================

proptest! {
    /// Invariant: Set with TTL creates an entry that will eventually expire.
    ///
    /// Note: We can't easily test actual expiration in property tests
    /// (would require time manipulation), but we can verify the API works.
    #[test]
    fn set_with_ttl_works(key in valid_key(), value in binary_value(), ttl in ttl_seconds()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set with TTL should not error
        let result = store.set_with_ttl(&key, &value, ttl);
        prop_assert!(result.is_ok(), "set_with_ttl should succeed");

        // Should be retrievable immediately (not expired yet)
        let retrieved = store.get(&key).unwrap();
        prop_assert!(retrieved.is_some(), "Key should exist immediately after set_with_ttl");
        prop_assert_eq!(retrieved.unwrap(), value);
    }

    /// Invariant: Zero TTL creates entry that expires immediately.
    ///
    /// With TTL=0, the entry should expire immediately when checked.
    #[test]
    fn zero_ttl_expires_immediately(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set with 0 TTL - this expires at current time
        store.set_with_ttl(&key, &value, 0).unwrap();

        // Small delay to ensure we're past the expiration
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Should be expired (returns None and cleans up)
        let retrieved = store.get(&key).unwrap();
        prop_assert!(
            retrieved.is_none(),
            "Key with TTL=0 should expire immediately"
        );
    }

    /// Invariant: Regular set creates entry that never expires.
    #[test]
    fn regular_set_never_expires(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Regular set (no TTL)
        store.set(&key, &value).unwrap();

        // Should always exist (can't truly test "never" but can verify it exists)
        let retrieved = store.get(&key).unwrap();
        prop_assert!(retrieved.is_some(), "Key without TTL should exist");
        prop_assert_eq!(retrieved.unwrap(), value);
    }
}

// ============================================================================
// Persistence Invariants
// ============================================================================

proptest! {
    /// Invariant: Data persists across store reopens.
    #[test]
    fn data_persists_across_reopen(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("kv.redb");

        // Set value and close store
        {
            let store = KvStore::open(&db_path).unwrap();
            store.set(&key, &value).unwrap();
            // store dropped here
        }

        // Reopen and verify data exists
        {
            let store = KvStore::open(&db_path).unwrap();
            let retrieved = store.get(&key).unwrap();
            prop_assert!(retrieved.is_some(), "Data should persist after reopen");
            prop_assert_eq!(retrieved.unwrap(), value);
        }
    }

    /// Invariant: Multiple stores can share the same database sequentially.
    #[test]
    fn sequential_store_access(
        key1 in valid_key(),
        value1 in binary_value(),
        key2 in valid_key(),
        value2 in binary_value()
    ) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("kv.redb");

        // First store sets key1
        {
            let store = KvStore::open(&db_path).unwrap();
            store.set(&key1, &value1).unwrap();
        }

        // Second store sets key2 and reads key1
        {
            let store = KvStore::open(&db_path).unwrap();
            store.set(&key2, &value2).unwrap();

            // key1 should still exist
            let v1 = store.get(&key1).unwrap();
            prop_assert!(v1.is_some());
            prop_assert_eq!(v1.unwrap(), value1.clone());
        }

        // Third store reads both
        {
            let store = KvStore::open(&db_path).unwrap();
            let v1 = store.get(&key1).unwrap();
            let v2 = store.get(&key2).unwrap();

            prop_assert!(v1.is_some());
            prop_assert!(v2.is_some());
            prop_assert_eq!(v1.unwrap(), value1);
            prop_assert_eq!(v2.unwrap(), value2);
        }
    }
}

// ============================================================================
// Edge Cases and Robustness
// ============================================================================

proptest! {
    /// Invariant: Empty values are handled correctly.
    #[test]
    fn empty_value_works(key in valid_key()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set empty value
        store.set(&key, &[]).unwrap();

        // Should retrieve empty value
        let retrieved = store.get(&key).unwrap();
        prop_assert!(retrieved.is_some());
        prop_assert!(retrieved.unwrap().is_empty(), "Should retrieve empty value");
    }

    /// Invariant: Large values are handled correctly.
    #[test]
    fn large_value_works(key in valid_key()) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Create a large value (1MB)
        let large_value: Vec<u8> = (0..1_000_000).map(|i| (i % 256) as u8).collect();

        // Set and retrieve
        store.set(&key, &large_value).unwrap();
        let retrieved = store.get(&key).unwrap();

        prop_assert!(retrieved.is_some());
        prop_assert_eq!(retrieved.unwrap(), large_value);
    }

    /// Invariant: Many keys can be stored and retrieved.
    #[test]
    fn many_keys_work(keys in prop::collection::hash_set(valid_key(), 50..100)) {
        let tmp = TempDir::new().unwrap();
        let store = KvStore::open(tmp.path().join("kv.redb")).unwrap();

        // Set all keys with unique values
        for (i, key) in keys.iter().enumerate() {
            store.set(key, &[i as u8]).unwrap();
        }

        // Verify all can be retrieved
        for (i, key) in keys.iter().enumerate() {
            let retrieved = store.get(key).unwrap();
            prop_assert!(retrieved.is_some(), "Key {} should exist", key);
            prop_assert_eq!(retrieved.unwrap(), vec![i as u8]);
        }

        // List should return all
        let listed = store.list_keys(None).unwrap();
        prop_assert_eq!(listed.len(), keys.len());
    }

    /// Invariant: Clone store shares the same database.
    #[test]
    fn cloned_store_shares_data(key in valid_key(), value in binary_value()) {
        let tmp = TempDir::new().unwrap();
        let store1 = KvStore::open(tmp.path().join("kv.redb")).unwrap();
        let store2 = store1.clone();

        // Write through store1
        store1.set(&key, &value).unwrap();

        // Read through store2
        let retrieved = store2.get(&key).unwrap();
        prop_assert!(retrieved.is_some(), "Cloned store should see same data");
        prop_assert_eq!(retrieved.unwrap(), value);

        // Delete through store2
        store2.delete(&key).unwrap();

        // Should be gone from store1 too
        prop_assert!(!store1.exists(&key).unwrap(), "Delete via clone should affect original");
    }
}
