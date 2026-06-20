//! Net builtin effect — runtime module for network connectivity.
//!
//! Provides TCP connection management via a process-global registry.
//! Connections are identified by an opaque i64 id and stored in a
//! Mutex<HashMap> with the stream. See design doc for full networking
//! spec (TLS, send, recv, close to follow).

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
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
const NET_ERR_BADHANDLE: i64 = 4;
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

/// Rust-testable send helper. Looks up the conn id in the registry,
/// writes the given bytes to the Plain TcpStream, and returns Ok(bytes_written)
/// or Err with the error code.
#[allow(clippy::disallowed_methods)]
pub fn send(conn_id: i64, data: &[u8]) -> Result<usize, i64> {
    let mut registry = CONN_REGISTRY.lock().expect("CONN_REGISTRY lock poisoned");
    match registry.map.get_mut(&conn_id) {
        Some(Conn::Plain(stream)) => match stream.write(data) {
            Ok(n) => Ok(n),
            Err(_) => Err(NET_ERR_OTHER),
        },
        None => Err(NET_ERR_BADHANDLE),
    }
}

/// Rust-testable recv helper. Looks up the conn id in the registry,
/// reads up to `max` bytes from the Plain TcpStream into a buffer,
/// and returns Ok(Vec<u8>) (empty vec = EOF) or Err with the error code.
#[allow(clippy::disallowed_methods)]
pub fn recv(conn_id: i64, max: usize) -> Result<Vec<u8>, i64> {
    let mut registry = CONN_REGISTRY.lock().expect("CONN_REGISTRY lock poisoned");
    match registry.map.get_mut(&conn_id) {
        Some(Conn::Plain(stream)) => {
            let mut buf = vec![0u8; max];
            match stream.read(&mut buf) {
                Ok(0) => Ok(Vec::new()),
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(_) => Err(NET_ERR_OTHER),
            }
        }
        None => Err(NET_ERR_BADHANDLE),
    }
}

/// Rust-testable close helper. Looks up the conn id in the registry,
/// removes it (which drops the TcpStream), and returns Ok(()) or Err
/// with the error code.
#[allow(clippy::disallowed_methods)]
pub fn close(conn_id: i64) -> Result<(), i64> {
    let mut registry = CONN_REGISTRY.lock().expect("CONN_REGISTRY lock poisoned");
    match registry.map.remove(&conn_id) {
        Some(_) => Ok(()),
        None => Err(NET_ERR_BADHANDLE),
    }
}

/// Build the 3-element `(Int, Int, String)` result tuple for Net.connect.
/// Bitmap = 0b100 (slot 2 is a pointer; slots 0 & 1 are scalars).
/// Uses the pre-registered tuple_int_int_ptr shape index.
unsafe fn build_net_connect_result_tuple(
    error_tag: i64,
    conn_id: i64,
    error_msg: *mut u8,
) -> *mut u8 {
    let idx = crate::gc::runtime_shape_indices().tuple_int_int_ptr;
    crate::effect_helpers::alloc_tuple(
        &[error_tag as u64, conn_id as u64, error_msg as u64],
        0b100,
        idx,
    )
}

/// Build the 3-element `(Int, Int, String)` result tuple for Net.send.
/// Bitmap = 0b100 (slot 2 is a pointer; slots 0 & 1 are scalars).
/// Uses the pre-registered tuple_int_int_ptr shape index.
unsafe fn build_net_send_result_tuple(
    error_tag: i64,
    bytes_written: i64,
    error_msg: *mut u8,
) -> *mut u8 {
    let idx = crate::gc::runtime_shape_indices().tuple_int_int_ptr;
    crate::effect_helpers::alloc_tuple(
        &[error_tag as u64, bytes_written as u64, error_msg as u64],
        0b100,
        idx,
    )
}

/// Build the 3-element `(Int, ByteArray, String)` result tuple for Net.recv.
/// Bitmap = 0b110 (slots 1 & 2 are pointers; slot 0 is a scalar).
/// Uses the pre-registered tuple_int_ptr_ptr shape index.
unsafe fn build_net_recv_result_tuple(
    error_tag: i64,
    data: *mut u8,
    error_msg: *mut u8,
) -> *mut u8 {
    let idx = crate::gc::runtime_shape_indices().tuple_int_ptr_ptr;
    crate::effect_helpers::alloc_tuple(
        &[error_tag as u64, data as u64, error_msg as u64],
        0b110,
        idx,
    )
}

