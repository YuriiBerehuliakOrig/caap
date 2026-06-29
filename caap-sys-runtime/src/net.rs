use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream, UdpSocket};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue, MAX_READ_BYTES};

const NET_TIMEOUT_MAX_MS: u64 = i32::MAX as u64;
const MAX_NET_HANDLES: usize = 1024;

// ── Handle tables ────────────────────────────────────────────────────────────

// Listener handle IDs: 1001..=2024, Socket: 2001..=3024, UDP: 3001..=4024.
// Fixed disjoint ranges; IDs are recycled on close.
const LISTENER_HANDLE_BASE: i32 = 1000;
const SOCKET_HANDLE_BASE: i32 = 2000;
const UDP_HANDLE_BASE: i32 = 3000;

/// Open listener/socket/UDP tables for one runtime session. Owned explicitly by
/// the caller and passed into every networking operation; see [`crate::fs::FsState`]
/// for the rationale behind state injection.
pub struct NetState {
    listeners: HashMap<i32, TcpListener>,
    sockets: HashMap<i32, TcpStream>,
    udp_sockets: HashMap<i32, UdpSocket>,
}

impl Default for NetState {
    fn default() -> Self {
        Self::new()
    }
}

impl NetState {
    pub fn new() -> Self {
        Self {
            listeners: HashMap::new(),
            sockets: HashMap::new(),
            udp_sockets: HashMap::new(),
        }
    }

    fn store_udp(&mut self, socket: UdpSocket) -> Result<i32, SysError> {
        let handle = allocate_net_handle(&self.udp_sockets, UDP_HANDLE_BASE, "UDP")?;
        self.udp_sockets.insert(handle, socket);
        Ok(handle)
    }

    fn store_listener(&mut self, listener: TcpListener) -> Result<i32, SysError> {
        let handle = allocate_net_handle(&self.listeners, LISTENER_HANDLE_BASE, "listener")?;
        self.listeners.insert(handle, listener);
        Ok(handle)
    }

    fn store_socket(&mut self, socket: TcpStream) -> Result<i32, SysError> {
        let handle = allocate_net_handle(&self.sockets, SOCKET_HANDLE_BASE, "socket")?;
        self.sockets.insert(handle, socket);
        Ok(handle)
    }
}

/// Hand out the lowest free ID in a handle table's fixed range (`base+1..=base+MAX`).
/// IDs are recycled implicitly: `net_close` removes the entry, which frees the slot
/// for the next scan. Shared by all three handle tables so the cap and error wording
/// stay identical across listener/socket/UDP allocation.
fn allocate_net_handle<T>(table: &HashMap<i32, T>, base: i32, kind: &str) -> Result<i32, SysError> {
    if table.len() >= MAX_NET_HANDLES {
        return Err(SysError::resource_exhausted(format!(
            "net: too many open {kind} handles (limit 1024)"
        )));
    }
    (1..=(MAX_NET_HANDLES as i32))
        .map(|i| base + i)
        .find(|id| !table.contains_key(id))
        .ok_or_else(|| {
            SysError::resource_exhausted(format!("net: {kind} handle allocation failed"))
        })
}

// ── Public invoke ─────────────────────────────────────────────────────────────

pub fn invoke(state: &mut NetState, name: &str, args: SysArgs) -> SysResult {
    tracing::debug!(name, "net invoke");
    match name {
        "listen" => net_listen(state, args),
        "accept" => net_accept(state, args),
        "connect" => net_connect(state, args),
        "read" => net_read(state, args),
        "write" => net_write(state, args),
        "read_bytes" => net_read_bytes(state, args),
        "write_bytes" => net_write_bytes(state, args),
        "close" => net_close(state, args),
        "poll" => net_poll(state, args),
        "is_ip" => net_is_ip(args),
        "is_loopback" => net_is_loopback(args),
        "host_port" => net_host_port(args),
        "resolve" => net_resolve(args),
        "local_addr" => net_local_addr(state, args),
        "peer_addr" => net_peer_addr(state, args),
        "shutdown" => net_shutdown(state, args),
        "udp_bind" => net_udp_bind(state, args),
        "udp_send_to" => net_udp_send_to(state, args),
        "udp_recv_from" => net_udp_recv_from(state, args),
        _ => Err(format!("net: unknown export '{name}'").into()),
    }
}

fn net_listen(state: &mut NetState, args: SysArgs) -> SysResult {
    let spec = args.require_map(0, "net.listen")?;
    let host = spec.require_str("host", "net.listen spec.host")?;
    let port = spec.require_int("port", "net.listen spec.port")?;
    validate_port(port, "net.listen spec.port")?;
    let reuse_addr = optional_bool(&spec, "reuse_addr", "net.listen")?;
    let backlog = optional_positive_i32(&spec, "backlog", 128, "net.listen")?;
    let listener = tcp_listen(host.as_str(), port as u16, reuse_addr, backlog)?;
    let handle = state.store_listener(listener)?;
    Ok(SysValue::Int(handle as i64))
}

