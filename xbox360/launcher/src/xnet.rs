//! Retail-safe XAM NetDll networking with SO_INSECURE (0x5801) for title mode.

use std::mem::size_of;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use launcher_core::guard::parse_http_response;
use xbox360_sys::xam::{
    self, AddressFamily, FdSet, IoctlCommand, MessageFlags, Protocol, SOCKET_ERROR, SockAddrIn,
    Socket, SocketLevel, SocketOption, SocketType, TimeVal, WinsockVersion, WsaData, XnCaller,
    XnetStartupFlags, XnetStartupParams,
};

pub const SO_INSECURE: SocketOption = SocketOption(0x5801);
const WSAEWOULDBLOCK: u32 = 10035;
const WSAEINPROGRESS: u32 = 10036;
const MAX_CONSECUTIVE_DIAG_FAILS: u32 = 2;

static NET_INIT: Mutex<Option<(u32, i32, u32)>> = Mutex::new(None);
static DIAG_CONSECUTIVE_FAILS: AtomicU32 = AtomicU32::new(0);

/// Initializes XNet and Winsock once WSAStartup succeeds for `XnCaller::Title`.
/// Returns `(ethernet_link_bits, xnet_startup_rc, wsa_startup_code)`.
pub fn init_once() -> (u32, i32, u32) {
    let mut cached = NET_INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(res) = *cached
        && res.2 == 0
    {
        return res;
    }

    let link = xam::NetDll_XNetGetEthernetLinkStatus(XnCaller::Title).bits();

    let mut xnet = XnetStartupParams::new();
    xnet.flags = XnetStartupFlags::BYPASS_SECURITY;
    // SAFETY: `xnet` is a live stack struct with `size` initialized to `size_of::<Self>()`.
    let mut xnet_rc = unsafe { xam::NetDll_XNetStartup(XnCaller::Title, &xnet) };
    if xnet_rc != 0 {
        xnet.flags = XnetStartupFlags::empty();
        // SAFETY: fallback retry without devkit bypass flag on retail kernels.
        xnet_rc = unsafe { xam::NetDll_XNetStartup(XnCaller::Title, &xnet) };
    }

    let mut wsa = WsaData::new();
    // SAFETY: `wsa` is a writable stack buffer matching `WsaData` layout.
    let wsa_rc = unsafe {
        xam::NetDll_WSAStartup(XnCaller::Title, WinsockVersion::new(2, 2), &mut wsa)
    }
    .0;

    let res = (link, xnet_rc, wsa_rc);
    if wsa_rc == 0 {
        *cached = Some(res);
    }
    res
}

pub struct SocketGuard(pub Socket);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.0 != Socket::INVALID {
            // SAFETY: `self.0` is an owned socket handle not used after `drop`.
            unsafe {
                let _ = xam::NetDll_closesocket(XnCaller::Title, self.0);
            }
        }
    }
}