/// Build the 2-element `(Int, String)` result tuple for Net.close.
/// Bitmap = 0b10 (slot 1 is a pointer; slot 0 is a scalar).
/// Uses the pre-registered tuple_int_ptr shape index.
unsafe fn build_net_close_result_tuple(error_tag: i64, error_msg: *mut u8) -> *mut u8 {
    let idx = crate::gc::runtime_shape_indices().tuple_int_ptr;
    crate::effect_helpers::alloc_tuple(&[error_tag as u64, error_msg as u64], 0b10, idx)
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

/// `Net.send(conn_id: Int, data: ByteArray) -> (Int, Int, String)` arm fn.
///
/// # Safety
///
/// `args_len == 7` (2 user args + trailing quintuple). `in_args[0]` is a
/// conn_id Int; `in_args[1]` is a non-null `TAG_BYTEARRAY` pointer (data).
#[no_mangle]
pub unsafe extern "C" fn sigil_net_send_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 7,
        "sigil_net_send_arm: args_len {args_len} != 7"
    );
    debug_assert!(!in_args.is_null());

    let conn_id = *in_args as i64;
    let data_ptr = *in_args.add(1) as *const u8;
    let k_closure = *in_args.add(2) as *mut u8;
    let k_fn = *in_args.add(3) as *mut u8;

    // Read the ByteArray data.
    let data_len = unsafe { crate::byte_array::sigil_byte_array_length(data_ptr) as usize };
    // SAFETY: gc-heap-ptr arithmetic (ByteArray payload starts at offset 16; bounds [16, 16+data_len)).
    let data_bytes = unsafe { data_ptr.add(16) };
    let data_slice = unsafe { std::slice::from_raw_parts(data_bytes, data_len) };

    // Call the Rust send helper.
    let (error_tag, bytes_written) = match send(conn_id, data_slice) {
        Ok(n) => (NET_OK, n as i64),
        Err(code) => (code, 0),
    };

    let error_msg = if error_tag == NET_OK {
        alloc_string_from_str("")
    } else {
        let msg = match error_tag {
            NET_ERR_BADHANDLE => "Bad connection handle",
            NET_ERR_OTHER => "Write error",
            _ => "Send error",
        };
        alloc_string_from_str(msg)
    };

    let tup = build_net_send_result_tuple(error_tag, bytes_written, error_msg);
    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Net.recv(conn_id: Int, max: Int) -> (Int, ByteArray, String)` arm fn.
///
/// # Safety
///
/// `args_len == 7` (2 user args + trailing quintuple). `in_args[0]` is a
/// conn_id Int; `in_args[1]` is a max Int.
#[no_mangle]
pub unsafe extern "C" fn sigil_net_recv_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 7,
        "sigil_net_recv_arm: args_len {args_len} != 7"
    );
    debug_assert!(!in_args.is_null());

    let conn_id = *in_args as i64;
    let max = *in_args.add(1) as usize;
    let k_closure = *in_args.add(2) as *mut u8;
    let k_fn = *in_args.add(3) as *mut u8;

    // Call the Rust recv helper.
    let (error_tag, data_vec) = match recv(conn_id, max) {
        Ok(vec) => (NET_OK, vec),
        Err(code) => (code, Vec::new()),
    };

    // Allocate ByteArray and copy data into it.
    let data_ptr = crate::byte_array::sigil_byte_array_alloc(data_vec.len() as u64, 0);
    if !data_vec.is_empty() {
        // SAFETY: gc-heap-ptr arithmetic (ByteArray payload starts at offset 16).
        let payload = data_ptr.add(16);
        // SAFETY: gc-heap-ptr arithmetic (data_vec is a Rust-owned Vec; payload is the ByteArray interior byte buffer at offset 16, bounds [16, 16+data_vec.len())).
        std::ptr::copy_nonoverlapping(data_vec.as_ptr(), payload, data_vec.len());
    }

    let error_msg = if error_tag == NET_OK {
        alloc_string_from_str("")
    } else {
        let msg = match error_tag {
            NET_ERR_BADHANDLE => "Bad connection handle",
            NET_ERR_OTHER => "Read error",
            _ => "Recv error",
        };
        alloc_string_from_str(msg)
    };

    let tup = build_net_recv_result_tuple(error_tag, data_ptr, error_msg);
    write_k_dispatch_value(k_closure, k_fn, tup as u64)
}

/// `Net.close(conn_id: Int) -> (Int, String)` arm fn.
///
/// # Safety
///
/// `args_len == 6` (1 user arg + trailing quintuple). `in_args[0]` is a
/// conn_id Int.
#[no_mangle]
pub unsafe extern "C" fn sigil_net_close_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(
        args_len == 6,
        "sigil_net_close_arm: args_len {args_len} != 6"
    );
    debug_assert!(!in_args.is_null());

    let conn_id = *in_args as i64;
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;

    // Call the Rust close helper.
    let error_tag = match close(conn_id) {
        Ok(()) => NET_OK,
        Err(code) => code,
    };

    let error_msg = if error_tag == NET_OK {
        alloc_string_from_str("")
    } else {
        let msg = match error_tag {
            NET_ERR_BADHANDLE => "Bad connection handle",
            _ => "Close error",
        };
        alloc_string_from_str(msg)
    };

    let tup = build_net_close_result_tuple(error_tag, error_msg);
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

    #[test]
    fn send_to_connected_stream_writes_bytes() {
        let _g = gc_test_lock();
        use std::io::Read;
        use std::thread;

        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Spawn a thread to accept the connection and read the data.
        let receiver_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut buf = [0u8; 13];
            let n = stream.read(&mut buf).expect("read from stream");
            (buf, n)
        });

        // Connect to the listener.
        let conn_id = connect("127.0.0.1", port).expect("connect should succeed");

        // Send data through the connection.
        let test_data = b"Hello, World!";
        let result = send(conn_id, test_data);
        assert!(result.is_ok(), "send should succeed");
        let bytes_written = result.unwrap();
        assert_eq!(bytes_written, 13, "should write all bytes");

        // Wait for the receiver thread and verify the data.
        let (buf, n) = receiver_thread.join().expect("receiver thread panicked");
        assert_eq!(n, 13, "listener should receive all bytes");
        assert_eq!(&buf[..n], test_data, "listener should receive correct data");
    }

    #[test]
    fn recv_from_connected_stream_reads_bytes() {
        let _g = gc_test_lock();
        use std::io::Write;
        use std::thread;

        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Spawn a thread to accept the connection and write the data.
        let writer_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let test_data = b"Hello, Recv!";
            stream.write_all(test_data).expect("write to stream");
            drop(stream);
        });

        // Connect to the listener.
        let conn_id = connect("127.0.0.1", port).expect("connect should succeed");

        // Recv data through the connection.
        let result = recv(conn_id, 100);
        assert!(result.is_ok(), "recv should succeed");
        let data = result.unwrap();
        assert_eq!(&data[..], b"Hello, Recv!", "should recv correct data");

        // Wait for the writer thread.
        writer_thread.join().expect("writer thread panicked");
    }

    #[test]
    fn recv_from_closed_peer_yields_empty() {
        let _g = gc_test_lock();
        use std::thread;

        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Spawn a thread to accept and immediately close the connection.
        let closer_thread = thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("accept connection");
            drop(_stream);
        });

        // Connect to the listener.
        let conn_id = connect("127.0.0.1", port).expect("connect should succeed");

        // Wait for the peer to close.
        closer_thread.join().expect("closer thread panicked");

        // Recv from the closed stream should return empty.
        let result = recv(conn_id, 100);
        assert!(result.is_ok(), "recv should succeed on closed stream");
        let data = result.unwrap();
        assert!(
            data.is_empty(),
            "recv should return empty on closed peer (EOF)"
        );
    }

    #[test]
    fn close_connected_stream_removes_from_registry() {
        let _g = gc_test_lock();
        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Connect to the listener.
        let conn_id = connect("127.0.0.1", port).expect("connect should succeed");

        // Verify the connection is in the registry.
        {
            let registry = CONN_REGISTRY.lock().expect("lock registry");
            assert!(
                registry.map.contains_key(&conn_id),
                "connection should be in registry"
            );
        }

        // Close the connection.
        let result = close(conn_id);
        assert!(result.is_ok(), "close should succeed");

        // Verify the connection is no longer in the registry.
        let registry = CONN_REGISTRY.lock().expect("lock registry");
        assert!(
            !registry.map.contains_key(&conn_id),
            "connection should be removed from registry"
        );
    }

    #[test]
    fn close_unknown_id_returns_badhandle() {
        let _g = gc_test_lock();
        let result = close(999999);
        assert!(result.is_err(), "close should fail for unknown id");
        let err = result.unwrap_err();
        assert_eq!(err, NET_ERR_BADHANDLE, "error should be BadHandle");
    }

    #[test]
    fn close_same_id_twice_returns_badhandle_second_time() {
        let _g = gc_test_lock();
        // Bind a listener on 127.0.0.1:0 to get an available port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("get local addr");
        let port = addr.port();

        // Connect to the listener.
        let conn_id = connect("127.0.0.1", port).expect("connect should succeed");

        // Close the connection once (should succeed).
        let result1 = close(conn_id);
        assert!(result1.is_ok(), "first close should succeed");

        // Close the same connection again (should fail with BadHandle).
        let result2 = close(conn_id);
        assert!(result2.is_err(), "second close should fail");
        let err = result2.unwrap_err();
        assert_eq!(
            err, NET_ERR_BADHANDLE,
            "second close should return BadHandle"
        );
    }
}