#[cfg(unix)]
fn tcp_listen(
    host: &str,
    port: u16,
    reuse_addr: bool,
    backlog: i32,
) -> Result<TcpListener, SysError> {
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|e| SysError::from_io("net.listen", e))?
        .next()
        .ok_or_else(|| "net.listen: host resolved to no socket addresses".to_string())?;
    let domain = if address.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    // SAFETY: `libc::socket` is a standard FFI syscall; we check the returned fd for errors.
    // SOCK_CLOEXEC prevents the socket fd from leaking into child processes on fork+exec.
    let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(SysError::from_io(
            "net.listen",
            std::io::Error::last_os_error(),
        ));
    }
    let result = bind_and_listen_fd(fd, address, reuse_addr, backlog);
    match result {
        // SAFETY: `fd` is a valid file descriptor for a bound and listening TCP socket.
        Ok(()) => Ok(unsafe { TcpListener::from_raw_fd(fd as RawFd) }),
        Err(error) => {
            // SAFETY: `fd` is a valid open file descriptor; closing it on error to avoid fd leak.
            unsafe {
                libc::close(fd);
            }
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn tcp_listen(
    host: &str,
    port: u16,
    reuse_addr: bool,
    backlog: i32,
) -> Result<TcpListener, SysError> {
    let _ = (reuse_addr, backlog);
    TcpListener::bind((host, port)).map_err(|e| SysError::from_io("net.listen", e))
}

#[cfg(unix)]
fn bind_and_listen_fd(
    fd: libc::c_int,
    address: std::net::SocketAddr,
    reuse_addr: bool,
    backlog: i32,
) -> Result<(), SysError> {
    if reuse_addr {
        let opt: libc::c_int = 1;
        // SAFETY: `fd` is a valid socket fd; `opt` is a local `c_int` correctly passed to
        // `setsockopt` with matching pointer and size.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &opt as *const _ as *const libc::c_void,
                std::mem::size_of_val(&opt) as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(SysError::from_io(
                "net.listen",
                std::io::Error::last_os_error(),
            ));
        }
    }
    let (storage, len) = socket_addr_storage(address);
    // SAFETY: `fd` is a valid socket fd; `storage` is a correctly initialized `sockaddr_storage`
    // with `len` reflecting the actual address family size.
    let rc = unsafe {
        libc::bind(
            fd,
            &storage as *const _ as *const libc::sockaddr,
            len as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(SysError::from_io(
            "net.listen",
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fd` is a bound socket fd; `backlog` is a validated positive i32.
    let rc = unsafe { libc::listen(fd, backlog) };
    if rc != 0 {
        return Err(SysError::from_io(
            "net.listen",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn socket_addr_storage(address: std::net::SocketAddr) -> (libc::sockaddr_storage, usize) {
    // SAFETY: `sockaddr_storage`, `sockaddr_in`, and `sockaddr_in6` are C repr structs valid when
    // zeroed; we initialize every relevant field before writing into `storage`.
    unsafe {
        let mut storage: libc::sockaddr_storage = std::mem::zeroed();
        match address {
            std::net::SocketAddr::V4(address) => {
                let mut raw: libc::sockaddr_in = std::mem::zeroed();
                raw.sin_family = libc::AF_INET as libc::sa_family_t;
                raw.sin_port = address.port().to_be();
                raw.sin_addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                };
                std::ptr::write(&mut storage as *mut _ as *mut libc::sockaddr_in, raw);
                (storage, std::mem::size_of::<libc::sockaddr_in>())
            }
            std::net::SocketAddr::V6(address) => {
                let mut raw: libc::sockaddr_in6 = std::mem::zeroed();
                raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                raw.sin6_port = address.port().to_be();
                raw.sin6_flowinfo = address.flowinfo();
                raw.sin6_scope_id = address.scope_id();
                raw.sin6_addr = libc::in6_addr {
                    s6_addr: address.ip().octets(),
                };
                std::ptr::write(&mut storage as *mut _ as *mut libc::sockaddr_in6, raw);
                (storage, std::mem::size_of::<libc::sockaddr_in6>())
            }
        }
    }
}

fn net_accept(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.accept")?, "net.accept")?;
    let timeout_ms = optional_timeout_arg(args.optional(1), "net.accept timeout_ms")?;
    let listener = state
        .listeners
        .get(&handle)
        .ok_or_else(|| format!("net.accept: unknown listener handle {handle}"))?;
    let socket = accept_with_timeout(listener, timeout_ms)?;
    let sh = state.store_socket(socket)?;
    Ok(SysValue::Int(sh as i64))
}

fn net_connect(state: &mut NetState, args: SysArgs) -> SysResult {
    let spec = args.require_map(0, "net.connect")?;
    let host = spec.require_str("host", "net.connect spec.host")?;
    let port = spec.require_int("port", "net.connect spec.port")?;
    validate_port(port, "net.connect spec.port")?;
    let timeout_ms = optional_timeout_map(&spec, "timeout_ms", "net.connect")?;
    let socket = connect_tcp(&host, port as u16, timeout_ms)?;
    let handle = state.store_socket(socket)?;
    Ok(SysValue::Int(handle as i64))
}

fn net_read(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.read")?, "net.read")?;
    let max_bytes = args.require_int(1, "net.read max_bytes")?;
    let timeout_ms = optional_timeout_arg(args.optional(2), "net.read timeout_ms")?;
    let max_bytes = usize::try_from(max_bytes).map_err(|_| {
        SysError::invalid_argument("net.read: max_bytes must be a non-negative int")
    })?;
    // A single read returns at most the buffer size; callers handle partial
    // reads, so cap the buffer to avoid a giant caller-driven allocation.
    let mut buf = vec![0u8; max_bytes.min(MAX_READ_BYTES)];
    let s = state
        .sockets
        .get_mut(&handle)
        .ok_or_else(|| format!("net.read: unknown socket handle {handle}"))?;
    let n = read_with_timeout(s, &mut buf, timeout_ms)?;
    buf.truncate(n);
    let text = String::from_utf8(buf)
        .map_err(|error| SysError::invalid_argument(format!("net.read: {error}")))?;
    Ok(SysValue::Str(text))
}

fn net_write(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.write")?, "net.write")?;
    let text = args.require_str(1, "net.write text")?;
    let timeout_ms = optional_timeout_arg(args.optional(2), "net.write timeout_ms")?;
    let s = state
        .sockets
        .get_mut(&handle)
        .ok_or_else(|| format!("net.write: unknown socket handle {handle}"))?;
    write_all_with_timeout(s, text.as_bytes(), timeout_ms)?;
    Ok(SysValue::Null)
}

fn net_read_bytes(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.read_bytes")?, "net.read_bytes")?;
    let max_bytes = args.require_int(1, "net.read-bytes max_bytes")?;
    let timeout_ms = optional_timeout_arg(args.optional(2), "net.read-bytes timeout_ms")?;
    let max_bytes = usize::try_from(max_bytes).map_err(|_| {
        SysError::invalid_argument("net.read_bytes: max_bytes must be a non-negative int")
    })?;
    let mut buf = vec![0u8; max_bytes.min(MAX_READ_BYTES)];
    let s = state
        .sockets
        .get_mut(&handle)
        .ok_or_else(|| format!("net.read_bytes: unknown socket handle {handle}"))?;
    let n = read_with_timeout(s, &mut buf, timeout_ms)?;
    buf.truncate(n);
    Ok(SysValue::Bytes(buf))
}

fn net_write_bytes(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.write_bytes")?, "net.write_bytes")?;
    let bytes = args.require_bytes(1, "net.write_bytes")?;
    let timeout_ms = optional_timeout_arg(args.optional(2), "net.write-bytes timeout_ms")?;
    let s = state
        .sockets
        .get_mut(&handle)
        .ok_or_else(|| format!("net.write_bytes: unknown socket handle {handle}"))?;
    write_all_with_timeout(s, &bytes, timeout_ms)?;
    Ok(SysValue::Null)
}

fn net_close(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.close")?, "net.close")?;
    if state.listeners.remove(&handle).is_none()
        && state.sockets.remove(&handle).is_none()
        && state.udp_sockets.remove(&handle).is_none()
    {
        return Err(SysError::not_found(format!(
            "net.close: unknown handle {handle}"
        )));
    }
    Ok(SysValue::Null)
}

fn net_poll(state: &mut NetState, args: SysArgs) -> SysResult {
    let handles = sys_handle_sequence(&args.require_list(0, "net.poll")?)?;
    let timeout_ms = args.require_int(1, "net.poll")?;
    if timeout_ms < 0 {
        return Err("net.poll: timeout_ms must be a non-negative int".into());
    }
    if timeout_ms > i64::from(i32::MAX) {
        return Err("net.poll: timeout_ms exceeds system poll range".into());
    }
    if handles.is_empty() {
        return Ok(SysValue::List(Vec::new()));
    }
    net_poll_state(state, &handles, timeout_ms)
}

fn net_is_ip(args: SysArgs) -> SysResult {
    let addr = args.require_str(0, "net.is_ip")?;
    Ok(SysValue::Bool(IpAddr::from_str(addr.as_str()).is_ok()))
}

fn net_is_loopback(args: SysArgs) -> SysResult {
    let addr = args.require_str(0, "net.is_loopback")?;
    let ip = IpAddr::from_str(addr.as_str())
        .map_err(|e| SysError::invalid_argument(format!("net.is_loopback: {e}")))?;
    Ok(SysValue::Bool(ip.is_loopback()))
}

fn net_host_port(args: SysArgs) -> SysResult {
    let host = args.require_str(0, "net.host_port")?;
    let port = args.require_int(1, "net.host_port")?;
    validate_port(port, "net.host_port")?;
    if host.contains(':') && !host.starts_with('[') {
        Ok(SysValue::Str(format!("[{host}]:{port}")))
    } else {
        Ok(SysValue::Str(format!("{host}:{port}")))
    }
}

fn net_resolve(args: SysArgs) -> SysResult {
    let address = args.require_str(0, "net.resolve")?;
    let addrs = address
        .to_socket_addrs()
        .map_err(|error| SysError::from_io("net.resolve", error))?;
    let resolved: Vec<SysValue> = addrs.map(|addr| SysValue::Str(addr.to_string())).collect();
    if resolved.is_empty() {
        return Err(SysError::other(
            "net.resolve: host resolved to no socket addresses",
        ));
    }
    Ok(SysValue::List(resolved))
}

fn net_local_addr(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.local_addr")?, "net.local_addr")?;
    let addr = if let Some(listener) = state.listeners.get(&handle) {
        listener.local_addr()
    } else if let Some(socket) = state.sockets.get(&handle) {
        socket.local_addr()
    } else if let Some(udp) = state.udp_sockets.get(&handle) {
        udp.local_addr()
    } else {
        return Err(SysError::not_found(format!(
            "net.local_addr: unknown handle {handle}"
        )));
    };
    Ok(SysValue::Str(
        addr.map_err(|error| SysError::from_io("net.local_addr", error))?
            .to_string(),
    ))
}

fn net_peer_addr(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.peer_addr")?, "net.peer_addr")?;
    let addr = if let Some(socket) = state.sockets.get(&handle) {
        socket.peer_addr()
    } else if let Some(udp) = state.udp_sockets.get(&handle) {
        udp.peer_addr()
    } else {
        return Err(SysError::not_found(format!(
            "net.peer_addr: unknown socket handle {handle}"
        )));
    };
    Ok(SysValue::Str(
        addr.map_err(|error| SysError::from_io("net.peer_addr", error))?
            .to_string(),
    ))
}

fn net_shutdown(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.shutdown")?, "net.shutdown")?;
    let how = args.require_str(1, "net.shutdown")?;
    let how = match how.as_str() {
        "read" => Shutdown::Read,
        "write" => Shutdown::Write,
        "both" => Shutdown::Both,
        _ => {
            return Err(SysError::invalid_argument(
                "net.shutdown: how must be read, write, or both",
            ))
        }
    };
    let socket = state
        .sockets
        .get(&handle)
        .ok_or_else(|| format!("net.shutdown: unknown socket handle {handle}"))?;
    socket
        .shutdown(how)
        .map_err(|error| SysError::from_io("net.shutdown", error))?;
    Ok(SysValue::Null)
}

fn net_udp_bind(state: &mut NetState, args: SysArgs) -> SysResult {
    let spec = args.require_map(0, "net.udp_bind")?;
    let host = spec.require_str("host", "net.udp-bind spec.host")?;
    let port = spec.require_int("port", "net.udp-bind spec.port")?;
    validate_port(port, "net.udp-bind spec.port")?;
    let socket = UdpSocket::bind((host.as_str(), port as u16))
        .map_err(|error| SysError::from_io("net.udp_bind", error))?;
    let handle = state.store_udp(socket)?;
    Ok(SysValue::Int(handle as i64))
}

fn net_udp_send_to(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(args.require_int(0, "net.udp_send_to")?, "net.udp_send_to")?;
    let data = args.require_str(1, "net.udp_send_to")?;
    let addr = args.require_str(2, "net.udp_send_to")?;
    let socket = state
        .udp_sockets
        .get(&handle)
        .ok_or_else(|| format!("net.udp_send_to: unknown UDP handle {handle}"))?;
    let sent = socket
        .send_to(data.as_bytes(), addr.as_str())
        .map_err(|error| SysError::from_io("net.udp_send_to", error))?;
    i64::try_from(sent).map(SysValue::Int).map_err(|_| {
        SysError::invalid_argument("net.udp_send_to: byte count exceeds CAAP SYS int range")
    })
}

fn net_udp_recv_from(state: &mut NetState, args: SysArgs) -> SysResult {
    let handle = require_i32_handle(
        args.require_int(0, "net.udp_recv_from")?,
        "net.udp_recv_from",
    )?;
    let max_bytes = args.require_int(1, "net.udp-recv-from max_bytes")?;
    let timeout_ms = optional_timeout_arg(args.optional(2), "net.udp-recv-from timeout_ms")?;
    let max_bytes = usize::try_from(max_bytes).map_err(|_| {
        SysError::invalid_argument("net.udp_recv_from: max_bytes must be a non-negative int")
    })?;
    let max_bytes = max_bytes.min(MAX_READ_BYTES);
    let socket = state
        .udp_sockets
        .get(&handle)
        .ok_or_else(|| format!("net.udp_recv_from: unknown UDP handle {handle}"))?;
    let previous = socket
        .read_timeout()
        .map_err(|error| SysError::from_io("net.udp_recv_from", error))?;
    if let Some(timeout_ms) = timeout_ms {
        socket
            .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
            .map_err(|error| SysError::from_io("net.udp_recv_from", error))?;
    }
    let mut buf = vec![0u8; max_bytes];
    let result = socket.recv_from(&mut buf);
    let _ = socket.set_read_timeout(previous);
    let (read, from) = result.map_err(|error| SysError::from_io("net.udp_recv_from", error))?;
    buf.truncate(read);
    let data = String::from_utf8(buf)
        .map_err(|error| SysError::invalid_argument(format!("net.udp_recv_from: {error}")))?;
    Ok(SysValue::Map(HashMap::from([
        ("data".to_string(), SysValue::Str(data)),
        ("from".to_string(), SysValue::Str(from.to_string())),
    ])))
}

fn validate_port(port: i64, ctx: &str) -> Result<(), SysError> {
    if !(0..=65535).contains(&port) {
        return Err(SysError::invalid_argument(format!(
            "{ctx}: port must be in 0..=65535"
        )));
    }
    Ok(())
}

fn optional_bool(spec: &crate::ffi_value::SysMap, key: &str, ctx: &str) -> Result<bool, SysError> {
    match spec.0.get(key) {
        Some(SysValue::Bool(value)) => Ok(*value),
        Some(SysValue::Null) | None => Ok(false),
        Some(_) => Err(SysError::invalid_argument(format!(
            "{ctx} spec.{key}: field must be bool"
        ))),
    }
}

fn optional_positive_i32(
    spec: &crate::ffi_value::SysMap,
    key: &str,
    default: i32,
    ctx: &str,
) -> Result<i32, SysError> {
    match spec.0.get(key) {
        Some(SysValue::Int(value)) if *value > 0 => i32::try_from(*value).map_err(|_| {
            SysError::invalid_argument(format!("{ctx} spec.{key}: field exceeds system range"))
        }),
        Some(SysValue::Int(_)) => Err(SysError::invalid_argument(format!(
            "{ctx} spec.{key}: field must be a positive int"
        ))),
        Some(SysValue::Null) | None => Ok(default),
        Some(_) => Err(SysError::invalid_argument(format!(
            "{ctx} spec.{key}: field must be int"
        ))),
    }
}

fn optional_timeout_map(
    spec: &crate::ffi_value::SysMap,
    key: &str,
    ctx: &str,
) -> Result<Option<u64>, SysError> {
    match spec.0.get(key) {
        Some(SysValue::Int(value)) if *value >= 0 => {
            let timeout_ms = u64::try_from(*value).map_err(|_| {
                SysError::invalid_argument(format!(
                    "{ctx} spec.{key}: field exceeds system timeout range"
                ))
            })?;
            validate_net_timeout_ms(timeout_ms, &format!("{ctx} spec.{key}: field"))?;
            Ok(Some(timeout_ms))
        }
        Some(SysValue::Int(_)) => Err(SysError::invalid_argument(format!(
            "{ctx} spec.{key}: field must be non-negative"
        ))),
        Some(SysValue::Null) | None => Ok(None),
        Some(_) => Err(SysError::invalid_argument(format!(
            "{ctx} spec.{key}: field must be int"
        ))),
    }
}

fn optional_timeout_arg(value: Option<&SysValue>, ctx: &str) -> Result<Option<u64>, SysError> {
    match value {
        Some(SysValue::Int(value)) if *value >= 0 => {
            let timeout_ms = u64::try_from(*value).map_err(|_| {
                SysError::invalid_argument(format!("{ctx}: exceeds system timeout range"))
            })?;
            validate_net_timeout_ms(timeout_ms, ctx)?;
            Ok(Some(timeout_ms))
        }
        Some(SysValue::Int(_)) => Err(SysError::invalid_argument(format!(
            "{ctx}: must be non-negative"
        ))),
        Some(SysValue::Null) | None => Ok(None),
        Some(_) => Err(SysError::invalid_argument(format!("{ctx}: must be int"))),
    }
}

fn sys_handle_sequence(items: &[SysValue]) -> Result<Vec<i32>, SysError> {
    items
        .iter()
        .map(|item| match item {
            SysValue::Int(handle) => i32::try_from(*handle).map_err(|_| {
                SysError::invalid_argument(format!("net.poll: handle out of i32 range: {handle}"))
            }),
            other => Err(SysError::invalid_argument(format!(
                "net.poll: handles must contain ints, got {:?}",
                other
            ))),
        })
        .collect()
}

fn require_i32_handle(handle: i64, ctx: &str) -> Result<i32, SysError> {
    i32::try_from(handle).map_err(|_| {
        SysError::invalid_argument(format!("{ctx}: handle out of i32 range: {handle}"))
    })
}

fn validate_net_timeout_ms(timeout_ms: u64, ctx: &str) -> Result<(), SysError> {
    if timeout_ms > NET_TIMEOUT_MAX_MS {
        return Err(SysError::invalid_argument(format!(
            "{ctx}: exceeds system timeout range"
        )));
    }
    Ok(())
}

fn connect_tcp(host: &str, port: u16, timeout_ms: Option<u64>) -> Result<TcpStream, SysError> {
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|error| SysError::from_io("net.connect", error))?
        .collect();
    if addresses.is_empty() {
        return Err(SysError::other(
            "net.connect: host resolved to no socket addresses",
        ));
    }
    if let Some(timeout_ms) = timeout_ms {
        let timeout = Duration::from_millis(timeout_ms);
        let mut last_error = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(SysError::other(format!(
            "net.connect: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "host resolved to no socket addresses".to_string())
        )))
    } else {
        TcpStream::connect(addresses.as_slice())
            .map_err(|error| SysError::from_io("net.connect", error))
    }
}

#[cfg(unix)]
fn accept_with_timeout(
    listener: &TcpListener,
    timeout_ms: Option<u64>,
) -> Result<TcpStream, SysError> {
    let Some(timeout_ms) = timeout_ms else {
        return listener
            .accept()
            .map(|(stream, _)| stream)
            .map_err(|error| SysError::from_io("net.accept", error));
    };
    let deadline = net_timeout_deadline(timeout_ms, "net.accept")?;
    // Use poll(2) instead of set_nonblocking + sleep: no mode change needed,
    // no 5 ms granularity, no risk of leaving the listener in non-blocking mode.
    loop {
        let remaining_ms = match deadline.checked_duration_since(Instant::now()) {
            Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
            None => return Err(SysError::timed_out("net.accept: timed out")),
        };
        let mut pollfd = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pollfd` is a valid initialised struct; `fd` is a live listener fd.
        let rc = unsafe { libc::poll(&mut pollfd, 1, remaining_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(SysError::from_io("net.accept", err));
        }
        if rc == 0 || pollfd.revents & libc::POLLIN == 0 {
            if Instant::now() >= deadline {
                return Err(SysError::timed_out("net.accept: timed out"));
            }
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(SysError::from_io("net.accept", error)),
        }
    }
}

#[cfg(not(unix))]
fn accept_with_timeout(
    listener: &TcpListener,
    timeout_ms: Option<u64>,
) -> Result<TcpStream, SysError> {
    let Some(timeout_ms) = timeout_ms else {
        return listener
            .accept()
            .map(|(stream, _)| stream)
            .map_err(|error| SysError::from_io("net.accept", error));
    };
    let deadline = net_timeout_deadline(timeout_ms, "net.accept")?;
    listener
        .set_nonblocking(true)
        .map_err(|error| SysError::from_io("net.accept", error))?;
    let result = loop {
        match listener.accept() {
            Ok((stream, _)) => break Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    break Err("net.accept: timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => break Err(format!("net.accept: {error}")),
        }
    };
    let restore = listener
        .set_nonblocking(false)
        .map_err(|error| SysError::from_io("net.accept: failed to restore blocking mode", error));
    match (result, restore) {
        (Ok(stream), Ok(())) => Ok(stream),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
    }
}

fn net_timeout_deadline(timeout_ms: u64, ctx: &str) -> Result<Instant, SysError> {
    validate_net_timeout_ms(timeout_ms, ctx)?;
    Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or_else(|| SysError::invalid_argument(format!("{ctx}: timeout_ms is too large")))
}

fn read_with_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    timeout_ms: Option<u64>,
) -> Result<usize, SysError> {
    let previous = stream
        .read_timeout()
        .map_err(|error| SysError::from_io("net.read", error))?;
    if let Some(timeout_ms) = timeout_ms {
        stream
            .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
            .map_err(|error| SysError::from_io("net.read", error))?;
    }
    let result = stream
        .read(buf)
        .map_err(|error| SysError::from_io("net.read", error));
    stream
        .set_read_timeout(previous)
        .map_err(|error| SysError::from_io("net.read: failed to restore timeout", error))?;
    result
}

fn write_all_with_timeout(
    stream: &mut TcpStream,
    bytes: &[u8],
    timeout_ms: Option<u64>,
) -> Result<(), SysError> {
    let previous = stream
        .write_timeout()
        .map_err(|error| SysError::from_io("net.write", error))?;
    if let Some(timeout_ms) = timeout_ms {
        stream
            .set_write_timeout(Some(Duration::from_millis(timeout_ms)))
            .map_err(|error| SysError::from_io("net.write", error))?;
    }
    let result = stream
        .write_all(bytes)
        .map_err(|error| SysError::from_io("net.write", error));
    stream
        .set_write_timeout(previous)
        .map_err(|error| SysError::from_io("net.write: failed to restore timeout", error))?;
    result
}

fn net_poll_state(state: &NetState, handles: &[i32], timeout_ms: i64) -> SysResult {
    #[cfg(unix)]
    {
        net_poll_unix(state, handles, timeout_ms)
    }
    #[cfg(not(unix))]
    {
        net_poll_portable(state, handles, timeout_ms)
    }
}

#[cfg(unix)]
fn net_poll_unix(state: &NetState, handles: &[i32], timeout_ms: i64) -> SysResult {
    let mut fds = Vec::new();
    let mut handle_kinds = Vec::new();
    for handle in handles {
        if let Some(listener) = state.listeners.get(handle) {
            fds.push(libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            handle_kinds.push((*handle, "listener"));
        } else if let Some(stream) = state.sockets.get(handle) {
            fds.push(libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN | libc::POLLOUT,
                revents: 0,
            });
            handle_kinds.push((*handle, "socket"));
        } else if let Some(udp) = state.udp_sockets.get(handle) {
            fds.push(libc::pollfd {
                fd: udp.as_raw_fd(),
                events: libc::POLLIN | libc::POLLOUT,
                revents: 0,
            });
            handle_kinds.push((*handle, "udp"));
        } else {
            return Err(SysError::not_found(format!(
                "net.poll: unknown net handle {handle}"
            )));
        }
    }
    // SAFETY: `fds` is a valid mutable slice of `pollfd` structs; `fds.len()` fits in `nfds_t`.
    let rc = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            fds.len() as libc::nfds_t,
            timeout_ms as i32,
        )
    };
    if rc < 0 {
        return Err(SysError::from_io(
            "net.poll",
            std::io::Error::last_os_error(),
        ));
    }
    let mut events = Vec::new();
    for (index, fd) in fds.iter().enumerate() {
        if fd.revents == 0 {
            continue;
        }
        let (handle, kind) = handle_kinds[index];
        events.push(net_poll_event_value(
            handle,
            kind,
            fd.revents & libc::POLLIN != 0,
            fd.revents & libc::POLLOUT != 0,
            fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0,
        ));
    }
    Ok(SysValue::List(events))
}

#[cfg(not(unix))]
fn net_poll_portable(state: &NetState, handles: &[i32], timeout_ms: i64) -> SysResult {
    // Validate all handles exist before touching any I/O.
    for handle in handles {
        if !state.listeners.contains_key(handle)
            && !state.sockets.contains_key(handle)
            && !state.udp_sockets.contains_key(handle)
        {
            return Err(SysError::not_found(format!(
                "net.poll: unknown net handle {handle}"
            )));
        }
    }

    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms as u64))
        .ok_or_else(|| "net.poll: timeout_ms is too large".to_string())?;

    // Poll loop: check socket readability using peek() with a 1 ms read timeout.
    // TcpListener readability cannot be tested without accepting a connection on
    // non-Unix (no WSAPoll available without windows-sys).  Listeners are omitted
    // from events; callers on Windows must use connect+read patterns instead.
    loop {
        let mut events = Vec::new();

        for handle in handles {
            if state.listeners.contains_key(handle) {
                // Cannot check listener readability portably — skip.
            } else if let Some(stream) = state.sockets.get(handle) {
                // Save the previous timeout so we can restore it unconditionally.
                let prev = stream.read_timeout().unwrap_or(None);
                if stream
                    .set_read_timeout(Some(Duration::from_millis(1)))
                    .is_err()
                {
                    // If we can't set the timeout, report an error event and move on.
                    events.push(net_poll_event_value(*handle, "socket", false, false, true));
                    continue;
                }
                let mut buf = [0u8; 1];
                let (readable, is_error) = match stream.peek(&mut buf) {
                    // Ok(0) = EOF: the socket is closed by the peer.  A subsequent
                    // read() will return 0.  Report readable=false, error=true so the
                    // caller can distinguish "data available" from "peer closed".
                    Ok(0) => (false, true),
                    Ok(_) => (true, false),
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        (false, false)
                    }
                    Err(_) => (false, true),
                };
                // Restore the original timeout; leave the socket clean even on error.
                let _ = stream.set_read_timeout(prev);
                events.push(net_poll_event_value(
                    *handle, "socket", readable, !is_error, is_error,
                ));
            }
        }

        if !events.is_empty() || Instant::now() >= deadline {
            return Ok(SysValue::List(events));
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}

fn net_poll_event_value(
    handle: i32,
    kind: &str,
    readable: bool,
    writable: bool,
    error: bool,
) -> SysValue {
    SysValue::Map(HashMap::from([
        ("handle".to_string(), SysValue::Int(handle as i64)),
        ("kind".to_string(), SysValue::Str(kind.to_string())),
        ("readable".to_string(), SysValue::Bool(readable)),
        ("writable".to_string(), SysValue::Bool(writable)),
        ("error".to_string(), SysValue::Bool(error)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_loopback_permission_denied(error: &str) -> bool {
        error.contains("Operation not permitted") || error.contains("Permission denied")
    }

    fn listen_or_skip(state: &mut NetState, args: SysArgs, test_name: &str) -> Option<SysValue> {
        match invoke(state, "listen", args) {
            Ok(value) => Some(value),
            Err(error) if is_loopback_permission_denied(&error) => {
                eprintln!("skipping {test_name}: loopback sockets are not permitted");
                None
            }
            Err(error) => panic!("{test_name}: {error}"),
        }
    }

    #[test]
    fn handle_allocator_ids_are_within_expected_ranges() {
        let state = NetState::new();
        let lid = allocate_net_handle(&state.listeners, LISTENER_HANDLE_BASE, "listener").unwrap();
        let sid = allocate_net_handle(&state.sockets, SOCKET_HANDLE_BASE, "socket").unwrap();
        assert!(lid > LISTENER_HANDLE_BASE && lid <= LISTENER_HANDLE_BASE + MAX_NET_HANDLES as i32);
        assert!(sid > SOCKET_HANDLE_BASE && sid <= SOCKET_HANDLE_BASE + MAX_NET_HANDLES as i32);
        // Listener and socket ID ranges must not overlap.
        assert_ne!(lid, sid);
    }

    #[test]
    fn host_port_formats_ipv4_and_ipv6_like_host_surface() {
        let mut state = NetState::new();
        assert_eq!(
            invoke(
                &mut state,
                "host_port",
                SysArgs(vec![SysValue::Str("127.0.0.1".into()), SysValue::Int(80),])
            )
            .unwrap(),
            SysValue::Str("127.0.0.1:80".into())
        );
        assert_eq!(
            invoke(
                &mut state,
                "host_port",
                SysArgs(vec![SysValue::Str("::1".into()), SysValue::Int(8080)])
            )
            .unwrap(),
            SysValue::Str("[::1]:8080".into())
        );
    }

    #[test]
    fn host_port_rejects_invalid_port() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "host_port",
            SysArgs(vec![
                SysValue::Str("127.0.0.1".into()),
                SysValue::Int(70000),
            ]),
        )
        .unwrap_err();
        assert!(error.contains("port must be in 0..=65535"));
    }

    #[test]
    fn is_loopback_rejects_malformed_ip_like_host_surface() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "is_loopback",
            SysArgs(vec![SysValue::Str("not_ip".into())]),
        )
        .unwrap_err();
        assert!(error.contains("net.is_loopback"));
    }

    #[test]
    fn connect_rejects_invalid_port_before_socket_call() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "connect",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(-1)),
            ]))]),
        )
        .unwrap_err();
        assert!(error.contains("net.connect spec.port: port must be in 0..=65535"));
    }

    #[test]
    fn listen_rejects_malformed_reuse_addr() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "listen",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(0)),
                ("reuse_addr".into(), SysValue::Str("yes".into())),
            ]))]),
        )
        .unwrap_err();
        assert!(error.contains("net.listen spec.reuse_addr: field must be bool"));
    }

    #[test]
    fn listen_rejects_malformed_backlog_before_socket_call() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "listen",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(0)),
                ("backlog".into(), SysValue::Int(0)),
            ]))]),
        )
        .unwrap_err();
        assert!(error.contains("net.listen spec.backlog: field must be a positive int"));

        let error = invoke(
            &mut state,
            "listen",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(0)),
                ("backlog".into(), SysValue::Str("many".into())),
            ]))]),
        )
        .unwrap_err();
        assert!(error.contains("net.listen spec.backlog: field must be int"));
    }

    #[test]
    fn listen_accepts_reuse_addr_before_bind() {
        let mut state = NetState::new();
        let Some(handle) = listen_or_skip(
            &mut state,
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(0)),
                ("reuse_addr".into(), SysValue::Bool(true)),
                ("backlog".into(), SysValue::Int(1)),
            ]))]),
            "listen_accepts_reuse_addr_before_bind",
        ) else {
            return;
        };
        let SysValue::Int(handle) = handle else {
            panic!("expected listener handle");
        };

        invoke(&mut state, "close", SysArgs(vec![SysValue::Int(handle)])).unwrap();
    }

    #[test]
    fn connect_rejects_malformed_timeout() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "connect",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(1)),
                ("timeout_ms".into(), SysValue::Int(-1)),
            ]))]),
        )
        .unwrap_err();
        assert!(error.contains("timeout_ms: field must be non-negative"));
    }

    #[test]
    fn accept_timeout_returns_without_connection() {
        let mut state = NetState::new();
        let Some(handle) = listen_or_skip(
            &mut state,
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("host".into(), SysValue::Str("127.0.0.1".into())),
                ("port".into(), SysValue::Int(0)),
                ("backlog".into(), SysValue::Int(1)),
            ]))]),
            "accept_timeout_returns_without_connection",
        ) else {
            return;
        };
        let SysValue::Int(handle) = handle else {
            panic!("expected listener handle");
        };

        let error = invoke(
            &mut state,
            "accept",
            SysArgs(vec![SysValue::Int(handle), SysValue::Int(1)]),
        )
        .unwrap_err();

        assert!(error.contains("net.accept: timed out"));
        invoke(&mut state, "close", SysArgs(vec![SysValue::Int(handle)])).unwrap();
    }

    #[test]
    fn accept_rejects_timeout_deadline_overflow() {
        let error = net_timeout_deadline(u64::MAX, "net.accept").unwrap_err();
        assert!(error.contains("net.accept: exceeds system timeout range"));
    }

    #[test]
    fn read_rejects_negative_max_bytes_before_handle_lookup() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "read",
            SysArgs(vec![SysValue::Int(1), SysValue::Int(-1)]),
        )
        .unwrap_err();
        assert!(error.contains("max_bytes must be a non-negative int"));
    }

    #[test]
    fn read_rejects_malformed_timeout_before_handle_lookup() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "read",
            SysArgs(vec![SysValue::Int(1), SysValue::Int(1), SysValue::Int(-1)]),
        )
        .unwrap_err();
        assert!(error.contains("net.read timeout_ms: must be non-negative"));
    }

    #[test]
    fn read_rejects_invalid_utf8_like_host_surface() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "skipping read_rejects_invalid_utf8_like_host_surface: loopback sockets are not permitted"
                );
                return;
            }
            Err(error) => panic!("read_rejects_invalid_utf8_like_host_surface: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        client.write_all(&[0xff]).unwrap();
        let mut state = NetState::new();
        let handle = state.store_socket(server).unwrap();

        let error = invoke(
            &mut state,
            "read",
            SysArgs(vec![SysValue::Int(handle as i64), SysValue::Int(1)]),
        )
        .unwrap_err();

        assert!(error.contains("net.read: invalid utf-8 sequence"));
    }

    #[test]
    fn socket_operations_reject_handle_values_outside_i32_range() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "close",
            SysArgs(vec![SysValue::Int(i64::from(i32::MAX) + 1)]),
        )
        .unwrap_err();
        assert!(error.contains("handle out of i32 range"));
    }

    #[test]
    fn poll_rejects_timeout_values_outside_system_range() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "poll",
            SysArgs(vec![
                SysValue::List(Vec::new()),
                SysValue::Int(i64::from(i32::MAX) + 1),
            ]),
        )
        .unwrap_err();
        assert!(error.contains("timeout_ms exceeds system poll range"));
    }

    #[test]
    fn poll_rejects_unknown_handle() {
        let mut state = NetState::new();
        let error = invoke(
            &mut state,
            "poll",
            SysArgs(vec![
                SysValue::List(vec![SysValue::Int(123)]),
                SysValue::Int(0),
            ]),
        )
        .unwrap_err();
        assert!(error.contains("unknown net handle 123"));
    }

    #[test]
    fn resolve_returns_literal_socket_addr() {
        let mut state = NetState::new();
        let SysValue::List(addrs) = invoke(
            &mut state,
            "resolve",
            SysArgs(vec![SysValue::Str("127.0.0.1:80".into())]),
        )
        .unwrap() else {
            panic!("expected list of addresses");
        };
        assert!(addrs.contains(&SysValue::Str("127.0.0.1:80".into())));
    }

    #[test]
    fn udp_send_recv_round_trips_over_loopback() {
        let mut state = NetState::new();
        let bind = |state: &mut NetState| {
            invoke(
                state,
                "udp_bind",
                SysArgs(vec![SysValue::Map(HashMap::from([
                    ("host".into(), SysValue::Str("127.0.0.1".into())),
                    ("port".into(), SysValue::Int(0)),
                ]))]),
            )
        };
        let a = match bind(&mut state) {
            Ok(SysValue::Int(h)) => h,
            Err(error) if is_loopback_permission_denied(&error) => {
                eprintln!("skipping udp test: loopback sockets are not permitted");
                return;
            }
            other => panic!("udp-bind a: {other:?}"),
        };
        let b = match bind(&mut state) {
            Ok(SysValue::Int(h)) => h,
            other => panic!("udp-bind b: {other:?}"),
        };
        let SysValue::Str(addr_b) =
            invoke(&mut state, "local_addr", SysArgs(vec![SysValue::Int(b)])).unwrap()
        else {
            panic!("expected addr string");
        };

        invoke(
            &mut state,
            "udp_send_to",
            SysArgs(vec![
                SysValue::Int(a),
                SysValue::Str("ping".into()),
                SysValue::Str(addr_b),
            ]),
        )
        .unwrap();

        let SysValue::Map(received) = invoke(
            &mut state,
            "udp_recv_from",
            SysArgs(vec![
                SysValue::Int(b),
                SysValue::Int(16),
                SysValue::Int(2000),
            ]),
        )
        .unwrap() else {
            panic!("expected recv map");
        };
        assert_eq!(received.get("data"), Some(&SysValue::Str("ping".into())));
        assert!(received.contains_key("from"));

        invoke(&mut state, "close", SysArgs(vec![SysValue::Int(a)])).unwrap();
        invoke(&mut state, "close", SysArgs(vec![SysValue::Int(b)])).unwrap();
    }

    #[test]
    fn poll_accepts_empty_handle_list() {
        let mut state = NetState::new();
        assert_eq!(
            invoke(
                &mut state,
                "poll",
                SysArgs(vec![SysValue::List(Vec::new()), SysValue::Int(0)])
            )
            .unwrap(),
            SysValue::List(Vec::new())
        );
    }
}
