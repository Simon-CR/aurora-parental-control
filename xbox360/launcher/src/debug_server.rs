//! Embedded live TCP remote debug console (`SHELFDBG/1` on port 7331).
//! Runs on retail 17559 consoles using `NetDll_*` + `SO_INSECURE (0x5801)` so
//! hardware/display state remains inspectable and recoverable even if video stalls.

use std::fmt::Write as _;
use std::mem::size_of;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use launcher_core::debug_proto::{
    BANNER, DEFAULT_DEBUG_PORT, DebugCommand, MAX_CMD_BYTES, RenderOverride,
    decode_render_override, encode_render_override, format_frame, help_text, parse_command,
};
use launcher_core::launch_plan::plan_aurora_launch;
use xbox360::console::{FirmwareRequest, return_to_firmware};
use xbox360::title::return_to_dashboard;
use xbox360_sys::mmio::GpuRegisters;
use xbox360_sys::xam::{
    self, AddressFamily, FdSet, IoctlCommand, MessageFlags, Protocol, SOCKET_ERROR, ShutdownHow,
    SockAddrIn, Socket, SocketLevel, SocketOption, SocketType, TimeVal, XnCaller,
};
use xbox360_sys::xboxkrnl::{
    DisplayInformation, VdEnumerateVideoModes, VdGetCurrentDisplayInformation,
    VdReadDVERegisterUlong, VdSetDisplayMode, VideoModeInfo,
};

use crate::diag;
use crate::launch;
use crate::xnet::{self, SO_INSECURE, SocketGuard};

const REMOTE_NONE: u32 = 0;
const REMOTE_DASH: u32 = 1;
const REMOTE_AURORA: u32 = 2;
const MAX_ENUM_MODES: usize = 32;
const SAFE_GPU_REGS: &[(u16, &str)] = &[
    (0x01c0, "CP_RB_BASE"),
    (0x01c1, "CP_RB_CNTL"),
    (0x01c4, "CP_RB_RPTR"),
    (0x01c5, "CP_RB_WPTR"),
    (0x05c8, "PIPELINE_05C8"),
    (0x05d0, "GPU_STATUS"),
    (0x0a2f, "COHERENCY_SIZE"),
    (0x0a31, "COHERENCY_STAT"),
    (0x0d02, "ENGINE_0D02"),
    (0x1841, "SCANOUT_CTRL"),
    (0x1844, "SCANOUT_ADDR"),
    (0x1851, "SCANOUT_UPD"),
    (0x1930, "SCANOUT_FMT"),
];

static RENDER_OVERRIDE: AtomicU32 = AtomicU32::new(0);
static REMOTE_ACTION: AtomicU32 = AtomicU32::new(REMOTE_NONE);
static REMOTE_ACK: AtomicBool = AtomicBool::new(false);
static SPAWNED: AtomicBool = AtomicBool::new(false);
static LISTENING: AtomicBool = AtomicBool::new(false);
static AURORA_PATH: Mutex<String> = Mutex::new(String::new());

pub fn is_spawned() -> bool {
    SPAWNED.load(Ordering::Acquire)
}

pub fn mark_down_on_panic() {
    LISTENING.store(false, Ordering::Release);
    SPAWNED.store(false, Ordering::Release);
}

pub fn render_override() -> RenderOverride {
    decode_render_override(RENDER_OVERRIDE.load(Ordering::Relaxed))
}

pub fn set_render_override(mode: RenderOverride) {
    RENDER_OVERRIDE.store(encode_render_override(mode), Ordering::Relaxed);
}

pub fn take_remote_action() -> u32 {
    REMOTE_ACTION.swap(REMOTE_NONE, Ordering::AcqRel)
}

pub fn acknowledge_remote_action() {
    REMOTE_ACK.store(true, Ordering::Release);
}

pub fn reset_control_state() {
    RENDER_OVERRIDE.store(0, Ordering::Release);
    REMOTE_ACTION.store(REMOTE_NONE, Ordering::Release);
    REMOTE_ACK.store(false, Ordering::Release);
    LISTENING.store(false, Ordering::Release);
}

pub struct DebugServer {
    shutdown: std::sync::Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl DebugServer {
    pub fn spawn(aurora_path: String) -> Self {
        diag::clear_debug_panicked();
        reset_control_state();
        if let Ok(mut guard) = AURORA_PATH.lock() {
            *guard = aurora_path;
        }
        SPAWNED.store(true, Ordering::Release);
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&shutdown);
        let join = thread::Builder::new()
            .name(String::from("shelf-debug"))
            .stack_size(256 * 1024)
            .spawn(move || server_loop(&flag))
            .ok();
        Self { shutdown, join }
    }

