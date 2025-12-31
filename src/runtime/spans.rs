//! Timing spans for request tracing (Phase 2).
//!
//! Provides lightweight span tracking for measuring execution time
//! of requests, scripts, and handler calls with parent-child relationships.
//!
//! # Span Hierarchy
//!
//! ```text
//! request (root)
//! └── script.checkout
//!     ├── handler.auth
//!     └── handler.orders
//! ```

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

/// Global counter for generating unique span IDs within a process.
static SPAN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate a unique span ID (process-local, not globally unique).
fn generate_span_id() -> String {
    let id = SPAN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{id:016x}")
}

/// Status of a completed span.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum SpanStatus {
    /// Operation completed successfully
    Ok,
    /// Operation failed with an error
    Error,
    /// Operation timed out
    Timeout,
}

impl fmt::Display for SpanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Timeout => "timeout",
        };
        write!(f, "{s}")
    }
}

/// A timing span that tracks the duration of an operation.
#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct Span {
    /// Unique identifier for this span
    pub span_id: String,
    /// Parent span ID (None for root spans)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Name of the span (e.g., "request", "script.checkout", "handler.auth")
    pub name: String,
    /// Duration in milliseconds (set when span completes)
    pub duration_ms: u64,
    /// Status of the operation
    pub status: SpanStatus,
    /// Optional error message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Builder for creating and completing spans.
///
/// Use `SpanBuilder::new()` to start timing, then `.finish()` to complete.
///
/// # Example
///
/// ```ignore
/// // Root span (no parent)
/// let request_span = SpanBuilder::new("request");
/// let request_id = request_span.span_id().to_string();
///
/// // Child span
/// let handler_span = SpanBuilder::with_parent("handler.auth", &request_id);
/// collector.add(handler_span.finish());
///
/// collector.add(request_span.finish());
/// ```
#[derive(Debug, Clone)]
pub struct SpanBuilder {
    span_id: String,
    parent_id: Option<String>,
    name: String,
    start: Instant,
}

impl SpanBuilder {
    /// Start a new root span with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            span_id: generate_span_id(),
            parent_id: None,
            name: name.into(),
            start: Instant::now(),
        }
    }

    /// Start a new child span with the given name and parent ID.
    pub fn with_parent(name: impl Into<String>, parent_id: impl Into<String>) -> Self {
        Self {
            span_id: generate_span_id(),
            parent_id: Some(parent_id.into()),
            name: name.into(),
            start: Instant::now(),
        }
    }

    /// Get the span ID (useful for creating child spans).
    pub fn span_id(&self) -> &str {
        &self.span_id
    }

    /// Complete the span with success status.
    pub fn finish(self) -> Span {
        Span {
            span_id: self.span_id,
            parent_id: self.parent_id,
            name: self.name,
            duration_ms: self.start.elapsed().as_millis() as u64,
            status: SpanStatus::Ok,
            error: None,
        }
    }

    /// Complete the span with error status.
    pub fn finish_with_error(self, error: impl Into<String>) -> Span {
        Span {
            span_id: self.span_id,
            parent_id: self.parent_id,
            name: self.name,
            duration_ms: self.start.elapsed().as_millis() as u64,
            status: SpanStatus::Error,
            error: Some(error.into()),
        }
    }

    /// Complete the span with timeout status.
    #[allow(dead_code)]
    pub fn finish_timeout(self) -> Span {
        Span {
            span_id: self.span_id,
            parent_id: self.parent_id,
            name: self.name,
            duration_ms: self.start.elapsed().as_millis() as u64,
            status: SpanStatus::Timeout,
            error: Some("Operation timed out".to_string()),
        }
    }

    /// Get elapsed time so far (without completing the span).
    #[allow(dead_code)]
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Collector for gathering spans during a request.
///
/// Thread-safe and clonable for use across async boundaries.
#[derive(Debug, Clone, Default)]
pub struct SpanCollector {
    spans: Arc<Mutex<Vec<Span>>>,
}

impl SpanCollector {
    /// Create a new empty collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a completed span to the collector.
    pub fn add(&self, span: Span) {
        if let Ok(mut spans) = self.spans.lock() {
            spans.push(span);
        }
    }

    /// Get all collected spans.
    pub fn collect(self) -> Vec<Span> {
        Arc::try_unwrap(self.spans).map_or_else(
            |arc| arc.lock().map(|g| g.clone()).unwrap_or_default(),
            |m| m.into_inner().unwrap_or_default(),
        )
    }

