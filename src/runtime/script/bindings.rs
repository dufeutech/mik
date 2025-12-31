//! Host bindings for `JavaScript` scripts.
//!
//! Provides the `host.call()` bridge between synchronous `JavaScript` and async Rust.

use std::cell::RefCell;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::types::HostMessage;

/// Bridge for sync JS -> async Rust communication
pub(crate) struct HostBridge {
    pub tx: mpsc::UnboundedSender<HostMessage>,
}

// Thread-local bridge (same pattern as SQL_BRIDGE in mikcar)
thread_local! {
    pub(super) static HOST_BRIDGE: RefCell<Option<Arc<HostBridge>>> = const { RefCell::new(None) };
}

/// RAII guard that clears the thread-local bridge on drop, even if a panic occurs.
pub(crate) struct HostBridgeGuard;

impl HostBridgeGuard {
    /// Set the thread-local bridge and return a guard that will clear it on drop.
    pub fn set(bridge: Arc<HostBridge>) -> Self {
        HOST_BRIDGE.with(|cell| {
            *cell.borrow_mut() = Some(bridge);
        });
        Self
    }
}

impl Drop for HostBridgeGuard {
    fn drop(&mut self) {
        HOST_BRIDGE.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }
}

/// Native `host_call` function - takes module and options JSON, returns response JSON.
///
/// # Note on `needless_pass_by_value`
/// rquickjs requires owned `String` values for JS function bindings because the FFI
/// boundary transfers ownership from the JS runtime to Rust. References would require
/// the JS runtime to maintain the strings alive across the FFI call, which is not
/// guaranteed by the rquickjs binding model.
#[allow(clippy::needless_pass_by_value)] // rquickjs FFI requires owned values for JS function bindings
pub(crate) fn native_host_call(module: String, options_json: String) -> rquickjs::Result<String> {
    HOST_BRIDGE.with(|cell| {
        let bridge = cell.borrow();
        let bridge = bridge.as_ref().ok_or(rquickjs::Error::Exception)?;

        // Parse options
        let options: serde_json::Value = serde_json::from_str(&options_json)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));

        let method = options
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("POST")
            .to_string();

        let path = options
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("/")
            .to_string();

        let headers: Vec<(String, String)> = options
            .get("headers")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let body = options.get("body").cloned();

        // Send message and block for response
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        bridge
            .tx
            .send(HostMessage::Call {
                module,
                method,
                path,
                headers,
                body,
                response_tx: resp_tx,
            })
            .map_err(|_| rquickjs::Error::Exception)?;

        // Block until handler responds
        let result = resp_rx.recv().map_err(|_| rquickjs::Error::Exception)?;

        serde_json::to_string(&result).map_err(|_| rquickjs::Error::Exception)
    })
}