    pub fn shutdown(mut self) {
        self.shutdown.store(true, Ordering::Release);
        SPAWNED.store(false, Ordering::Release);
        LISTENING.store(false, Ordering::Release);
        if let Some(handle) = self.join.take()
            && !diag::debug_panicked()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for DebugServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        SPAWNED.store(false, Ordering::Release);
        LISTENING.store(false, Ordering::Release);
        if let Some(handle) = self.join.take()
            && !diag::debug_panicked()
        {
            let _ = handle.join();
        }
    }
}

#[derive(Clone, Copy)]
struct CapturedMode {
    width: u16,
    height: u16,
    kind: u8,
    refresh_rate: f32,
    flags: u32,
}

struct ModeList {
    items: [CapturedMode; MAX_ENUM_MODES],
    count: usize,
    total_seen: usize,
}

impl ModeList {
    fn new() -> Self {
        Self {
            items: [CapturedMode {
                width: 0,
                height: 0,
                kind: 0,
                refresh_rate: 0.0,
                flags: 0,
            }; MAX_ENUM_MODES],
            count: 0,
            total_seen: 0,
        }
    }
}

#[repr(C, packed(2))]
#[derive(Clone, Copy)]
struct RawVideoModeInfo {
    _reserved_000: u16,
    width: u16,
    height: u16,
    _reserved_006: u8,
    kind: u8,
    refresh_rate: f32,
    flags: u32,
}

const _: () = {
    assert!(core::mem::size_of::<RawVideoModeInfo>() == 0x10);
    assert!(core::mem::align_of::<RawVideoModeInfo>() == 2);
    assert!(core::mem::offset_of!(RawVideoModeInfo, width) == 2);
    assert!(core::mem::offset_of!(RawVideoModeInfo, kind) == 7);
    assert!(core::mem::offset_of!(RawVideoModeInfo, refresh_rate) == 8);
    assert!(core::mem::offset_of!(RawVideoModeInfo, flags) == 12);
};

xbox360::sys::bridged!(enum_modes_trampoline, enum_modes_cb);

extern "C" fn enum_modes_cb(mode: *const VideoModeInfo, ctx: *mut ModeList) -> u32 {
    if mode.is_null() || ctx.is_null() {
        return 0;
    }
    // SAFETY: Kernel passes `_VD_VIDEO_MODE_INFO` (0x16 bytes packed to 2-byte alignment,
    // so odd elements are 2-byte aligned); `read_unaligned` prevents UB on 4-byte fields.
    // `ctx` points to `ModeList` on `collect_modes` stack through the synchronous call.
    unsafe {
        let raw = core::ptr::read_unaligned(mode.cast::<RawVideoModeInfo>());
        let list = &mut *ctx;
        list.total_seen = list.total_seen.saturating_add(1);
        if list.count < MAX_ENUM_MODES {
            list.items[list.count] = CapturedMode {
                width: raw.width,
                height: raw.height,
                kind: raw.kind,
                refresh_rate: raw.refresh_rate,
                flags: raw.flags,
            };
            list.count += 1;
        }
    }
    1
}

fn collect_modes() -> ModeList {
    let mut modes = ModeList::new();
    let cb_addr = (enum_modes_trampoline as unsafe extern "C" fn()) as *const () as usize as u32;
    let ctx_addr = (&raw mut modes) as usize as u32;
    // SAFETY: `enum_modes_trampoline` preserves console ABI r14-r31 via `bridged!`
    // and reads `modes` exclusively before `VdEnumerateVideoModes` returns.
    unsafe { VdEnumerateVideoModes(cb_addr, ctx_addr) };
    modes
}

fn sleep_interruptible(shutdown: &AtomicBool, total_ms: u64) -> bool {
    let mut elapsed = 0u64;
    while elapsed < total_ms {
        if shutdown.load(Ordering::Acquire) {
            return true;
        }
        let step = (total_ms - elapsed).min(50);
        thread::sleep(Duration::from_millis(step));
        elapsed += step;
    }
    shutdown.load(Ordering::Acquire)
}