    /// Get the number of spans collected.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.spans.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Check if no spans have been collected.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Summary of all spans for a request.
#[derive(Debug, Serialize)]
pub struct SpanSummary {
    /// Trace ID for correlating all spans in this request
    pub trace_id: String,
    /// Total request duration in milliseconds
    pub total_ms: u64,
    /// Number of handler calls made
    pub handler_calls: usize,
    /// Individual spans (ordered by completion time)
    pub spans: Vec<Span>,
}

impl SpanSummary {
    /// Create a summary from collected spans, trace ID, and total duration.
    pub fn new(trace_id: impl Into<String>, total_ms: u64, spans: Vec<Span>) -> Self {
        let handler_calls = spans
            .iter()
            .filter(|s| s.name.starts_with("handler."))
            .count();

        Self {
            trace_id: trace_id.into(),
            total_ms,
            handler_calls,
            spans,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_span_builder_finish() {
        let builder = SpanBuilder::new("test");
        thread::sleep(Duration::from_millis(10));
        let span = builder.finish();

        assert_eq!(span.name, "test");
        assert!(span.duration_ms >= 10);
        assert_eq!(span.status, SpanStatus::Ok);
        assert!(span.error.is_none());
        assert!(!span.span_id.is_empty());
        assert!(span.parent_id.is_none());
    }

    #[test]
    fn test_span_builder_with_parent() {
        let parent = SpanBuilder::new("parent");
        let parent_id = parent.span_id().to_string();

        let child = SpanBuilder::with_parent("child", &parent_id);
        let child_span = child.finish();

        assert_eq!(child_span.name, "child");
        assert_eq!(child_span.parent_id, Some(parent_id.clone()));
        assert_ne!(child_span.span_id, parent_id);
    }

    #[test]
    fn test_span_builder_finish_with_error() {
        let builder = SpanBuilder::new("test");
        let span = builder.finish_with_error("something failed");

        assert_eq!(span.status, SpanStatus::Error);
        assert_eq!(span.error, Some("something failed".to_string()));
        assert!(!span.span_id.is_empty());
    }

    #[test]
    fn test_span_builder_finish_timeout() {
        let builder = SpanBuilder::new("test");
        let span = builder.finish_timeout();

        assert_eq!(span.status, SpanStatus::Timeout);
        assert!(span.error.is_some());
        assert!(!span.span_id.is_empty());
    }

    #[test]
    fn test_span_id_uniqueness() {
        let span1 = SpanBuilder::new("span1");
        let span2 = SpanBuilder::new("span2");
        let span3 = SpanBuilder::new("span3");

        assert_ne!(span1.span_id(), span2.span_id());
        assert_ne!(span2.span_id(), span3.span_id());
        assert_ne!(span1.span_id(), span3.span_id());
    }

    #[test]
    fn test_span_collector() {
        let collector = SpanCollector::new();
        let parent = SpanBuilder::new("request");
        let parent_id = parent.span_id().to_string();

        collector.add(Span {
            span_id: "0000000000000001".to_string(),
            parent_id: None,
            name: "span1".to_string(),
            duration_ms: 10,
            status: SpanStatus::Ok,
            error: None,
        });

        collector.add(Span {
            span_id: "0000000000000002".to_string(),
            parent_id: Some(parent_id),
            name: "span2".to_string(),
            duration_ms: 20,
            status: SpanStatus::Ok,
            error: None,
        });

        assert_eq!(collector.len(), 2);

        let spans = collector.collect();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "span1");
        assert_eq!(spans[1].name, "span2");
        assert!(spans[1].parent_id.is_some());
    }

    #[test]
    fn test_span_summary() {
        let spans = vec![
            Span {
                span_id: "0000000000000001".to_string(),
                parent_id: None,
                name: "request".to_string(),
                duration_ms: 100,
                status: SpanStatus::Ok,
                error: None,
            },
            Span {
                span_id: "0000000000000002".to_string(),
                parent_id: Some("0000000000000001".to_string()),
                name: "handler.auth".to_string(),
                duration_ms: 20,
                status: SpanStatus::Ok,
                error: None,
            },
            Span {
                span_id: "0000000000000003".to_string(),
                parent_id: Some("0000000000000001".to_string()),
                name: "handler.orders".to_string(),
                duration_ms: 50,
                status: SpanStatus::Ok,
                error: None,
            },
        ];

        let summary = SpanSummary::new("trace-abc123", 100, spans);
        assert_eq!(summary.trace_id, "trace-abc123");
        assert_eq!(summary.total_ms, 100);
        assert_eq!(summary.handler_calls, 2);
        assert_eq!(summary.spans.len(), 3);
    }

    #[test]
    fn test_span_serialization() {
        let span = Span {
            span_id: "0000000000000042".to_string(),
            parent_id: None,
            name: "test".to_string(),
            duration_ms: 42,
            status: SpanStatus::Ok,
            error: None,
        };

        let json = serde_json::to_value(&span).unwrap();
        assert_eq!(json["span_id"], "0000000000000042");
        assert_eq!(json["name"], "test");
        assert_eq!(json["duration_ms"], 42);
        assert_eq!(json["status"], "ok");
        // parent_id and error should not be present when None
        assert!(json.get("parent_id").is_none());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn test_span_serialization_with_parent() {
        let span = Span {
            span_id: "0000000000000002".to_string(),
            parent_id: Some("0000000000000001".to_string()),
            name: "child".to_string(),
            duration_ms: 10,
            status: SpanStatus::Ok,
            error: None,
        };

        let json = serde_json::to_value(&span).unwrap();
        assert_eq!(json["span_id"], "0000000000000002");
        assert_eq!(json["parent_id"], "0000000000000001");
        assert_eq!(json["name"], "child");
    }

    #[test]
    fn test_summary_serialization() {
        let spans = vec![Span {
            span_id: "0000000000000001".to_string(),
            parent_id: None,
            name: "request".to_string(),
            duration_ms: 100,
            status: SpanStatus::Ok,
            error: None,
        }];

        let summary = SpanSummary::new("abc123", 100, spans);
        let json = serde_json::to_value(&summary).unwrap();

        assert_eq!(json["trace_id"], "abc123");
        assert_eq!(json["total_ms"], 100);
        assert_eq!(json["handler_calls"], 0);
        assert_eq!(json["spans"].as_array().unwrap().len(), 1);
    }
}
