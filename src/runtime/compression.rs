//! Response compression utilities.
//!
//! Provides gzip compression for HTTP responses when beneficial.
//! Compression is applied based on content type and size thresholds.

use crate::constants::GZIP_MIN_SIZE;
use flate2::Compression;
use flate2::write::GzEncoder;
use http_body_util::Full;
use hyper::body::{Body, Bytes};
use hyper::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Request, Response};
use std::io::Write;
use std::task::{Context, Poll};
use tracing::debug;

/// Check if a content type is compressible (text-based or JSON/XML).
pub fn is_compressible_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript")
        || content_type == "image/svg+xml"
}

/// Check if the client accepts gzip encoding.
pub fn accepts_gzip<B>(req: &Request<B>) -> bool {
    req.headers()
        .get(ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("gzip"))
}

/// Compress body bytes with gzip if appropriate.
///
/// Applies gzip compression when:
/// - Body is larger than `GZIP_MIN_SIZE` (1024 bytes)
/// - Content type is compressible (text, JSON, XML, etc.)
/// - Client accepts gzip encoding
///
/// Returns the original bytes unchanged if compression is not beneficial,
/// along with optional Content-Encoding header value.
pub fn maybe_compress_body(
    body: Bytes,
    content_type: &str,
    client_accepts_gzip: bool,
) -> (Bytes, Option<&'static str>) {
    // Skip if client doesn't accept gzip
    if !client_accepts_gzip {
        return (body, None);
    }

    // Skip small responses (compression overhead not worth it)
    if body.len() < GZIP_MIN_SIZE {
        return (body, None);
    }

    // Check content type for compressibility
    if !is_compressible_content_type(content_type) {
        return (body, None);
    }

    // Compress with gzip (pre-allocate estimated compressed size)
    let mut encoder = GzEncoder::new(Vec::with_capacity(body.len()), Compression::fast());
    if encoder.write_all(&body).is_err() {
        return (body, None);
    }
    let Ok(compressed) = encoder.finish() else {
        return (body, None);
    };

    // Only use compressed version if it's smaller
    if compressed.len() >= body.len() {
        return (body, None);
    }

    debug!(
        "Gzip compressed response: {} -> {} bytes ({:.1}% reduction)",
        body.len(),
        compressed.len(),
        (1.0 - compressed.len() as f64 / body.len() as f64) * 100.0
    );

    (Bytes::from(compressed), Some("gzip"))
}

/// Apply gzip compression to a response if appropriate.
///
/// This is a convenience wrapper that extracts body bytes from a response,
/// compresses them, and rebuilds the response.
pub fn maybe_compress_response(
    response: Response<Full<Bytes>>,
    client_accepts_gzip: bool,
) -> Response<Full<Bytes>> {
    // Skip if client doesn't accept gzip
    if !client_accepts_gzip {
        return response;
    }

    // For Full<Bytes>, we can't easily extract the bytes without consuming.
    // Instead, we'll collect the body using the BodyExt trait.
    // Since this is synchronous code and Full<Bytes> is a simple single-frame body,
    // we use a blocking approach via poll.
    let (parts, body) = response.into_parts();

    // Get content type for compression decision
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Check body size hint first
    let size_hint = body.size_hint();
    let body_size = size_hint.exact().unwrap_or_else(|| size_hint.lower()) as usize;

    if body_size < GZIP_MIN_SIZE {
        return Response::from_parts(parts, body);
    }

    if !is_compressible_content_type(content_type) {
        return Response::from_parts(parts, body);
    }

    // Poll the single frame from Full<Bytes>
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut pinned = std::pin::pin!(body);

    let body_bytes = match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
            Ok(data) => data,
            Err(_) => return Response::from_parts(parts, Full::new(Bytes::new())),
        },
        _ => return Response::from_parts(parts, Full::new(Bytes::new())),
    };

    // Compress
    let (compressed_bytes, encoding) = maybe_compress_body(body_bytes.clone(), content_type, true);

    if encoding.is_none() {
        // Compression didn't help, return original
        return Response::from_parts(parts, Full::new(body_bytes));
    }

    // Build compressed response
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in &parts.headers {
        // Skip Content-Length (will be recalculated)
        if name != CONTENT_LENGTH {
            builder = builder.header(name, value);
        }
    }
    builder = builder.header(CONTENT_ENCODING, "gzip");
    builder = builder.header(CONTENT_LENGTH, compressed_bytes.len());

    builder.body(Full::new(compressed_bytes)).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to build compressed response, falling back to uncompressed");
        Response::from_parts(parts, Full::new(body_bytes))
    })
}