fn configure_socket(sock: Socket, is_listener: bool) {
    let one: u32 = 1;
    // SAFETY: stack locals outlive `NetDll_setsockopt`. Note: `SO_KEEPALIVE` and
    // `SO_ERROR` are intentionally avoided as retail XAM rejects them with WSAENOPROTOOPT.
    unsafe {
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            sock,
            SocketLevel::Socket,
            SO_INSECURE,
            (&raw const one).cast::<u8>(),
            size_of::<u32>() as i32,
        );
        if is_listener {
            let _ = xam::NetDll_setsockopt(
                XnCaller::Title,
                sock,
                SocketLevel::Socket,
                SocketOption::SO_REUSEADDR,
                (&raw const one).cast::<u8>(),
                size_of::<u32>() as i32,
            );
        } else {
            let mut blocking: u32 = 0;
            let _ = xam::NetDll_ioctlsocket(
                XnCaller::Title,
                sock,
                IoctlCommand::Fionbio,
                &mut blocking,
            );
            let snd_timeout: u32 = 2000;
            let _ = xam::NetDll_setsockopt(
                XnCaller::Title,
                sock,
                SocketLevel::Socket,
                SocketOption::SO_SNDTIMEO,
                (&raw const snd_timeout).cast::<u8>(),
                size_of::<u32>() as i32,
            );
        }
    }
}

fn bind_listener() -> Result<SocketGuard, String> {
    let (_, xnet_rc, wsa_rc) = xnet::init_once();
    if wsa_rc != 0 {
        return Err(format!("wsa={wsa_rc} xnet={xnet_rc}"));
    }

    // SAFETY: creates title TCP listener after Winsock initialization.
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
        return Err(format!("socket wsa={wsa}"));
    }
    let guard = SocketGuard(sock);
    configure_socket(guard.0, true);
    let mut nonblocking: u32 = 1;
    // SAFETY: `nonblocking` stays live across `FIONBIO` so `accept` never blocks on race/RST.
    unsafe {
        let _ = xam::NetDll_ioctlsocket(
            XnCaller::Title,
            guard.0,
            IoctlCommand::Fionbio,
            &mut nonblocking,
        );
    }

    // Bind `INADDR_ANY` (`0`): binding loopback asserts on Xbox 360 network stack.
    let sa = SockAddrIn {
        family: AddressFamily::Inet as u32 as u16,
        port: DEFAULT_DEBUG_PORT.to_be(),
        address: 0,
        zero: [0; 8],
    };
    // SAFETY: `sa` lives through `NetDll_bind`.
    let bind_rc = unsafe {
        xam::NetDll_bind(
            XnCaller::Title,
            guard.0,
            (&raw const sa).cast::<u8>(),
            size_of::<SockAddrIn>() as i32,
        )
    };
    if bind_rc == SOCKET_ERROR {
        let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
        return Err(format!("bind wsa={wsa}"));
    }

    // SAFETY: `guard.0` is bound to `INADDR_ANY:7331`.
    let listen_rc = unsafe { xam::NetDll_listen(XnCaller::Title, guard.0, 2) };
    if listen_rc == SOCKET_ERROR {
        let wsa = xam::NetDll_WSAGetLastError(XnCaller::Title).0;
        return Err(format!("listen wsa={wsa}"));
    }

    Ok(guard)
}

fn server_loop(shutdown: &AtomicBool) {
    diag::register_thread(diag::ThreadRole::Debug);
    let mut logged_listen = false;
    let mut logged_bind_err = false;
    while !shutdown.load(Ordering::Acquire) {
        let listener = match bind_listener() {
            Ok(sock) => {
                logged_bind_err = false;
                sock
            }
            Err(err) => {
                LISTENING.store(false, Ordering::Release);
                if !logged_bind_err {
                    logged_bind_err = true;
                    diag::stage("02b_debug_bind_wait", &err);
                }
                if sleep_interruptible(shutdown, 500) {
                    break;
                }
                continue;
            }
        };

        LISTENING.store(true, Ordering::Release);
        if !logged_listen {
            logged_listen = true;
            diag::stage(
                "02b_debug_listen",
                &format!("tcp/{DEFAULT_DEBUG_PORT} listening"),
            );
        }

        let mut select_errs = 0u32;
        while !shutdown.load(Ordering::Acquire) {
            let mut readable = FdSet::new();
            readable.count = 1;
            readable.sockets[0] = listener.0;
            let timeout = TimeVal::from_milliseconds(200);
            // SAFETY: stack locals live for `NetDll_select`.
            let ready = unsafe {
                xam::NetDll_select(
                    XnCaller::Title,
                    1,
                    &mut readable,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &timeout,
                )
            };
            if ready == 0 {
                select_errs = 0;
                continue;
            }
            if ready == SOCKET_ERROR {
                select_errs = select_errs.saturating_add(1);
                if select_errs >= 5 || sleep_interruptible(shutdown, 100) {
                    break;
                }
                continue;
            }
            select_errs = 0;
            if readable.count == 0 {
                continue;
            }

            let mut peer = [0u8; 16];
            let mut peer_len = peer.len() as i32;
            // SAFETY: `listener.0` signalled readable; `peer` holds `SockAddrIn`.
            let client = unsafe {
                xam::NetDll_accept(
                    XnCaller::Title,
                    listener.0,
                    peer.as_mut_ptr(),
                    &mut peer_len,
                )
            };
            if client != Socket::INVALID {
                let client_guard = SocketGuard(client);
                handle_client(client_guard, shutdown);
            }
        }
        LISTENING.store(false, Ordering::Release);
    }
}

