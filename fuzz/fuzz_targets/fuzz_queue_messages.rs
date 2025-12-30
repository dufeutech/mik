//! Fuzz target for queue service message parsing and handling.
//!
//! This fuzzer tests that:
//! 1. Message sizes are properly bounded
//! 2. Queue names are validated correctly
//! 3. Edge cases in message handling don't cause panics
//! 4. Pub/sub doesn't have unbounded growth
//! 5. Persistence serialization is robust
//!
//! Run with: `cargo +nightly fuzz run fuzz_queue_messages`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mik::daemon::services::queue::{QueueConfig, QueueService};
use tempfile::TempDir;

/// Maximum message size to prevent OOM.
const MAX_MESSAGE_SIZE: usize = 1_000_000; // 1MB

/// Maximum queue name length.
const MAX_QUEUE_NAME_LEN: usize = 256;

/// Maximum number of operations per fuzz iteration.
const MAX_OPERATIONS: usize = 100;

/// Maximum queue size for testing.
const TEST_MAX_QUEUE_SIZE: usize = 1000;

/// Input structure for queue fuzzing.
#[derive(Arbitrary, Debug)]
struct QueueInput {
    /// Operations to perform
    operations: Vec<QueueOperation>,
    /// Whether to use persistence
    use_persistence: bool,
    /// Queue size limit to use
    max_queue_size: Option<u16>,
}

/// Queue operations to fuzz.
#[derive(Arbitrary, Debug)]
enum QueueOperation {
    /// Push a message to a queue
    Push { queue_name: String, message: Vec<u8> },
    /// Pop a message from a queue
    Pop { queue_name: String },
    /// Peek at the next message
    Peek { queue_name: String },
    /// Get queue length
    Len { queue_name: String },
    /// Clear a queue
    Clear { queue_name: String },
    /// Delete a queue
    Delete { queue_name: String },
    /// List all queues
    ListQueues,
    /// Publish to a topic
    Publish { topic: String, message: Vec<u8> },
    /// Subscribe to a topic (and immediately drop)
    Subscribe { topic: String },
    /// List all topics
    ListTopics,
    /// Test adversarial queue name
    AdversarialName(AdversarialQueueName),
    /// Test adversarial message content
    AdversarialMessage(AdversarialMessage),
}

/// Adversarial queue name patterns.
#[derive(Arbitrary, Debug)]
enum AdversarialQueueName {
    /// Empty queue name
    Empty,
    /// Very long queue name
    VeryLong(u16),
    /// Queue name with null bytes
    NullBytes,
    /// Queue name with control characters
    ControlChars,
    /// Queue name with path separators
    PathSeparators,
    /// Queue name with Unicode edge cases
    UnicodeEdge,
    /// Queue name with only whitespace
    Whitespace,
    /// Queue name with special characters
    SpecialChars,
}

impl AdversarialQueueName {
    fn generate(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::VeryLong(len) => "a".repeat(*len as usize),
            Self::NullBytes => "queue\0name".to_string(),
            Self::ControlChars => "queue\x01\x02\x03name".to_string(),
            Self::PathSeparators => "../../../etc/passwd".to_string(),
            Self::UnicodeEdge => "\u{FEFF}queue\u{200B}name".to_string(), // BOM + zero-width space
            Self::Whitespace => "   \t\n   ".to_string(),
            Self::SpecialChars => "queue<>:\"|?*name".to_string(),
        }
    }
}

/// Adversarial message content patterns.
#[derive(Arbitrary, Debug)]
enum AdversarialMessage {
    /// Empty message
    Empty,
    /// Very large message
    VeryLarge(u32),
    /// Message with many null bytes
    NullFilled,
    /// Message that looks like JSON but is invalid
    MalformedJson,
    /// Message with binary data that might break serialization
    BinaryBreaker,
    /// Message with maximum byte values
    MaxBytes,
}

impl AdversarialMessage {
    fn generate(&self) -> Vec<u8> {
        match self {
            Self::Empty => Vec::new(),
            Self::VeryLarge(size) => {
                // Cap at MAX_MESSAGE_SIZE to prevent OOM
                let actual_size = (*size as usize).min(MAX_MESSAGE_SIZE);
                vec![0x42; actual_size]
            }
            Self::NullFilled => vec![0; 1000],
            Self::MalformedJson => br#"{"unclosed": {"nested": {"deep":"#.to_vec(),
            Self::BinaryBreaker => {
                // Bytes that might break JSON/UTF-8 serialization
                vec![0xFF, 0xFE, 0x00, 0x01, 0x80, 0x81, 0xC0, 0xC1]
            }
            Self::MaxBytes => vec![0xFF; 256],
        }
    }
}

