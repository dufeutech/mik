//! W3C Trace Context (Phase 3) - Standard distributed tracing headers.
//!
//! Implements [W3C Trace Context](https://www.w3.org/TR/trace-context/) for
//! interoperability with `OpenTelemetry`, `Jaeger`, `Zipkin`, and other tracing systems.
//!
//! # Header Format
//!
//! ```text
//! traceparent: {version}-{trace_id}-{parent_id}-{flags}
//!              00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01
//! ```
//!
//! - version: 2 hex chars (always "00" for current spec)
//! - `trace_id`: 32 hex chars (128-bit)
//! - `parent_id`: 16 hex chars (64-bit span ID)
//! - flags: 2 hex chars (01 = sampled)

// Allow dead code - this module provides W3C Trace Context APIs that are tested
// but not yet fully integrated into the main code path (Phase 3 tracing).
#![allow(dead_code)]

use std::fmt;

/// W3C Trace Context parsed from headers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct TraceContext {
    /// 128-bit trace ID (32 hex chars)
    pub trace_id: String,
    /// 64-bit parent span ID (16 hex chars)
    pub parent_id: String,
    /// Trace flags (sampled = 0x01)
    pub flags: TraceFlags,
    /// Optional tracestate for vendor-specific data
    pub tracestate: Option<String>,
}

/// Trace flags from W3C spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraceFlags(u8);

impl TraceFlags {
    /// No flags set
    pub const NONE: Self = Self(0x00);
    /// Trace is sampled (will be recorded)
    pub const SAMPLED: Self = Self(0x01);

    /// Check if sampled flag is set
    pub const fn is_sampled(self) -> bool {
        self.0 & 0x01 != 0
    }

    /// Create from raw byte
    pub const fn from_byte(b: u8) -> Self {
        Self(b)
    }

    /// Get raw byte value
    pub const fn as_byte(self) -> u8 {
        self.0
    }
}

impl fmt::Display for TraceFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}", self.0)
    }
}

/// Error parsing trace context headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceContextError {
    /// Header value is malformed
    InvalidFormat,
    /// Unsupported version (only 00 is supported)
    UnsupportedVersion,
    /// `trace_id` is all zeros (invalid)
    InvalidTraceId,
    /// `parent_id` is all zeros (invalid)
    InvalidParentId,
}

impl fmt::Display for TraceContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "invalid traceparent format"),
            Self::UnsupportedVersion => write!(f, "unsupported traceparent version"),
            Self::InvalidTraceId => write!(f, "invalid trace_id (all zeros)"),
            Self::InvalidParentId => write!(f, "invalid parent_id (all zeros)"),
        }
    }
}

impl std::error::Error for TraceContextError {}

impl TraceContext {
    /// Parse from traceparent header value.
    ///
    /// Format: `{version}-{trace_id}-{parent_id}-{flags}`
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")?;
    /// assert_eq!(ctx.trace_id, "0af7651916cd43dd8448eb211c80319c");
    /// assert!(ctx.flags.is_sampled());
    /// ```
    pub fn parse(traceparent: &str) -> Result<Self, TraceContextError> {
        let parts: Vec<&str> = traceparent.trim().split('-').collect();

        // Must have exactly 4 parts
        if parts.len() != 4 {
            return Err(TraceContextError::InvalidFormat);
        }

        let version = parts[0];
        let trace_id = parts[1];
        let parent_id = parts[2];
        let flags = parts[3];

        // Validate lengths
        if version.len() != 2 || trace_id.len() != 32 || parent_id.len() != 16 || flags.len() != 2 {
            return Err(TraceContextError::InvalidFormat);
        }

        // Only version 00 is supported
        if version != "00" {
            return Err(TraceContextError::UnsupportedVersion);
        }

        // Validate hex characters
        if !trace_id.chars().all(|c| c.is_ascii_hexdigit())
            || !parent_id.chars().all(|c| c.is_ascii_hexdigit())
            || !flags.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(TraceContextError::InvalidFormat);
        }

        // trace_id must not be all zeros
        if trace_id.chars().all(|c| c == '0') {
            return Err(TraceContextError::InvalidTraceId);
        }

        // parent_id must not be all zeros
        if parent_id.chars().all(|c| c == '0') {
            return Err(TraceContextError::InvalidParentId);
        }

        // Parse flags
        let flags_byte =
            u8::from_str_radix(flags, 16).map_err(|_| TraceContextError::InvalidFormat)?;

