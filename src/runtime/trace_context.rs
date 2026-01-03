//! W3C Trace Context support (https://www.w3.org/TR/trace-context/).
//!
//! Implements parsing and generation of W3C Trace Context headers:
//! - `traceparent`: Primary header for trace propagation
//! - `tracestate`: Optional vendor-specific trace information
//!
//! # Format
//!
//! ```text
//! traceparent: {version}-{trace-id}-{parent-id}-{trace-flags}
//!
//! Example: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
//!
//! version:     2 hex chars (always "00")
//! trace-id:    32 hex chars (128-bit, lowercase)
//! parent-id:   16 hex chars (64-bit, lowercase)
//! trace-flags: 2 hex chars (01 = sampled)
//! ```

use std::fmt;
use uuid::Uuid;

/// W3C Trace Context version (currently only "00" is defined).
const TRACE_CONTEXT_VERSION: &str = "00";

/// Trace flags: sampled (bit 0 set).
const TRACE_FLAG_SAMPLED: u8 = 0x01;

/// A parsed W3C Trace Context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    /// The trace ID (128-bit, 32 hex chars).
    pub trace_id: String,
    /// The parent span ID (64-bit, 16 hex chars).
    pub parent_id: String,
    /// Whether this trace is sampled.
    pub sampled: bool,
}

impl TraceContext {
    /// Create a new trace context with random trace ID and parent ID.
    ///
    /// The trace is marked as sampled by default.
    pub fn new() -> Self {
        Self {
            trace_id: generate_trace_id(),
            parent_id: generate_span_id(),
            sampled: true,
        }
    }

    /// Create a new span under this trace context.
    ///
    /// Returns a new `TraceContext` with the same trace ID but a new span ID.
    #[must_use]
    pub fn new_span(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            parent_id: generate_span_id(),
            sampled: self.sampled,
        }
    }

    /// Parse a `traceparent` header value.
    ///
    /// Returns `None` if the header is invalid or malformed.
    ///
    /// # Format
    ///
    /// ```text
    /// {version}-{trace-id}-{parent-id}-{trace-flags}
    /// 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
    /// ```
    pub fn parse(traceparent: &str) -> Option<Self> {
        let parts: Vec<&str> = traceparent.split('-').collect();

        // Must have exactly 4 parts
        if parts.len() != 4 {
            return None;
        }

        let version = parts[0];
        let trace_id = parts[1];
        let parent_id = parts[2];
        let trace_flags = parts[3];

        // Validate version (must be "00" for now)
        if version != TRACE_CONTEXT_VERSION {
            // Per spec: if version is unknown, try to parse anyway
            // but for simplicity, we only support version 00
            return None;
        }

        // Validate trace_id (32 hex chars, not all zeros)
        if trace_id.len() != 32 || !is_valid_hex(trace_id) || is_all_zeros(trace_id) {
            return None;
        }

        // Validate parent_id (16 hex chars, not all zeros)
        if parent_id.len() != 16 || !is_valid_hex(parent_id) || is_all_zeros(parent_id) {
            return None;
        }

        // Validate trace_flags (2 hex chars)
        if trace_flags.len() != 2 || !is_valid_hex(trace_flags) {
            return None;
        }

        // Parse trace flags
        let flags = u8::from_str_radix(trace_flags, 16).ok()?;
        let sampled = (flags & TRACE_FLAG_SAMPLED) != 0;

        Some(Self {
            trace_id: trace_id.to_lowercase(),
            parent_id: parent_id.to_lowercase(),
            sampled,
        })
    }

    /// Format as a `traceparent` header value.
    pub fn to_traceparent(&self) -> String {
        let flags = if self.sampled { TRACE_FLAG_SAMPLED } else { 0 };
        format!(
            "{}-{}-{}-{:02x}",
            TRACE_CONTEXT_VERSION, self.trace_id, self.parent_id, flags
        )
    }

    /// Get just the trace ID for logging/correlation.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Get just the span/parent ID.
    pub fn span_id(&self) -> &str {
        &self.parent_id
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_traceparent())
    }
}

/// Generate a random 128-bit trace ID (32 hex chars).
fn generate_trace_id() -> String {
    // Use UUID v4 random bytes for trace ID (128-bit)
    let uuid = Uuid::new_v4();
    uuid.simple().to_string()
}

/// Generate a random 64-bit span ID (16 hex chars).
fn generate_span_id() -> String {
    // Use first 8 bytes of UUID v4 for span ID (64-bit)
    let uuid = Uuid::new_v4();
    let bytes = uuid.as_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]
    )
}