fn send_all(sock: Socket, bytes: &[u8]) -> bool {
    let mut sent = 0usize;
    while sent < bytes.len() {
        let remaining = (bytes.len() - sent).min(i32::MAX as usize) as i32;
        // SAFETY: `bytes[sent..]` is readable for `remaining` bytes.
        let n = unsafe {
            xam::NetDll_send(
                XnCaller::Title,
                sock,
                bytes[sent..].as_ptr(),
                remaining,
                MessageFlags::default(),
            )
        };
        if n <= 0 {
            return false;
        }
        sent += n as usize;
    }
    true
}

fn close_gracefully(mut guard: SocketGuard) {
    let sock = guard.0;
    if sock != Socket::INVALID {
        // SAFETY: flushes TCP stream before `SocketGuard::drop` closes handle.
        unsafe {
            let _ = xam::NetDll_shutdown(XnCaller::Title, sock, ShutdownHow::Both);
            let _ = xam::NetDll_closesocket(XnCaller::Title, sock);
        }
        guard.0 = Socket::INVALID;
    }
}

fn read_vd_state() -> (DisplayInformation, u32, u32) {
    let mut info = DisplayInformation::new();
    // SAFETY: `info` is an aligned 0x58-byte block on stack.
    unsafe { VdGetCurrentDisplayInformation(info.as_mut_ptr()) };
    let regs = GpuRegisters::get();
    let ctrl = regs.display_graphics_control().read();
    let addr = regs.display_primary_surface().read();
    (info, ctrl, addr)
}

fn format_status() -> String {
    let snap = diag::snapshot();
    let (vd, ctrl, addr) = read_vd_state();
    let regs = GpuRegisters::get();
    let status = regs.status().read().bits();
    let rptr = regs.register(0x01c4).read();
    let wptr = regs.register(0x01c5).read();
    let upd = regs.register(0x1851).read();
    let fmt = regs.register(0x1930).read();
    let r0d02 = regs.register(0x0d02).read();
    let r05c8 = regs.register(0x05c8).read();
    let link = xam::NetDll_XNetGetEthernetLinkStatus(XnCaller::Title).bits();
    let hb = match snap.heartbeat_age_ms {
        Some(age) => format!("{age}ms"),
        None => String::from("none"),
    };
    let panic_line = snap.panic_msg.as_deref().unwrap_or("none");

    format!(
        concat!(
            "uptime_ms={}\n",
            "stage={}\n",
            "detail={}\n",
            "frame={}\n",
            "heartbeat_age={}\n",
            "panicked={}\n",
            "panic_msg={}\n",
            "render_mode={}\n",
            "eth_link=0x{:x}\n",
            "vd_surface={}x{}\n",
            "vd_window={},{},{},{}\n",
            "vd_scaled={}x{}\n",
            "vd_colorspace={}\n",
            "gpu_status_05d0=0x{:08x}\n",
            "ring_rptr_01c4=0x{:08x}\n",
            "ring_wptr_01c5=0x{:08x}\n",
            "scanout_ctrl_1841=0x{:08x}\n",
            "scanout_addr_1844=0x{:08x}\n",
            "scanout_upd_1851=0x{:08x}\n",
            "scanout_fmt_1930=0x{:08x}\n",
            "reg_0d02=0x{:08x}\n",
            "reg_05c8=0x{:08x}"
        ),
        snap.uptime_ms,
        snap.stage,
        snap.detail,
        snap.frame,
        hb,
        snap.panicked,
        panic_line,
        render_override(),
        link,
        vd.surface_width,
        vd.surface_height,
        vd.left,
        vd.top,
        vd.right,
        vd.bottom,
        vd.scaled_width,
        vd.scaled_height,
        vd.colorspace,
        status,
        rptr,
        wptr,
        ctrl,
        addr,
        upd,
        fmt,
        r0d02,
        r05c8,
    )
}

