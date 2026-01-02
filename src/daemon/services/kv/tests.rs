//! Tests for the KV store module.

use super::*;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn test_set_and_get() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("key1", b"value1", None).await.unwrap();
    let value = store.get("key1").await.unwrap().unwrap();
    assert_eq!(value, b"value1");
}

#[tokio::test]
async fn test_get_nonexistent_key() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    let result = store.get("nonexistent").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_delete() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("key1", b"value1", None).await.unwrap();
    let deleted = store.delete("key1").await.unwrap();
    assert!(deleted);

    let result = store.get("key1").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_delete_nonexistent() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    let deleted = store.delete("nonexistent").await.unwrap();
    assert!(!deleted);
}

#[tokio::test]
async fn test_exists() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    assert!(!store.exists("key1").await.unwrap());

    store.set("key1", b"value1", None).await.unwrap();
    assert!(store.exists("key1").await.unwrap());

    store.delete("key1").await.unwrap();
    assert!(!store.exists("key1").await.unwrap());
}

#[tokio::test]
async fn test_list_keys_all() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("key1", b"value1", None).await.unwrap();
    store.set("key2", b"value2", None).await.unwrap();
    store.set("other", b"value3", None).await.unwrap();

    let keys = store.list_keys(None).await.unwrap();
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"key1".to_string()));
    assert!(keys.contains(&"key2".to_string()));
    assert!(keys.contains(&"other".to_string()));
}

#[tokio::test]
async fn test_list_keys_with_prefix() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("user:1", b"alice", None).await.unwrap();
    store.set("user:2", b"bob", None).await.unwrap();
    store.set("session:abc", b"xyz", None).await.unwrap();

    let user_keys = store.list_keys(Some("user:")).await.unwrap();
    assert_eq!(user_keys.len(), 2);
    assert!(user_keys.contains(&"user:1".to_string()));
    assert!(user_keys.contains(&"user:2".to_string()));

    let session_keys = store.list_keys(Some("session:")).await.unwrap();
    assert_eq!(session_keys.len(), 1);
    assert!(session_keys.contains(&"session:abc".to_string()));
}

#[tokio::test]
async fn test_overwrite_value() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("key1", b"value1", None).await.unwrap();
    store.set("key1", b"value2", None).await.unwrap();

    let value = store.get("key1").await.unwrap().unwrap();
    assert_eq!(value, b"value2");
}

#[tokio::test]
async fn test_binary_data() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    let binary_data = vec![0u8, 1, 2, 3, 255, 128, 64];
    store.set("binary", &binary_data, None).await.unwrap();

    let value = store.get("binary").await.unwrap().unwrap();
    assert_eq!(value, binary_data);
}

#[tokio::test]
async fn test_ttl_expiration() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    // Set with 2 second TTL
    let ttl = Some(Duration::from_secs(2));
    store.set("temp", b"expires soon", ttl).await.unwrap();

    // Should exist immediately
    assert!(store.exists("temp").await.unwrap());
    let value = store.get("temp").await.unwrap().unwrap();
    assert_eq!(value, b"expires soon");

    // Wait for expiration (3 seconds to ensure TTL has passed)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Should be expired and automatically removed
    assert!(!store.exists("temp").await.unwrap());
    assert!(store.get("temp").await.unwrap().is_none());
}

#[tokio::test]
async fn test_ttl_no_expiration() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    // Set with long TTL
    let ttl = Some(Duration::from_secs(3600));
    store.set("key", b"value", ttl).await.unwrap();

    // Should still exist
    assert!(store.exists("key").await.unwrap());
    let value = store.get("key").await.unwrap().unwrap();
    assert_eq!(value, b"value");
}

#[tokio::test]
async fn test_list_keys_filters_expired() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    // Add permanent key
    store.set("permanent", b"forever", None).await.unwrap();

    // Add temporary key with 3 second TTL (longer to avoid CI timing issues)
    let ttl = Some(Duration::from_secs(3));
    store.set("temporary", b"short-lived", ttl).await.unwrap();

    // Both should appear initially
    let keys = store.list_keys(None).await.unwrap();
    assert_eq!(keys.len(), 2);

    // Wait for expiration
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Only permanent key should remain
    let keys = store.list_keys(None).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&"permanent".to_string()));
}

#[tokio::test]
async fn test_persistence_across_reopens() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");

    {
        let store = KvStore::file(&db_path).unwrap();
        store.set("persistent", b"value", None).await.unwrap();
    }

    // Reopen database and verify data persists
    {
        let store = KvStore::file(&db_path).unwrap();
        let value = store.get("persistent").await.unwrap().unwrap();
        assert_eq!(value, b"value");
    }
}

#[tokio::test]
async fn test_delete_all_keys() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("key1", b"value1", None).await.unwrap();
    store.set("key2", b"value2", None).await.unwrap();
    store.set("key3", b"value3", None).await.unwrap();

    assert_eq!(store.list_keys(None).await.unwrap().len(), 3);

    // Delete all keys individually
    for key in store.list_keys(None).await.unwrap() {
        store.delete(&key).await.unwrap();
    }

    assert_eq!(store.list_keys(None).await.unwrap().len(), 0);
    assert!(!store.exists("key1").await.unwrap());
}

#[tokio::test]
async fn test_update_ttl() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    // Set with short TTL
    let short_ttl = Some(Duration::from_secs(1));
    store.set("key", b"value1", short_ttl).await.unwrap();

    // Overwrite with longer TTL before expiration
    let long_ttl = Some(Duration::from_secs(3600));
    store.set("key", b"value2", long_ttl).await.unwrap();

    // Wait for original TTL to pass
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Should still exist with new value
    assert!(store.exists("key").await.unwrap());
    let value = store.get("key").await.unwrap().unwrap();
    assert_eq!(value, b"value2");
}

#[tokio::test]
async fn test_empty_value() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.redb");
    let store = KvStore::file(&db_path).unwrap();

    store.set("empty", b"", None).await.unwrap();
    let value = store.get("empty").await.unwrap().unwrap();
    assert_eq!(value, b"");
}

#[tokio::test]
async fn test_memory_backend() {
    let store = KvStore::memory();

    store.set("key1", b"value1", None).await.unwrap();
    let value = store.get("key1").await.unwrap().unwrap();
    assert_eq!(value, b"value1");

    // Memory backend data is not persisted between instances
    let store2 = KvStore::memory();
    assert!(store2.get("key1").await.unwrap().is_none());
}