/// Check if a string contains only valid hex characters (lowercase).
fn is_valid_hex(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Check if a string is all zeros.
fn is_all_zeros(s: &str) -> bool {
    s.chars().all(|c| c == '0')
}

/// Extract trace context from request headers.
///
/// Looks for the `traceparent` header. If not present or invalid,
/// generates a new trace context.
pub fn extract_trace_context(headers: &hyper::HeaderMap) -> TraceContext {
    // Try to parse W3C traceparent header
    if let Some(traceparent) = headers.get("traceparent")
        && let Ok(value) = traceparent.to_str()
        && let Some(ctx) = TraceContext::parse(value)
    {
        return ctx;
    }

    // No valid trace context found, generate new
    TraceContext::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_context_new() {
        let ctx = TraceContext::new();

        assert_eq!(ctx.trace_id.len(), 32);
        assert_eq!(ctx.parent_id.len(), 16);
        assert!(ctx.sampled);
        assert!(is_valid_hex(&ctx.trace_id));
        assert!(is_valid_hex(&ctx.parent_id));
    }

    #[test]
    fn test_trace_context_parse_valid() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let ctx = TraceContext::parse(traceparent).expect("should parse");

        assert_eq!(ctx.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(ctx.parent_id, "00f067aa0ba902b7");
        assert!(ctx.sampled);
    }

    #[test]
    fn test_trace_context_parse_not_sampled() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00";
        let ctx = TraceContext::parse(traceparent).expect("should parse");

        assert!(!ctx.sampled);
    }

    #[test]
    fn test_trace_context_parse_invalid_version() {
        let traceparent = "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        assert!(TraceContext::parse(traceparent).is_none());
    }

    #[test]
    fn test_trace_context_parse_invalid_trace_id_length() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e47-00f067aa0ba902b7-01"; // 31 chars
        assert!(TraceContext::parse(traceparent).is_none());
    }

    #[test]
    fn test_trace_context_parse_invalid_all_zeros_trace_id() {
        let traceparent = "00-00000000000000000000000000000000-00f067aa0ba902b7-01";
        assert!(TraceContext::parse(traceparent).is_none());
    }

    #[test]
    fn test_trace_context_parse_invalid_all_zeros_parent_id() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01";
        assert!(TraceContext::parse(traceparent).is_none());
    }

    #[test]
    fn test_trace_context_parse_invalid_hex() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e473g-00f067aa0ba902b7-01"; // 'g' invalid
        assert!(TraceContext::parse(traceparent).is_none());
    }

    #[test]
    fn test_trace_context_to_traceparent() {
        let ctx = TraceContext {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
            parent_id: "00f067aa0ba902b7".to_string(),
            sampled: true,
        };

        assert_eq!(
            ctx.to_traceparent(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
    }

    #[test]
    fn test_trace_context_to_traceparent_not_sampled() {
        let ctx = TraceContext {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
            parent_id: "00f067aa0ba902b7".to_string(),
            sampled: false,
        };

        assert_eq!(
            ctx.to_traceparent(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00"
        );
    }

    #[test]
    fn test_trace_context_roundtrip() {
        let original = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let ctx = TraceContext::parse(original).expect("should parse");
        assert_eq!(ctx.to_traceparent(), original);
    }

    #[test]
    fn test_trace_context_new_span() {
        let ctx = TraceContext::new();
        let child = ctx.new_span();

        assert_eq!(child.trace_id, ctx.trace_id);
        assert_ne!(child.parent_id, ctx.parent_id);
        assert_eq!(child.sampled, ctx.sampled);
    }

    #[test]
    fn test_generate_trace_id_format() {
        let id = generate_trace_id();
        assert_eq!(id.len(), 32);
        assert!(is_valid_hex(&id));
        assert!(!is_all_zeros(&id));
    }

    #[test]
    fn test_generate_span_id_format() {
        let id = generate_span_id();
        assert_eq!(id.len(), 16);
        assert!(is_valid_hex(&id));
    }

    #[test]
    fn test_display() {
        let ctx = TraceContext {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
            parent_id: "00f067aa0ba902b7".to_string(),
            sampled: true,
        };

        assert_eq!(
            format!("{ctx}"),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
    }

    #[test]
    fn test_parse_uppercase_is_normalized() {
        let traceparent = "00-4BF92F3577B34DA6A3CE929D0E0E4736-00F067AA0BA902B7-01";
        let ctx = TraceContext::parse(traceparent).expect("should parse");

        // Should be normalized to lowercase
        assert_eq!(ctx.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(ctx.parent_id, "00f067aa0ba902b7");
    }
}