fn format_gpu_regs() -> String {
    let regs = GpuRegisters::get();
    let mut out = String::new();
    for &(offset, label) in SAFE_GPU_REGS {
        let val = regs.register(offset).read();
        let _ = writeln!(out, "reg[0x{offset:04x}] ({label}) = 0x{val:08x}");
    }
    out
}

fn format_modes() -> String {
    let modes = collect_modes();
    let mut out = String::new();
    let _ = writeln!(out, "modes_count={} total_seen={}", modes.count, modes.total_seen);
    for idx in 0..modes.count {
        let m = modes.items[idx];
        let scan = if (m.flags & 0x4) != 0 { "interlaced" } else { "progressive" };
        let _ = writeln!(
            out,
            "[{idx}] {}x{} @ {:.2}Hz kind={} flags=0x{:08x} ({scan})",
            m.width, m.height, m.refresh_rate, m.kind, m.flags
        );
    }
    out
}

enum PostAction {
    Continue,
    Close,
    Dash { force: bool },
    Aurora { force: bool },
    Reboot,
}

fn handle_command(line: &str) -> (String, PostAction) {
    match parse_command(line) {
        Ok(None) => (String::new(), PostAction::Continue),
        Err(err) => (err, PostAction::Continue),
        Ok(Some(cmd)) => match cmd {
            DebugCommand::Help => (String::from(help_text()), PostAction::Continue),
            DebugCommand::Ping => {
                let snap = diag::snapshot();
                (
                    format!(
                        "pong uptime_ms={} stage={} frame={} mode={}",
                        snap.uptime_ms,
                        snap.stage,
                        snap.frame,
                        render_override()
                    ),
                    PostAction::Continue,
                )
            }
            DebugCommand::Status => (format_status(), PostAction::Continue),
            DebugCommand::Log { lines } => {
                let entries = diag::recent_lines(lines);
                let body = if entries.is_empty() {
                    String::from("(no log entries)")
                } else {
                    entries.join("\n")
                };
                (body, PostAction::Continue)
            }
            DebugCommand::Gpu => (format_gpu_regs(), PostAction::Continue),
            DebugCommand::Reg { offset } => {
                let val = GpuRegisters::get().register(offset).read();
                (format!("reg[0x{offset:04x}] = 0x{val:08x}"), PostAction::Continue)
            }
            DebugCommand::Dve { register } => {
                // SAFETY: invokes Xbox 360 kernel video encoder read import.
                let val = unsafe { VdReadDVERegisterUlong(register) };
                (format!("dve[0x{register:02x}] = 0x{val:08x}"), PostAction::Continue)
            }
            DebugCommand::Modes => (format_modes(), PostAction::Continue),
            DebugCommand::VdMode { flags } => {
                // SAFETY: caller explicitly requested kernel display-mode re-programming.
                unsafe { VdSetDisplayMode(flags) };
                let (vd, ctrl, addr) = read_vd_state();
                (
                    format!(
                        "OK VdSetDisplayMode(0x{flags:x}) surf={}x{} win={},{},{},{} ctrl=0x{ctrl:08x} addr=0x{addr:08x}",
                        vd.surface_width, vd.surface_height, vd.left, vd.top, vd.right, vd.bottom
                    ),
                    PostAction::Continue,
                )
            }
            DebugCommand::Solid { color } => {
                let mode = match color {
                    Some((r, g, b)) => RenderOverride::GpuSolid(r, g, b),
                    None => RenderOverride::Normal,
                };
                set_render_override(mode);
                (format!("OK render_mode={mode}"), PostAction::Continue)
            }
            DebugCommand::Cpu { color } => {
                let mode = match color {
                    Some((r, g, b)) => RenderOverride::CpuSolid(r, g, b),
                    None => RenderOverride::Normal,
                };
                set_render_override(mode);
                (format!("OK render_mode={mode}"), PostAction::Continue)
            }
            DebugCommand::Dash { force } => {
                let mode_tag = if force { "forced" } else { "clean" };
                (format!("OK returning to dashboard ({mode_tag})"), PostAction::Dash { force })
            }
            DebugCommand::Aurora { force } => {
                let mode_tag = if force { "forced" } else { "clean" };
                (format!("OK launching aurora ({mode_tag})"), PostAction::Aurora { force })
            }
            DebugCommand::Reboot => (String::from("OK rebooting console"), PostAction::Reboot),
            DebugCommand::Quit => (String::from("bye"), PostAction::Close),
        },
    }
}

