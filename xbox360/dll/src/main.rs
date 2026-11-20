//! PeerPressure Title DLL (`screentime_dll.xex`).
//! Loaded into the active game or dashboard process via `<TitleID>.ini`.

#![no_std]
#![no_main]

use core::mem::size_of;
use core::num::NonZeroU32;
use core::panic::PanicInfo;
use core::time::Duration;

use screentime_shared::config::Config;
use screentime_shared::proto::{PluginMode, Response, format_heartbeat_req};
use screentime_shared::state::OfflineTracker;
use screentime_shared::xbox_api::{
    current_title_id, dispatch_response, handle_offline, wait_for_config,
};

use xbox360::dbg_println;
use xbox360::sys::xam::{
    self, AddressFamily, EthernetLinkStatus, MessageFlags, Protocol, SockAddrIn, Socket,
    SocketLevel, SocketOption, SocketType, WinsockVersion, WsaData, XnCaller, XnetStartupFlags,
    XnetStartupParams,
};
use xbox360::sys::xboxkrnl::{Boolean, DllEntryReason};
use xbox360::thread::{self, Builder};
use xbox360::time::TickCount;
use xbox360::{thread_entry, title};

const SO_INSECURE: SocketOption = SocketOption(0x5801);
const NET_TIMEOUT_MS: u32 = 4000;
const WORKER_STACK_BYTES: usize = 64 * 1024;

xbox360::sys::bridged!(#[unsafe(no_mangle)] pub _start, dll_main);
thread_entry!(WORKER = worker_entry);

extern "C" fn dll_main(_module: u32, reason: u32, _reserved: u32) -> Boolean {
    if let Some(DllEntryReason::ProcessAttach) = DllEntryReason::from_raw(reason) {
        let Some(stack) = NonZeroU32::new(WORKER_STACK_BYTES as u32) else {
            return Boolean::from(false);
        };
        if let Err(err) = Builder::new().stack_size(stack).spawn_entry(WORKER, 0) {
            dbg_println!("screentime_dll: failed to spawn worker: {err}");
            return Boolean::from(false);
        }
    }
    Boolean::from(true)
}

extern "C" fn worker_entry(_param: u32) -> u32 {
    let Some(cfg) = wait_for_config(8) else {
        dbg_println!("screentime_dll: no screentime.cfg found; idle");
        return 0;
    };

    init_title_network();
    let mut tracker = OfflineTracker::new(TickCount::now().milliseconds(), cfg.offline_grace_sec);
    let mut req_buf = [0u8; 256];
    let mut resp_buf = [0u8; 1024];

    loop {
        let now_ms = TickCount::now().milliseconds();
        let title_id = current_title_id();

        if let Some(len) = format_heartbeat_req(
            &mut req_buf,
            cfg.server_ip,
            cfg.server_port,
            title_id,
            PluginMode::Title,
        ) {
            match http_exchange(&cfg, &req_buf[..len], &mut resp_buf) {
                Some(n) => {
                    if let Some(resp) = Response::parse_http(&resp_buf[..n]) {
                        tracker.record_contact(TickCount::now().milliseconds());
                        dispatch_response(
                            &cfg,
                            &resp,
                            &mut req_buf,
                            &mut resp_buf,
                            http_exchange,
                            exit_to_dashboard,
                        );
                    } else {
                        handle_offline(&cfg, &mut tracker, now_ms);
                    }
                }
                None => handle_offline(&cfg, &mut tracker, now_ms),
            }
        }

        thread::sleep(Duration::from_secs(u64::from(cfg.poll_interval_sec)));
    }
}

fn exit_to_dashboard() -> ! {
    title::return_to_dashboard();
}

fn init_title_network() {
    for _ in 0..20 {
        let link = xam::NetDll_XNetGetEthernetLinkStatus(XnCaller::Title);
        if link.contains(EthernetLinkStatus::ACTIVE) {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }

    let mut xnet = XnetStartupParams::new();
    xnet.flags = XnetStartupFlags::BYPASS_SECURITY;
    let mut wsa = WsaData::new();
    // SAFETY: Stack locals remain valid for the duration of startup calls.
    unsafe {
        let _ = xam::NetDll_XNetStartup(XnCaller::Title, &xnet);
        let _ = xam::NetDll_WSAStartup(XnCaller::Title, WinsockVersion::new(2, 2), &mut wsa);
    }
}

fn http_exchange(cfg: &Config, req: &[u8], out: &mut [u8]) -> Option<usize> {
    let sock = unsafe {
        xam::NetDll_socket(
            XnCaller::Title,
            AddressFamily::Inet,
            SocketType::Stream,
            Protocol::Tcp,
        )
    };
    if sock == Socket::INVALID {
        return None;
    }

    let one: u32 = 1;
    let timeout: u32 = NET_TIMEOUT_MS;
    unsafe {
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            sock,
            SocketLevel::Socket,
            SO_INSECURE,
            (&one as *const u32).cast::<u8>(),
            size_of::<u32>() as i32,
        );
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            sock,
            SocketLevel::Socket,
            SocketOption::SO_RCVTIMEO,
            (&timeout as *const u32).cast::<u8>(),
            size_of::<u32>() as i32,
        );
        let _ = xam::NetDll_setsockopt(
            XnCaller::Title,
            sock,
            SocketLevel::Socket,
            SocketOption::SO_SNDTIMEO,
            (&timeout as *const u32).cast::<u8>(),
            size_of::<u32>() as i32,
        );
    }

    let addr = SockAddrIn {
        family: AddressFamily::Inet as u32 as u16,
        port: cfg.server_port,
        address: u32::from_be_bytes(cfg.server_ip),
        zero: [0; 8],
    };

    let rc = unsafe {
        xam::NetDll_connect(
            XnCaller::Title,
            sock,
            (&addr as *const SockAddrIn).cast::<u8>(),
            size_of::<SockAddrIn>() as i32,
        )
    };
    if rc != 0 {
        unsafe {
            let _ = xam::NetDll_closesocket(XnCaller::Title, sock);
        }
        return None;
    }

    let mut sent = 0usize;
    while sent < req.len() {
        let n = unsafe {
            xam::NetDll_send(
                XnCaller::Title,
                sock,
                req[sent..].as_ptr(),
                (req.len() - sent) as i32,
                MessageFlags::default(),
            )
        };
        if n <= 0 {
            unsafe {
                let _ = xam::NetDll_closesocket(XnCaller::Title, sock);
            }
            return None;
        }
        sent += n as usize;
    }

    let mut total = 0usize;
    while total < out.len() {
        let n = unsafe {
            xam::NetDll_recv(
                XnCaller::Title,
                sock,
                out[total..].as_mut_ptr(),
                (out.len() - total) as i32,
                MessageFlags::default(),
            )
        };
        if n <= 0 {
            break;
        }
        total += n as usize;
    }

    unsafe {
        let _ = xam::NetDll_closesocket(XnCaller::Title, sock);
    }
    (total > 0).then_some(total)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    dbg_println!("screentime_dll: {info}");
    title::terminate()
}
