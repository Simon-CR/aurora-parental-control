//! DashLaunch / RGH Resident System Module (`screentime_sys.xex`).
//! Runs inside the system process so it remains active across dashboard and title transitions.

#![no_main]
#![feature(xbox360_ext)]

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::os::xbox360::release_tls_slots;
use std::panic::{self, PanicHookInfo};
use std::thread;
use std::time::Duration;

use screentime_shared::config::Config;
use screentime_shared::proto::{PluginMode, Response, format_heartbeat_req};
use screentime_shared::state::OfflineTracker;
use screentime_shared::xbox_api::{
    current_title_id, dispatch_response, handle_offline, wait_for_config,
};

use xbox360::console;
use xbox360::sys::xboxkrnl::{Boolean, DllEntryReason, FirmwareRequest};
use xbox360::time::TickCount;

const STACK_SIZE: usize = 64 * 1024;
const NET_TIMEOUT: Duration = Duration::from_secs(4);

xbox360::sys::bridged!(#[unsafe(no_mangle)] pub _start, module_main);

extern "C" fn module_main(_module: u32, reason: u32, _reserved: u32) -> Boolean {
    let ok = match DllEntryReason::from_raw(reason) {
        Some(DllEntryReason::ProcessAttach) => {
            panic::set_hook(Box::new(park_on_panic));
            thread::Builder::new().stack_size(STACK_SIZE).spawn(worker).is_ok()
        }
        Some(DllEntryReason::ProcessDetach) => {
            // SAFETY: Worker thread teardown completes prior to module unload.
            unsafe { release_tls_slots() };
            true
        }
        _ => true,
    };
    Boolean::from(ok)
}

fn worker() {
    let Some(cfg) = wait_for_config(10) else {
        println!("screentime_sys: no screentime.cfg found; idle");
        return;
    };

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
            PluginMode::System,
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
                            reboot_quiesce,
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

fn reboot_quiesce() -> ! {
    // System threads must not invoke `XamLoaderLaunchTitle`; reboot cleanly instead.
    console::return_to_firmware(FirmwareRequest::RebootQuiesce);
}

fn http_exchange(cfg: &Config, req: &[u8], out: &mut [u8]) -> Option<usize> {
    let ip = Ipv4Addr::from(cfg.server_ip);
    let addr = SocketAddr::V4(SocketAddrV4::new(ip, cfg.server_port));
    let mut stream = TcpStream::connect_timeout(&addr, NET_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(NET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(NET_TIMEOUT));
    stream.write_all(req).ok()?;

    let mut total = 0usize;
    while total < out.len() {
        match stream.read(&mut out[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => break,
        }
    }
    (total > 0).then_some(total)
}

fn park_on_panic(info: &PanicHookInfo<'_>) {
    println!("screentime_sys: {info}");
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