fn execute_exit_or_aurora(is_aurora: bool, force: bool, shutdown: &AtomicBool) {
    if !force {
        REMOTE_ACK.store(false, Ordering::Release);
        REMOTE_ACTION.store(
            if is_aurora { REMOTE_AURORA } else { REMOTE_DASH },
            Ordering::Release,
        );
        // Give the live UI loop up to 600ms to acknowledge and begin clean teardown.
        let mut acked = false;
        for _ in 0..12 {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            if REMOTE_ACK.load(Ordering::Acquire) {
                acked = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if acked {
            // UI loop acknowledged; wait up to 15s for worker + GPU teardown to set `shutdown`.
            if sleep_interruptible(shutdown, 15000) {
                return;
            }
        }
    }

    thread::sleep(Duration::from_millis(100));
    if is_aurora {
        let path = AURORA_PATH
            .lock()
            .map(|p| p.clone())
            .unwrap_or_else(|_| String::from(r"Hdd1:\PeerPressure\Dashboard\Aurora.xex"));
        if let Ok(target) = plan_aurora_launch(&path) {
            let _ = launch::execute_launch(&target);
        }
    }
    return_to_dashboard();
}

fn handle_client(guard: SocketGuard, shutdown: &AtomicBool) {
    configure_socket(guard.0, false);
    if !send_all(guard.0, format!("{BANNER}\n").as_bytes()) {
        return;
    }

    let mut chunk = vec![0u8; 1024];
    let mut line_buf: Vec<u8> = Vec::with_capacity(MAX_CMD_BYTES + 16);
    let mut overflow = false;

    while !shutdown.load(Ordering::Acquire) {
        let mut readable = FdSet::new();
        readable.count = 1;
        readable.sockets[0] = guard.0;
        let timeout = TimeVal::from_milliseconds(200);
        // SAFETY: `readable` and `timeout` live across `NetDll_select`.
        let ready = unsafe {
            xam::NetDll_select(
                XnCaller::Title,
                1,
                &mut readable,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                &timeout,
            )
        };
        if ready == 0 {
            continue;
        }
        if ready == SOCKET_ERROR || readable.count == 0 {
            break;
        }

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
        if n <= 0 {
            break;
        }

        for &byte in &chunk[..n as usize] {
            if byte == b'\n' {
                if overflow {
                    overflow = false;
                    line_buf.clear();
                    let err_frame = format_frame(&format!("ERR command exceeds {MAX_CMD_BYTES} bytes"));
                    if !send_all(guard.0, err_frame.as_bytes()) {
                        return;
                    }
                    continue;
                }
                let line_str = String::from_utf8_lossy(&line_buf);
                let trimmed = line_str.trim_end_matches('\r');
                let (reply, action) = handle_command(trimmed);
                line_buf.clear();
                if !reply.is_empty() {
                    let framed = format_frame(&reply);
                    if !send_all(guard.0, framed.as_bytes()) && matches!(action, PostAction::Continue)
                    {
                        return;
                    }
                }
                match action {
                    PostAction::Continue => {}
                    PostAction::Close => {
                        close_gracefully(guard);
                        return;
                    }
                    PostAction::Dash { force } => {
                        close_gracefully(guard);
                        execute_exit_or_aurora(false, force, shutdown);
                        return;
                    }
                    PostAction::Aurora { force } => {
                        close_gracefully(guard);
                        execute_exit_or_aurora(true, force, shutdown);
                        return;
                    }
                    PostAction::Reboot => {
                        close_gracefully(guard);
                        thread::sleep(Duration::from_millis(100));
                        return_to_firmware(FirmwareRequest::RebootQuiesce);
                    }
                }
            } else if !overflow {
                if line_buf.len() >= MAX_CMD_BYTES {
                    overflow = true;
                } else {
                    line_buf.push(byte);
                }
            }
        }
    }
}
