//! Embedded queue service with optional persistence.
//!
//! Provides in-memory queue operations with optional redb persistence
//! and simple pub/sub functionality for inter-instance communication.
//!
//! # Examples
//!
//! ## Basic Queue Operations
//!
//! ```rust
//! use mik::daemon::services::queue::{QueueService, QueueConfig};
//!
//! # fn main() -> anyhow::Result<()> {
//! // Create an in-memory queue service
//! let service = QueueService::new(QueueConfig::default())?;
//!
//! // Push messages
//! let msg_id = service.push("tasks", b"process this")?;
//! service.push("tasks", b"then process this")?;
//!
//! // Pop messages (FIFO order)
//! let msg = service.pop("tasks")?.unwrap();
//! assert_eq!(msg.data, b"process this");
//!
//! // Peek without removing
//! let next = service.peek("tasks")?.unwrap();
//! assert_eq!(next.data, b"then process this");
//! # Ok(())
//! # }
//! ```
//!
//! ## Pub/Sub Pattern
//!
//! ```rust
//! use mik::daemon::services::queue::{QueueService, QueueConfig};
//!
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! let service = QueueService::new(QueueConfig::default())?;
//!
//! // Subscribe to a topic
//! let mut subscriber1 = service.subscribe("events");
//! let mut subscriber2 = service.subscribe("events");
//!
//! // Publish a message
//! let count = service.publish("events", b"something happened")?;
//! assert_eq!(count, 2); // 2 active subscribers
//!
//! // Both subscribers receive it
//! let msg1 = subscriber1.recv().await.unwrap();
//! let msg2 = subscriber2.recv().await.unwrap();
//! assert_eq!(msg1.data, msg2.data);
//! # Ok(())
//! # }
//! ```
//!
//! ## Persistent Queues
//!
//! ```rust
//! use mik::daemon::services::queue::{QueueService, QueueConfig};
//! use std::path::PathBuf;
//!
//! # fn main() -> anyhow::Result<()> {
//! # let temp_dir = tempfile::tempdir()?;
//! let config = QueueConfig {
//!     persist: true,
//!     db_path: Some(temp_dir.path().join("queues.db")),
//!     max_queue_size: None,
//! };
//!
//! let service = QueueService::new(config)?;
//! service.push("durable_queue", b"important message")?;
//!
//! // Messages are persisted to disk automatically
//! # Ok(())
//! # }
//! ```

#![allow(dead_code)] // Queue service for future sidecar integration
#![allow(clippy::unnecessary_wraps)]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use redb::{ReadableDatabase, ReadableTable};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Maximum number of subscribers per topic
const MAX_SUBSCRIBERS_PER_TOPIC: usize = 1024;

/// Serde helper for `DateTime<Utc>` as RFC3339 string (backward compatible).
mod datetime_rfc3339 {
    use chrono::{DateTime, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(date: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&date.to_rfc3339())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

/// Message in a queue with metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueueMessage {
    pub id: String,
    pub data: Vec<u8>,
    #[serde(with = "datetime_rfc3339")]
    pub created_at: DateTime<Utc>,
}

/// A single queue with its messages
#[derive(Debug)]
struct Queue {
    messages: VecDeque<QueueMessage>,
    max_size: Option<usize>,
}

impl Queue {
    fn new(max_size: Option<usize>) -> Self {
        Self {
            messages: VecDeque::new(),
            max_size,
        }
    }

    fn push(&mut self, message: QueueMessage) -> Result<()> {
        if let Some(max) = self.max_size
            && self.messages.len() >= max
        {
            anyhow::bail!("Queue is full (max size: {max})");
        }
        self.messages.push_back(message);
        Ok(())
    }

    fn pop(&mut self) -> Option<QueueMessage> {
        self.messages.pop_front()
    }

    fn peek(&self) -> Option<&QueueMessage> {
        self.messages.front()
    }

    fn len(&self) -> usize {
        self.messages.len()
    }