        Ok(Self {
            trace_id: trace_id.to_lowercase(),
            parent_id: parent_id.to_lowercase(),
            flags: TraceFlags::from_byte(flags_byte),
            tracestate: None,
        })
    }

    /// Create a new trace context with generated IDs.
    ///
    /// Uses the provided `trace_id` and generates a new `span_id`.
    pub fn new(trace_id: impl Into<String>) -> Self {
        Self {
            trace_id: trace_id.into(),
            parent_id: generate_span_id(),
            flags: TraceFlags::SAMPLED,
            tracestate: None,
        }
    }

    /// Create a child context (same `trace_id`, new `parent_id`).
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            parent_id: generate_span_id(),
            flags: self.flags,
            tracestate: self.tracestate.clone(),
        }
    }

    /// Set the tracestate header value.
    pub fn with_tracestate(mut self, tracestate: impl Into<String>) -> Self {
        self.tracestate = Some(tracestate.into());
        self
    }

    /// Generate the traceparent header value.
    pub fn to_traceparent(&self) -> String {
        format!("00-{}-{}-{}", self.trace_id, self.parent_id, self.flags)
    }
}

/// Generate a random 64-bit span ID (16 hex chars).
fn generate_span_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    // Mix timestamp + counter for uniqueness without external deps
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);

    // Simple mixing: XOR timestamp with counter, add some bit rotation
    let mixed = ts ^ count.rotate_left(32);
    format!("{mixed:016x}")
}

/// Generate a random 128-bit trace ID (32 hex chars).
pub fn generate_trace_id() -> String {
    // Use UUID v4 which is already a dependency
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_traceparent() {
        let ctx =
            TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").unwrap();

        assert_eq!(ctx.trace_id, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(ctx.parent_id, "b7ad6b7169203331");
        assert!(ctx.flags.is_sampled());
    }

    #[test]
    fn test_parse_not_sampled() {
        let ctx =
            TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00").unwrap();

        assert!(!ctx.flags.is_sampled());
    }

    #[test]
    fn test_parse_uppercase() {
        let ctx =
            TraceContext::parse("00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01").unwrap();

        // Should normalize to lowercase
        assert_eq!(ctx.trace_id, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(ctx.parent_id, "b7ad6b7169203331");
    }

    #[test]
    fn test_parse_invalid_version() {
        let result = TraceContext::parse("01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01");
        assert_eq!(result, Err(TraceContextError::UnsupportedVersion));
    }

    #[test]
    fn test_parse_invalid_format_too_few_parts() {
        let result = TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331");
        assert_eq!(result, Err(TraceContextError::InvalidFormat));
    }

    #[test]
    fn test_parse_invalid_trace_id_length() {
        let result = TraceContext::parse("00-0af765-b7ad6b7169203331-01");
        assert_eq!(result, Err(TraceContextError::InvalidFormat));
    }

    #[test]
    fn test_parse_all_zero_trace_id() {
        let result = TraceContext::parse("00-00000000000000000000000000000000-b7ad6b7169203331-01");
        assert_eq!(result, Err(TraceContextError::InvalidTraceId));
    }

    #[test]
    fn test_parse_all_zero_parent_id() {
        let result = TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01");
        assert_eq!(result, Err(TraceContextError::InvalidParentId));
    }

    #[test]
    fn test_parse_invalid_hex() {
        let result = TraceContext::parse("00-0af7651916cd43dd8448eb211c80319g-b7ad6b7169203331-01");
        assert_eq!(result, Err(TraceContextError::InvalidFormat));
    }

    #[test]
    fn test_to_traceparent() {
        let ctx =
            TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").unwrap();

        assert_eq!(
            ctx.to_traceparent(),
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        );
    }

    #[test]
    fn test_child_context() {
        let parent =
            TraceContext::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").unwrap();
        let child = parent.child();

        // Same trace_id
        assert_eq!(child.trace_id, parent.trace_id);
        // Different parent_id
        assert_ne!(child.parent_id, parent.parent_id);
        // Same flags
        assert_eq!(child.flags, parent.flags);
    }

    #[test]
    fn test_new_context() {
        let ctx = TraceContext::new("0af7651916cd43dd8448eb211c80319c");

        assert_eq!(ctx.trace_id, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(ctx.parent_id.len(), 16);
        assert!(ctx.flags.is_sampled());
    }

    #[test]
    fn test_generate_trace_id() {
        let id1 = generate_trace_id();
        let id2 = generate_trace_id();

        assert_eq!(id1.len(), 32);
        assert_eq!(id2.len(), 32);
        assert_ne!(id1, id2);
        assert!(id1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_span_id_unique() {
        let id1 = generate_span_id();
        let id2 = generate_span_id();

        assert_eq!(id1.len(), 16);
        assert_eq!(id2.len(), 16);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_with_tracestate() {
        let ctx =
            TraceContext::new("0af7651916cd43dd8448eb211c80319c").with_tracestate("vendor=value");

        assert_eq!(ctx.tracestate, Some("vendor=value".to_string()));
    }

    #[test]
    fn test_trace_flags_display() {
        assert_eq!(format!("{}", TraceFlags::NONE), "00");
        assert_eq!(format!("{}", TraceFlags::SAMPLED), "01");
        assert_eq!(format!("{}", TraceFlags::from_byte(0xff)), "ff");
    }
}
