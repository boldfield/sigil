//! Net builtin effect — runtime module for network connectivity.
//!
//! Provides TCP connection management via a process-global registry.
//! Connections are identified by an opaque i64 id and stored in a
//! Mutex<HashMap> with the stream. See design doc for full networking
//! spec (TLS, send, recv, close to follow).

use std::collections::BTreeMap;
use std::io;
use std::net::TcpStream;
use std::sync::{LazyLock, Mutex};

use crate::effect_helpers::alloc_string_from_str;
use crate::handlers::{write_k_dispatch_value, NextStep, TerminalResult};

#[allow(dead_code)]
enum Conn {
    Plain(TcpStream),
}

struct ConnectionRegistry {
    map: BTreeMap<i64, Conn>,
    next_id: i64,
}

impl ConnectionRegistry {
    fn new() -> Self {
        ConnectionRegistry {
            map: BTreeMap::new(),
            next_id: 1,
        }
    }
}

// Global connection registry: id -> Conn.
static CONN_REGISTRY: LazyLock<Mutex<ConnectionRegistry>> =
    LazyLock::new(|| Mutex::new(ConnectionRegistry::new()));

// Error codes per the design doc.
const NET_OK: i64 = 0;
const NET_ERR_RESOLVE_FAILED: i64 = 1;
const NET_ERR_CONNECTION_REFUSED: i64 = 2;
const NET_ERR_TLS_ERROR: i64 = 3;
const NET_ERR_OTHER: i64 = 5;

/// Rust-testable connect helper. Does TcpStream::connect((host, port)),
/// inserts the stream under a fresh i64 id, and returns Ok(id) or Err
/// with the error code. DNS resolution happens during connect.
#[allow(clippy::disallowed_methods)]
pub fn connect(host: &str, port: u16) -> Result<i64, i64> {
    match TcpStream::connect((host, port)) {
        Ok(stream) => {
            let mut registry = CONN_REGISTRY.lock().expect("CONN_REGISTRY lock poisoned");
            let id = registry.next_id;
            registry.next_id = registry.next_id.wrapping_add(1);
            registry.map.insert(id, Conn::Plain(stream));
            Ok(id)
        }
        Err(e) => {
            let code = match e.kind() {
                io::ErrorKind::NotFound => NET_ERR_RESOLVE_FAILED,
                io::ErrorKind::ConnectionRefused => NET_ERR_CONNECTION_REFUSED,
                _ => NET_ERR_OTHER,
            };
            Err(code)
        }
    }
}

/// Build the 3-element `(Int, Int, String)` result tuple for Net.connect.
/// Bitmap = 0b100 (slot 2 is a pointer; slots 0 & 1 are scalars).
/// Uses conservative scanning (descriptor_index = u32::MAX) since this
/// shape (Int, Int, Ptr) is not pre-registered.
unsafe fn build_net_connect_result_tuple(
    error_tag: i64,
    conn_id: i64,
    error_msg: *mut u8,
) -> *mut u8 {
    crate::effect_helpers::alloc_tuple(
        &[error_tag as u64, conn_id as u64, error_msg as u64],
        0b100,
        u32::MAX,
    )
}

/// `Net.connect(host: String, port: Int, tls: Bool) -> (Int, Int, String)` arm fn.
///
/// # Safety
///
/// `args_len == 8` (3 user args + trailing quintuple). `in_args[0]` is a
/// non-null `TAG_STRING` pointer (host); `in_args[1]` is a port Int;
/// `in_args[2]` is a tls Bool.
#[no_mangle]
pub unsafe extern "C" fn sigil_net_connect_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 8,
        "sigil_net_connect_arm: args_len {args_len} != 8"
    );
    debug_assert!(!in_args.is_null());

    let host_ptr = *in_args as *const u8;
    let port = *in_args.add(1) as u16;
    let tls = *in_args.add(2) != 0;
    let k_closure = *in_args.add(3) as *mut u8;
    let k_fn = *in_args.add(4) as *mut u8;

    // If tls=true, return TlsError (not implemented yet).
    if tls {
        let error_msg = alloc_string_from_str("TLS not yet implemented");
        let tup = build_net_connect_result_tuple(NET_ERR_TLS_ERROR, 0, error_msg);
        return write_k_dispatch_value(k_closure, k_fn, tup as u64);
    }

    // Read the host string.
    let (host_bytes, host_len) = crate::gc::string_bytes(host_ptr);
    let host_slice = std::slice::from_raw_parts(host_bytes, host_len);
    let host_str = match std::str::from_utf8(host_slice) {
        Ok(h) => h,
        Err(_) => {
            let error_msg = alloc_string_from_str("Invalid UTF-8 in host");
            let tup = build_net_connect_result_tuple(NET_ERR_OTHER, 0, error_msg);
            return write_k_dispatch_value(k_closure, k_fn, tup as u64);
        }
    };

    // Call the Rust connect helper.
    let (error_tag, conn_id) = match connect(host_str, port) {
        Ok(id) => (NET_OK, id),
        Err(code) => (code, 0),
    };

    let error_msg = if error_tag == NET_OK {
        alloc_string_from_str("")
    } else {
        let msg = match error_tag {
            NET_ERR_RESOLVE_FAILED => "DNS resolution failed",
            NET_ERR_CONNECTION_REFUSED => "Connection refused",
            NET_ERR_TLS_ERROR => "TLS error",
            _ => "Connection error",
        };
        alloc_string_from_str(msg)
    };

    let tup = build_net_connect_result_tuple(error_tag, conn_id, error_msg);
    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;
    use std::net::TcpListener;

    #[test]
    fn connect_to_tcp_listener_returns_id() {
        let _g = gc_test_lock();
        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Try to connect to the listener.
        let result = connect("127.0.0.1", port);
        assert!(result.is_ok(), "connect should succeed");
        let conn_id = result.unwrap();
        assert!(conn_id > 0, "conn_id should be positive");

        // Verify the connection is in the registry.
        let registry = CONN_REGISTRY.lock().expect("lock registry");
        assert!(
            registry.map.contains_key(&conn_id),
            "connection should be in registry"
        );
    }

    #[test]
    fn connect_with_localhost_hostname_resolves_dns() {
        let _g = gc_test_lock();
        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Try to connect using "localhost" hostname (requires DNS resolution).
        let result = connect("localhost", port);
        assert!(
            result.is_ok(),
            "connect to localhost should succeed with DNS resolution"
        );
        let conn_id = result.unwrap();
        assert!(conn_id > 0, "conn_id should be positive");

        // Verify the connection is in the registry.
        let registry = CONN_REGISTRY.lock().expect("lock registry");
        assert!(
            registry.map.contains_key(&conn_id),
            "connection should be in registry"
        );
    }
}