    fn clear(&mut self) -> usize {
        let count = self.messages.len();
        self.messages.clear();
        count
    }
}

/// Configuration for the queue service
#[derive(Debug, Clone, Default)]
pub struct QueueConfig {
    /// Enable persistence to disk
    pub persist: bool,
    /// Path to redb database file (if persistence enabled)
    pub db_path: Option<std::path::PathBuf>,
    /// Maximum size per queue (None = unlimited)
    pub max_queue_size: Option<usize>,
}

/// Embedded queue service
#[derive(Clone)]
pub struct QueueService {
    inner: Arc<QueueServiceInner>,
}

struct QueueServiceInner {
    /// All queues indexed by name
    queues: RwLock<HashMap<String, Queue>>,
    /// Pub/sub topics with broadcast channels
    topics: RwLock<HashMap<String, broadcast::Sender<QueueMessage>>>,
    /// Configuration
    config: QueueConfig,
    /// Optional persistence layer
    db: Option<RwLock<redb::Database>>,
}

impl QueueService {
    /// Create a new queue service.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Persistence is enabled but `db_path` is not provided
    /// - Database directory cannot be created
    /// - Database file cannot be opened or created
    /// - Persisted queue data cannot be loaded (corruption)
    pub fn new(config: QueueConfig) -> Result<Self> {
        let db = if config.persist {
            let path = config
                .db_path
                .as_ref()
                .context("db_path required when persist is enabled")?;

            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).context("Failed to create database directory")?;
            }

            let database =
                redb::Database::create(path).context("Failed to create redb database")?;
            Some(RwLock::new(database))
        } else {
            None
        };

        let mut service = Self {
            inner: Arc::new(QueueServiceInner {
                queues: RwLock::new(HashMap::new()),
                topics: RwLock::new(HashMap::new()),
                config,
                db,
            }),
        };

        // Load persisted queues if persistence is enabled
        if service.inner.db.is_some() {
            service.load_from_disk()?;
        }