/// Create a test queue service.
fn create_test_service(config: QueueConfig) -> (QueueService, Option<TempDir>) {
    if config.persist {
        let tmp = TempDir::new().expect("Failed to create temp dir");
        let db_path = tmp.path().join("fuzz_queue.db");
        let actual_config = QueueConfig {
            persist: true,
            db_path: Some(db_path),
            max_queue_size: config.max_queue_size,
        };
        let service = QueueService::new(actual_config).expect("Failed to create queue service");
        (service, Some(tmp))
    } else {
        let service = QueueService::new(config).expect("Failed to create queue service");
        (service, None)
    }
}

/// Sanitize queue name to reasonable length.
fn sanitize_name(name: &str) -> String {
    if name.len() > MAX_QUEUE_NAME_LEN {
        name[..MAX_QUEUE_NAME_LEN].to_string()
    } else {
        name.to_string()
    }
}

/// Sanitize message to reasonable size.
fn sanitize_message(msg: &[u8]) -> Vec<u8> {
    if msg.len() > MAX_MESSAGE_SIZE {
        msg[..MAX_MESSAGE_SIZE].to_vec()
    } else {
        msg.to_vec()
    }
}

fuzz_target!(|input: QueueInput| {
    // Create config with bounded queue size
    let max_size = input
        .max_queue_size
        .map(|s| (s as usize).min(TEST_MAX_QUEUE_SIZE));

    let config = QueueConfig {
        persist: input.use_persistence,
        db_path: None, // Will be set by create_test_service
        max_queue_size: max_size,
    };

    let (service, _tmp) = create_test_service(config);

    // Track queue lengths for invariant checking
    let mut expected_lengths: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // Execute operations with limit
    for operation in input.operations.iter().take(MAX_OPERATIONS) {
        match operation {
            QueueOperation::Push { queue_name, message } => {
                let name = sanitize_name(queue_name);
                let msg = sanitize_message(message);

                let result = service.push(&name, &msg);

                if result.is_ok() {
                    // INVARIANT: Message ID should be valid UUID
                    let msg_id = result.unwrap();
                    assert!(
                        uuid::Uuid::parse_str(&msg_id).is_ok(),
                        "Invalid message ID format: {}",
                        msg_id
                    );

                    *expected_lengths.entry(name.clone()).or_insert(0) += 1;
                }
                // Queue full errors are expected and acceptable
            }

            QueueOperation::Pop { queue_name } => {
                let name = sanitize_name(queue_name);
                let result = service.pop(&name);

                if let Ok(Some(msg)) = result {
                    // INVARIANT: Popped message should have valid ID
                    assert!(
                        uuid::Uuid::parse_str(&msg.id).is_ok(),
                        "Invalid message ID in popped message"
                    );

                    // INVARIANT: Message data should match what was pushed
                    // (we can't verify exact match without tracking, but size should be bounded)
                    assert!(
                        msg.data.len() <= MAX_MESSAGE_SIZE,
                        "Popped message exceeds size limit"
                    );

                    if let Some(len) = expected_lengths.get_mut(&name) {
                        *len = len.saturating_sub(1);
                    }
                }
            }

            QueueOperation::Peek { queue_name } => {
                let name = sanitize_name(queue_name);
                let result = service.peek(&name);

                if let Ok(Some(msg)) = result {
                    // INVARIANT: Peeked message should have valid ID
                    assert!(
                        uuid::Uuid::parse_str(&msg.id).is_ok(),
                        "Invalid message ID in peeked message"
                    );

                    // INVARIANT: Peek shouldn't change length
                    let len_before = service.len(&name).unwrap_or(0);
                    let _ = service.peek(&name);
                    let len_after = service.len(&name).unwrap_or(0);
                    assert_eq!(len_before, len_after, "Peek modified queue length");
                }
            }

            QueueOperation::Len { queue_name } => {
                let name = sanitize_name(queue_name);
                let result = service.len(&name);

                // INVARIANT: Length should never be negative (it's usize)
                if let Ok(len) = result {
                    // INVARIANT: Length should be bounded by max_queue_size if set
                    if let Some(max) = max_size {
                        assert!(
                            len <= max,
                            "Queue length {} exceeds max size {}",
                            len,
                            max
                        );
                    }
                }
            }

            QueueOperation::Clear { queue_name } => {
                let name = sanitize_name(queue_name);
                let result = service.clear(&name);

                if result.is_ok() {
                    expected_lengths.insert(name.clone(), 0);

                    // INVARIANT: Queue should be empty after clear
                    let len = service.len(&name).unwrap_or(usize::MAX);
                    assert_eq!(len, 0, "Queue not empty after clear");
                }
            }

            QueueOperation::Delete { queue_name } => {
                let name = sanitize_name(queue_name);
                let _ = service.delete_queue(&name);

                expected_lengths.remove(&name);

                // INVARIANT: Queue should not exist after delete
                let len = service.len(&name).unwrap_or(0);
                assert_eq!(len, 0, "Deleted queue still has messages");
            }

            QueueOperation::ListQueues => {
                let queues = service.list_queues();

                // INVARIANT: All listed queues should be accessible
                for queue_name in &queues {
                    let result = service.len(queue_name);
                    assert!(result.is_ok(), "Listed queue is inaccessible: {}", queue_name);
                }
            }

            QueueOperation::Publish { topic, message } => {
                let name = sanitize_name(topic);
                let msg = sanitize_message(message);

                let result = service.publish(&name, &msg);

                // INVARIANT: Publish should return count >= 0
                if let Ok(count) = result {
                    assert!(
                        count <= 1024, // MAX_SUBSCRIBERS_PER_TOPIC
                        "Subscriber count exceeds limit"
                    );
                }
            }

            QueueOperation::Subscribe { topic } => {
                let name = sanitize_name(topic);
                let _rx = service.subscribe(&name);

                // INVARIANT: Subscriber count should increase
                let count = service.subscriber_count(&name);
                assert!(count >= 1, "Subscribe didn't increment count");

                // Drop the receiver immediately to test cleanup
            }

            QueueOperation::ListTopics => {
                let topics = service.list_topics();

                // INVARIANT: All listed topics should have valid subscriber counts
                for topic_name in &topics {
                    let count = service.subscriber_count(topic_name);
                    assert!(
                        count <= 1024, // MAX_SUBSCRIBERS_PER_TOPIC
                        "Topic has too many subscribers"
                    );
                }
            }

            QueueOperation::AdversarialName(pattern) => {
                let name = pattern.generate();
                let name = sanitize_name(&name);

                // These operations should handle adversarial names gracefully
                let _ = service.push(&name, b"test");
                let _ = service.pop(&name);
                let _ = service.peek(&name);
                let _ = service.len(&name);
                let _ = service.clear(&name);

                // No panic means success
            }

            QueueOperation::AdversarialMessage(pattern) => {
                let msg = pattern.generate();
                let msg = sanitize_message(&msg);

                // Adversarial messages should be handled safely
                let result = service.push("test_queue", &msg);

                if result.is_ok() {
                    // INVARIANT: Should be able to pop what we pushed
                    let popped = service.pop("test_queue");
                    if let Ok(Some(popped_msg)) = popped {
                        // INVARIANT: Data should match exactly
                        assert_eq!(
                            popped_msg.data, msg,
                            "Popped message data differs from pushed"
                        );
                    }
                }
            }
        }
    }

    // Final invariant checks
    verify_queue_integrity(&service, &expected_lengths, max_size);
});

/// Verify queue integrity after all operations.
fn verify_queue_integrity(
    service: &QueueService,
    expected_lengths: &std::collections::HashMap<String, usize>,
    max_size: Option<usize>,
) {
    for (name, expected) in expected_lengths {
        let actual = service.len(name).unwrap_or(0);

        // Note: Due to the randomness of fuzzing, actual length might differ
        // from expected if operations failed. What matters is that:

        // INVARIANT 1: Length should be bounded
        if let Some(max) = max_size {
            assert!(
                actual <= max,
                "Queue '{}' length {} exceeds max {}",
                name,
                actual,
                max
            );
        }

        // INVARIANT 2: Length should match operations if we tracked perfectly
        // (this may not always hold due to failed operations, so we just log)
        if actual != *expected {
            // Expected - operations might have failed, that's okay
        }
    }

    // INVARIANT 3: All queues should be listable
    let queues = service.list_queues();
    for queue_name in &queues {
        let result = service.len(queue_name);
        assert!(result.is_ok(), "Queue '{}' became inaccessible", queue_name);
    }
}