/// Performs a single HTTP/1.x request-response over XAM `NetDll_*` sockets with
/// `SO_INSECURE` (0x5801) enabled so plain LAN servers work on retail 17559.
pub fn http_exchange(
    ip: [u8; 4],
    port: u16,
    req: &[u8],
    max_body_bytes: usize,
    connect_timeout_ms: u32,
) -> Result<(u16, Vec<u8>), String> {
    let _ = init_once();

    // SAFETY: creates a title TCP socket after `init_once` initializes XNet/WSA.
    let sock = unsafe {
        xam::NetDll_socket(
            XnCaller::Title,
            AddressFamily::Inet,
            SocketType::Stream,
            Protocol::Tcp,
        )
    };
    if sock == Socket::INVALID {
        let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
        return Err(format!("socket: wsa={wsa}"));
    }
    let guard = SocketGuard(sock);

    let one: u32 = 1;
    let rcv_timeout: u32 = 4000;
    let snd_timeout: u32 = 2000;
    // SAFETY: stack locals outlive each `NetDll_setsockopt` call on `guard.0`.
    unsafe {
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            guard.0,
            SocketLevel::Socket,
            SO_INSECURE,
            (&raw const one).cast::<u8>(),
            size_of::<u32>() as i32,
        );
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            guard.0,
            SocketLevel::Socket,
            SocketOption::SO_RCVTIMEO,
            (&raw const rcv_timeout).cast::<u8>(),
            size_of::<u32>() as i32,
        );
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            guard.0,
            SocketLevel::Socket,
            SocketOption::SO_SNDTIMEO,
            (&raw const snd_timeout).cast::<u8>(),
            size_of::<u32>() as i32,
        );
    }

    let mut nonblocking: u32 = 1;
    // SAFETY: `nonblocking` is a live `u32` passed to `FIONBIO`.
    let nb_rc = unsafe {
        xam::NetDll_ioctlsocket(XnCaller::Title, guard.0, IoctlCommand::Fionbio, &mut nonblocking)
    };
    if nb_rc == SOCKET_ERROR {
        let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
        return Err(format!("fionbio(1): wsa={wsa}"));
    }

    let sa = SockAddrIn {
        family: AddressFamily::Inet as u32 as u16,
        port,
        address: u32::from_be_bytes(ip),
        zero: [0; 8],
    };
    // SAFETY: `sa` is a live `SockAddrIn` matching `size_of::<SockAddrIn>()`.
    let conn_rc = unsafe {
        xam::NetDll_connect(
            XnCaller::Title,
            guard.0,
            (&raw const sa).cast::<u8>(),
            size_of::<SockAddrIn>() as i32,
        )
    };
    if conn_rc == SOCKET_ERROR {
        let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
        if wsa != WSAEWOULDBLOCK && wsa != WSAEINPROGRESS {
            return Err(format!("connect: wsa={wsa}"));
        }

        let mut writable = FdSet::new();
        writable.count = 1;
        writable.sockets[0] = guard.0;
        let mut failed = FdSet::new();
        failed.count = 1;
        failed.sockets[0] = guard.0;
        let timeout = TimeVal::from_milliseconds(connect_timeout_ms.max(1));

        // SAFETY: `writable`, `failed`, and `timeout` are live stack structs.
        let ready = unsafe {
            xam::NetDll_select(
                XnCaller::Title,
                1,
                core::ptr::null_mut(),
                &mut writable,
                &mut failed,
                &timeout,
            )
        };
        if ready == SOCKET_ERROR {
            let sel_wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
            return Err(format!("select: wsa={sel_wsa}"));
        }
        if ready == 0 {
            return Err(String::from("connect timed out"));
        }
        if failed.count != 0 {
            return Err(String::from("connect refused or unreachable"));
        }
        if writable.count == 0 {
            return Err(String::from("connect did not become writable"));
        }
    }

    let mut blocking: u32 = 0;
    // SAFETY: `blocking` is a live `u32` restoring blocking mode for `send`/`recv`.
    let blk_rc = unsafe {
        xam::NetDll_ioctlsocket(XnCaller::Title, guard.0, IoctlCommand::Fionbio, &mut blocking)
    };
    if blk_rc == SOCKET_ERROR {
        let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
        return Err(format!("fionbio(0): wsa={wsa}"));
    }

    let mut sent = 0usize;
    while sent < req.len() {
        let remaining = (req.len() - sent).min(i32::MAX as usize) as i32;
        // SAFETY: `req[sent..]` is readable for `remaining` bytes.
        let n = unsafe {
            xam::NetDll_send(
                XnCaller::Title,
                guard.0,
                req[sent..].as_ptr(),
                remaining,
                MessageFlags::default(),
            )
        };
        if n <= 0 {
            let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
            return Err(format!("send: n={n} wsa={wsa}"));
        }
        sent += n as usize;
    }

    let mut raw = Vec::new();
    let mut chunk = vec![0u8; 4096];
    let limit = max_body_bytes.saturating_add(4096);
    loop {
        // SAFETY: `chunk` is writable for `chunk.len()` bytes.
        let n = unsafe {
            xam::NetDll_recv(
                XnCaller::Title,
                guard.0,
                chunk.as_mut_ptr(),
                chunk.len() as i32,
                MessageFlags::default(),
            )
        };
        if n == 0 {
            break;
        }
        if n < 0 {
            let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
            return Err(format!("recv: wsa={wsa}"));
        }
        let got = n as usize;
        if raw.len().saturating_add(got) > limit {
            return Err(String::from("HTTP response too large"));
        }
        raw.extend_from_slice(&chunk[..got]);
    }

    let (code, body) = parse_http_response(&raw)?;
    DIAG_CONSECUTIVE_FAILS.store(0, Ordering::Relaxed);
    Ok((code, body.to_vec()))
}

fn percent_encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Best-effort diagnostic beacon sent to `xbox-guard`. Never panics and stops
/// sending after consecutive offline failures to avoid stalling offline startup.
pub fn send_diag(ip: [u8; 4], port: u16, stage: &str, detail: &str) {
    if DIAG_CONSECUTIVE_FAILS.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_DIAG_FAILS {
        return;
    }
    let stage_enc = percent_encode_component(stage);
    let detail_enc = percent_encode_component(detail);
    let req = format!(
        "GET /api/v1/launcher/diag?stage={stage_enc}&detail={detail_enc} HTTP/1.0\r\nHost: {}.{}.{}.{}:{port}\r\nConnection: close\r\n\r\n",
        ip[0], ip[1], ip[2], ip[3]
    );
    match http_exchange(ip, port, req.as_bytes(), 512, 600) {
        Ok(_) => DIAG_CONSECUTIVE_FAILS.store(0, Ordering::Relaxed),
        Err(_) => {
            let _ = DIAG_CONSECUTIVE_FAILS.fetch_add(1, Ordering::Relaxed);
        }
    }
}