        Ok(service)
    }

    /// Push a message to a queue.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue is full (when `max_queue_size` is set)
    /// or if persistence fails.
    pub fn push(&self, queue_name: &str, message: &[u8]) -> Result<String> {
        let msg = QueueMessage {
            id: Uuid::new_v4().to_string(),
            data: message.to_vec(),
            created_at: Utc::now(),
        };

        let msg_id = msg.id.clone();

        // Add to in-memory queue
        {
            let mut queues = self.inner.queues.write();
            let queue = queues
                .entry(queue_name.to_string())
                .or_insert_with(|| Queue::new(self.inner.config.max_queue_size));
            queue.push(msg.clone())?;
        }

        // Persist if enabled
        if self.inner.config.persist {
            self.persist_queue(queue_name)?;
        }

        Ok(msg_id)
    }

    /// Pop a message from a queue.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence is enabled and the database write fails.
    pub fn pop(&self, queue_name: &str) -> Result<Option<QueueMessage>> {
        let msg = {
            let mut queues = self.inner.queues.write();
            queues.get_mut(queue_name).and_then(Queue::pop)
        };

        // Persist if enabled and message was popped
        if msg.is_some() && self.inner.config.persist {
            self.persist_queue(queue_name)?;
        }

        Ok(msg)
    }

    /// Peek at the next message without removing it.
    ///
    /// # Errors
    ///
    /// This method is infallible in practice but returns `Result` for API consistency.
    pub fn peek(&self, queue_name: &str) -> Result<Option<QueueMessage>> {
        let queues = self.inner.queues.read();
        Ok(queues.get(queue_name).and_then(|q| q.peek()).cloned())
    }

    /// Get the length of a queue.
    ///
    /// # Errors
    ///
    /// This method is infallible in practice but returns `Result` for API consistency.
    pub fn len(&self, queue_name: &str) -> Result<usize> {
        let queues = self.inner.queues.read();
        Ok(queues.get(queue_name).map_or(0, Queue::len))
    }

    /// Check if a queue is empty.
    ///
    /// # Errors
    ///
    /// This method is infallible in practice but returns `Result` for API consistency.
    pub fn is_empty(&self, queue_name: &str) -> Result<bool> {
        Ok(self.len(queue_name)? == 0)
    }

    /// Clear all messages from a queue.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence is enabled and the database write fails.
    pub fn clear(&self, queue_name: &str) -> Result<usize> {
        let count = {
            let mut queues = self.inner.queues.write();
            queues.get_mut(queue_name).map_or(0, Queue::clear)
        };

        // Persist if enabled
        if count > 0 && self.inner.config.persist {
            self.persist_queue(queue_name)?;
        }

        Ok(count)
    }

    /// Delete a queue entirely.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence is enabled and the database write fails.
    pub fn delete_queue(&self, queue_name: &str) -> Result<bool> {
        let existed = {
            let mut queues = self.inner.queues.write();
            queues.remove(queue_name).is_some()
        };

        // Remove from disk if enabled
        if existed && self.inner.config.persist {
            self.delete_queue_from_disk(queue_name)?;
        }

        Ok(existed)
    }

    /// List all queue names
    pub fn list_queues(&self) -> Vec<String> {
        let queues = self.inner.queues.read();
        queues.keys().cloned().collect()
    }

    /// Publish a message to all subscribers of a topic.
    ///
    /// # Errors
    ///
    /// This method is infallible in practice but returns `Result` for API consistency.
    pub fn publish(&self, topic: &str, message: &[u8]) -> Result<usize> {
        let msg = QueueMessage {
            id: Uuid::new_v4().to_string(),
            data: message.to_vec(),
            created_at: Utc::now(),
        };

        let topics = self.inner.topics.read();
        if let Some(sender) = topics.get(topic) {
            // send() returns the number of active receivers
            let count = sender.send(msg).unwrap_or(0);
            Ok(count)
        } else {
            // No subscribers yet
            Ok(0)
        }
    }

    /// Subscribe to a topic
    pub fn subscribe(&self, topic: &str) -> broadcast::Receiver<QueueMessage> {
        let mut topics = self.inner.topics.write();
        let sender = topics
            .entry(topic.to_string())
            .or_insert_with(|| broadcast::channel(MAX_SUBSCRIBERS_PER_TOPIC).0);
        sender.subscribe()
    }

    /// Get the number of active subscribers for a topic
    pub fn subscriber_count(&self, topic: &str) -> usize {
        let topics = self.inner.topics.read();
        topics
            .get(topic)
            .map_or(0, tokio::sync::broadcast::Sender::receiver_count)
    }

    /// List all topic names
    pub fn list_topics(&self) -> Vec<String> {
        let topics = self.inner.topics.read();
        topics.keys().cloned().collect()
    }

    /// Persist a queue to disk (internal)
    fn persist_queue(&self, queue_name: &str) -> Result<()> {
        if let Some(ref db_lock) = self.inner.db {
            let db = db_lock.read();
            let write_txn = db.begin_write()?;

            {
                const TABLE: redb::TableDefinition<'static, &'static str, &'static [u8]> =
                    redb::TableDefinition::new("queues");

                let mut table = write_txn.open_table(TABLE)?;

                // Serialize queue messages
                let queues = self.inner.queues.read();
                if let Some(queue) = queues.get(queue_name) {
                    let serialized =
                        serde_json::to_vec(&queue.messages).context("Failed to serialize queue")?;
                    table.insert(queue_name, serialized.as_slice())?;
                } else {
                    // Queue was deleted, remove from db
                    table.remove(queue_name)?;
                }
            }

            write_txn.commit()?;
        }
        Ok(())
    }

    /// Delete a queue from disk (internal)
    fn delete_queue_from_disk(&self, queue_name: &str) -> Result<()> {
        if let Some(ref db_lock) = self.inner.db {
            let db = db_lock.read();
            let write_txn = db.begin_write()?;

            {
                const TABLE: redb::TableDefinition<'static, &'static str, &'static [u8]> =
                    redb::TableDefinition::new("queues");
                let mut table = write_txn.open_table(TABLE)?;
                table.remove(queue_name)?;
            }

            write_txn.commit()?;
        }
        Ok(())
    }

    /// Load all queues from disk (internal)
    fn load_from_disk(&mut self) -> Result<()> {
        const TABLE: redb::TableDefinition<'static, &'static str, &'static [u8]> =
            redb::TableDefinition::new("queues");

        if let Some(ref db_lock) = self.inner.db {
            let db = db_lock.read();
            let read_txn = db.begin_read()?;

            // Check if table exists
            let table = match read_txn.open_table(TABLE) {
                Ok(t) => t,
                Err(redb::TableError::TableDoesNotExist(_)) => {
                    // Table doesn't exist yet, nothing to load
                    return Ok(());
                },
                Err(e) => return Err(e.into()),
            };

            let mut queues = self.inner.queues.write();

            for result in table.iter()? {
                let (name, data) = result?;
                let name_str = name.value();
                let messages: VecDeque<QueueMessage> = serde_json::from_slice(data.value())
                    .with_context(|| format!("Failed to deserialize queue '{name_str}'"))?;

                let mut queue = Queue::new(self.inner.config.max_queue_size);
                queue.messages = messages;
                queues.insert(name_str.to_string(), queue);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn test_push_pop() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        // Push messages
        let id1 = service.push("test_queue", b"message 1")?;
        let id2 = service.push("test_queue", b"message 2")?;

        assert_ne!(id1, id2);
        assert_eq!(service.len("test_queue")?, 2);

        // Pop messages in FIFO order
        let msg1 = service.pop("test_queue")?.unwrap();
        assert_eq!(msg1.data, b"message 1");
        assert_eq!(msg1.id, id1);

        let msg2 = service.pop("test_queue")?.unwrap();
        assert_eq!(msg2.data, b"message 2");
        assert_eq!(msg2.id, id2);

        // Queue should be empty now
        assert_eq!(service.len("test_queue")?, 0);
        assert!(service.pop("test_queue")?.is_none());

        Ok(())
    }

    #[test]
    fn test_peek() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        service.push("test_queue", b"message 1")?;
        service.push("test_queue", b"message 2")?;

        // Peek doesn't remove
        let msg = service.peek("test_queue")?.unwrap();
        assert_eq!(msg.data, b"message 1");
        assert_eq!(service.len("test_queue")?, 2);

        // Peek again, should be the same
        let msg = service.peek("test_queue")?.unwrap();
        assert_eq!(msg.data, b"message 1");

        Ok(())
    }

    #[test]
    fn test_clear() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        service.push("test_queue", b"message 1")?;
        service.push("test_queue", b"message 2")?;
        service.push("test_queue", b"message 3")?;

        let count = service.clear("test_queue")?;
        assert_eq!(count, 3);
        assert_eq!(service.len("test_queue")?, 0);

        Ok(())
    }

    #[test]
    fn test_multiple_queues() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        service.push("queue_a", b"message a1")?;
        service.push("queue_b", b"message b1")?;
        service.push("queue_a", b"message a2")?;

        assert_eq!(service.len("queue_a")?, 2);
        assert_eq!(service.len("queue_b")?, 1);

        let msg_a = service.pop("queue_a")?.unwrap();
        assert_eq!(msg_a.data, b"message a1");

        let msg_b = service.pop("queue_b")?.unwrap();
        assert_eq!(msg_b.data, b"message b1");

        assert_eq!(service.len("queue_a")?, 1);
        assert_eq!(service.len("queue_b")?, 0);

        Ok(())
    }

    #[test]
    fn test_max_queue_size() -> Result<()> {
        let config = QueueConfig {
            max_queue_size: Some(2),
            ..Default::default()
        };
        let service = QueueService::new(config)?;

        service.push("test_queue", b"message 1")?;
        service.push("test_queue", b"message 2")?;

        // Third push should fail
        let result = service.push("test_queue", b"message 3");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Queue is full"));

        Ok(())
    }

    #[test]
    fn test_list_queues() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        service.push("queue_a", b"message")?;
        service.push("queue_b", b"message")?;
        service.push("queue_c", b"message")?;

        let mut queues = service.list_queues();
        queues.sort();

        assert_eq!(queues, vec!["queue_a", "queue_b", "queue_c"]);

        Ok(())
    }

    #[test]
    fn test_delete_queue() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        service.push("test_queue", b"message 1")?;
        service.push("test_queue", b"message 2")?;

        assert!(service.delete_queue("test_queue")?);
        assert_eq!(service.len("test_queue")?, 0);
        assert!(!service.delete_queue("test_queue")?);

        Ok(())
    }

    #[tokio::test]
    async fn test_pubsub() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        // Subscribe to a topic
        let mut rx1 = service.subscribe("test_topic");
        let mut rx2 = service.subscribe("test_topic");

        assert_eq!(service.subscriber_count("test_topic"), 2);

        // Publish a message
        let count = service.publish("test_topic", b"hello world")?;
        assert_eq!(count, 2);

        // Both subscribers should receive it
        let msg1 = timeout(Duration::from_millis(100), rx1.recv())
            .await?
            .unwrap();
        assert_eq!(msg1.data, b"hello world");

        let msg2 = timeout(Duration::from_millis(100), rx2.recv())
            .await?
            .unwrap();
        assert_eq!(msg2.data, b"hello world");

        Ok(())
    }

    #[tokio::test]
    async fn test_pubsub_no_subscribers() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        // Publish to a topic with no subscribers
        let count = service.publish("empty_topic", b"message")?;
        assert_eq!(count, 0);

        Ok(())
    }

    #[test]
    fn test_list_topics() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        service.subscribe("topic_a");
        service.subscribe("topic_b");
        service.subscribe("topic_c");

        let mut topics = service.list_topics();
        topics.sort();

        assert_eq!(topics, vec!["topic_a", "topic_b", "topic_c"]);

        Ok(())
    }

    #[test]
    fn test_persistence() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let db_path = temp_dir.path().join("queue.db");

        // Create service with persistence
        {
            let config = QueueConfig {
                persist: true,
                db_path: Some(db_path.clone()),
                max_queue_size: None,
            };
            let service = QueueService::new(config)?;

            service.push("persistent_queue", b"message 1")?;
            service.push("persistent_queue", b"message 2")?;
            service.push("another_queue", b"message 3")?;
        }

        // Reload from disk
        {
            let config = QueueConfig {
                persist: true,
                db_path: Some(db_path.clone()),
                max_queue_size: None,
            };
            let service = QueueService::new(config)?;

            assert_eq!(service.len("persistent_queue")?, 2);
            assert_eq!(service.len("another_queue")?, 1);

            let msg1 = service.pop("persistent_queue")?.unwrap();
            assert_eq!(msg1.data, b"message 1");

            let msg2 = service.pop("persistent_queue")?.unwrap();
            assert_eq!(msg2.data, b"message 2");

            let msg3 = service.pop("another_queue")?.unwrap();
            assert_eq!(msg3.data, b"message 3");
        }

        Ok(())
    }

    #[test]
    fn test_message_metadata() -> Result<()> {
        let service = QueueService::new(QueueConfig::default())?;

        let before = Utc::now();
        let id = service.push("test_queue", b"test message")?;
        let after = Utc::now();

        let msg = service.pop("test_queue")?.unwrap();

        // Check ID format (UUID v4)
        assert_eq!(msg.id, id);
        assert!(Uuid::parse_str(&msg.id).is_ok());

        // Check timestamp is within reasonable bounds
        assert!(msg.created_at >= before);
        assert!(msg.created_at <= after);

        Ok(())
    }
}
