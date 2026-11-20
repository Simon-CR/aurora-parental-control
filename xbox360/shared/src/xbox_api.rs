//! Xbox 360 kernel and XAM bindings for notifications, message boxes, power control, and config I/O.

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr;
use core::time::Duration;

use xbox360::dbg_println;
use xbox360::thread;
use xbox360::time::TickCount;
use xbox360_sys::lib_index::{XAM, XBOXKRNL};
use xbox360_sys::status::{NtStatus, Win32Error, Win32ErrorCode};
use xbox360_sys::xam::{self, MessageBoxFlags, MessageBoxIcon, Overlapped, UserIndex};
use xbox360_sys::xboxkrnl::{
    self, AccessMask, AnsiString, CreateOptions, EventType, Handle, IoStatusBlock,
    ObjectAttributeFlags, ObjectAttributes, ProcessorMode, ShareAccess,
};

use crate::config::Config;
use crate::proto::{
    Action, RequestStatus, Response, format_request_resume_req, format_request_time_req,
};
use crate::state::OfflineTracker;
use crate::utf16::encode_utf16_lossy;

const XNOTIFY_GENERIC: u32 = 0x22;
const XNOTIFY_AREA_SYSTEM: u64 = 1;
const XMB_PASSCODEMODE: u32 = 0x0001_0000;
const NOT_ALERTABLE: bool = false;

const CONFIG_PATHS: [&[u8]; 3] = [
    br"\Device\Harddisk0\Partition1\PeerPressure\SideLoading\screentime.cfg",
    br"\Device\Harddisk0\Partition1\screentime.cfg",
    br"\Device\Mass0\screentime.cfg",
];

xbox360_sys::import_thunk!(
    /// Returns the running title ID, or 0 if unavailable.
    pub safe XamGetCurrentTitleId() -> u32,
    lib = XAM,
    ordinal = 0x1CF
);

xbox360_sys::import_thunk!(
    /// Raw `HalReturnToFirmware(routine)` allowing `HalPowerDownRoutine` (5).
    pub HalReturnToFirmwareRaw(routine: u32) -> (),
    lib = XBOXKRNL,
    ordinal = 0x028
);

/// Raw `XNotifyQueueUI` thunk (xam.xex ordinal `0x290`), packing the 64-bit
/// `qwAreas` register pair into `r5` per the Xbox 360 console ABI.
#[unsafe(naked)]
unsafe extern "C" fn xnotify_queue_ui_raw(
    _ty: u32,
    _user: u32,
    _areas_hi: u32,
    _areas_lo: u32,
    _text: *const u16,
    _ctx: *const u8,
) {
    core::arch::naked_asm!(
        ".long 0x78A6000E", // rldimi r6, r5, 32, 0
        "mr 5, 6",
        "mr 6, 7",
        "mr 7, 8",
        "stwu 1, -0x50(1)",
        "mflr 0",
        "stw 0, 0x54(1)",
        "bl 2f",
        "lwz 0, 0x54(1)",
        "mtlr 0",
        "addi 1, 1, 0x50",
        "blr",
        concat!(
            ".pushsection \".xthunks.",
            module_path!(),
            "::XNotifyQueueUI\", \"ax\""
        ),
        ".balign 4",
        "2:",
        ".long 0x01010290", // (1 << 24) | (XAM=1 << 16) | 0x290
        ".long 0x60000000", // nop
        ".long 0x60000000", // nop
        ".long 0x4e800020", // blr
        ".popsection",
    );
}

/// Returns the active Xbox 360 Title ID.
pub fn current_title_id() -> u32 {
    XamGetCurrentTitleId()
}

/// Shows a native Xbox 360 slide-in toast notification.
pub fn show_toast(msg: &[u8]) {
    let mut wide = [0u16; 160];
    let ptr = encode_utf16_lossy(msg, &mut wide);
    // SAFETY: `wide` stays valid and NUL-terminated for the synchronous XAM call.
    unsafe {
        xnotify_queue_ui_raw(
            XNOTIFY_GENERIC,
            UserIndex::Any as u32,
            (XNOTIFY_AREA_SYSTEM >> 32) as u32,
            XNOTIFY_AREA_SYSTEM as u32,
            ptr,
            ptr::null(),
        );
    }
}

/// Powers down the console gracefully via `HalReturnToFirmware(HalPowerDownRoutine = 5)`.
pub fn power_down() -> ! {
    // SAFETY: `5` is `HalPowerDownRoutine`; halts hardware cleanly.
    unsafe { HalReturnToFirmwareRaw(5) };
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

struct OverlappedWait {
    block: UnsafeCell<Overlapped>,
    event: Handle,
}

impl OverlappedWait {
    fn new() -> Option<Self> {
        let mut event = Handle::NULL;
        // SAFETY: `event` points to a valid local handle slot.
        let st = unsafe {
            xboxkrnl::NtCreateEvent(&mut event, ptr::null(), EventType::Notification, false)
        };
        if !st.is_success() || event == Handle::NULL {
            return None;
        }
        Some(Self {
            block: UnsafeCell::new(Overlapped {
                event,
                ..Overlapped::default()
            }),
            event,
        })
    }

    fn as_ptr(&self) -> *mut Overlapped {
        self.block.get()
    }

    fn wait(&self, code: Win32ErrorCode) -> Result<(), Win32Error> {
        if code.error() != Some(Win32Error::IO_PENDING) {
            return code.result();
        }
        // SAFETY: `self.event` is open; null timeout waits until XAM signals completion.
        let wait = unsafe {
            xboxkrnl::NtWaitForSingleObjectEx(
                self.event,
                ProcessorMode::User,
                NOT_ALERTABLE,
                ptr::null(),
            )
        };
        if !wait.is_success() {
            panic!("screentime: NtWaitForSingleObjectEx failed: {:?}", wait);
        }
        // SAFETY: XAM has signaled `self.event` and is done writing `block`.
        unsafe { (*self.block.get()).result }.result()
    }
}

impl Drop for OverlappedWait {
    fn drop(&mut self) {
        // SAFETY: `self.event` is an open handle owned by this guard.
        unsafe {
            let _ = xboxkrnl::NtClose(self.event);
        }
    }
}

/// Displays an interactive XAM message box with up to 3 buttons and waits for selection.
/// Returns `None` if dismissed with B / cancelled or if XAM could not display UI.
pub fn show_message_box_dyn(
    title: &[u8],
    text: &[u8],
    buttons: &[&[u8]],
    icon: MessageBoxIcon,
) -> Option<usize> {
    if buttons.is_empty() || buttons.len() > 3 {
        return None;
    }

    let mut title_w = [0u16; 64];
    let mut text_w = [0u16; 320];
    let mut btn_w = [[0u16; 32]; 3];
    let mut btn_ptrs = [0u32; 3];

    let title_ptr = encode_utf16_lossy(title, &mut title_w);
    let text_ptr = encode_utf16_lossy(text, &mut text_w);
    for (idx, label) in buttons.iter().enumerate() {
        let p = encode_utf16_lossy(label, &mut btn_w[idx]);
        btn_ptrs[idx] = p as usize as u32;
    }

    let overlapped = OverlappedWait::new()?;
    let mut result = [0u32; 2];

    // SAFETY: All string buffers, `btn_ptrs`, `result`, and `overlapped` live until
    // `overlapped.wait()` returns after XAM signals completion.
    let status = unsafe {
        xam::XamShowMessageBoxUI(
            UserIndex::Any,
            title_ptr,
            text_ptr,
            buttons.len() as u32,
            btn_ptrs.as_ptr(),
            0,
            MessageBoxFlags::new(icon),
            result.as_mut_ptr(),
            overlapped.as_ptr(),
        )
    };
    overlapped.wait(status).ok()?;

    let chosen = result[0] as usize;
    (chosen < buttons.len()).then_some(chosen)
}

/// Prompts for the 4-button controller Parent PIN using XAM `XMB_PASSCODEMODE`.
/// Returns `None` if cancelled or failed.
pub fn show_passcode_prompt(title: &[u8], text: &[u8]) -> Option<[u16; 4]> {
    let mut title_w = [0u16; 64];
    let mut text_w = [0u16; 320];
    let title_ptr = encode_utf16_lossy(title, &mut title_w);
    let text_ptr = encode_utf16_lossy(text, &mut text_w);

    let overlapped = OverlappedWait::new()?;
    let mut result = [0u32; 2];
    let flags =
        MessageBoxFlags::from_bits_retain(XMB_PASSCODEMODE | (MessageBoxIcon::Warning as u32));

    // SAFETY: Buffers and `overlapped` outlive the blocking `overlapped.wait()` call.
    // `result` holds `[u16; 4]` (`rgwPasscode`) in Xbox 360 big-endian layout.
    let status = unsafe {
        xam::XamShowMessageBoxUI(
            UserIndex::Any,
            title_ptr,
            text_ptr,
            0,
            ptr::null(),
            0,
            flags,
            result.as_mut_ptr(),
            overlapped.as_ptr(),
        )
    };
    overlapped.wait(status).ok()?;

    // SAFETY: `[u32; 2]` is 8 bytes aligned to 4; reading `[u16; 4]` stays within bounds.
    let passcode = unsafe { *result.as_ptr().cast::<[u16; 4]>() };
    Some(passcode)
}

/// Loads `screentime.cfg` from HDD or USB using NT kernel APIs, retrying briefly
/// so USB storage has time to mount after cold boot.
pub fn wait_for_config(attempts: u32) -> Option<Config> {
    let mut buf = [0u8; 2048];
    for _ in 0..attempts {
        for path in CONFIG_PATHS {
            if let Some(len) = read_nt_file(path, &mut buf) {
                if let Some(cfg) = Config::parse(&buf[..len]) {
                    return Some(cfg);
                }
                dbg_println!("screentime: invalid config syntax");
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    None
}

/// Dispatches an `xbox-guard` heartbeat action across native Xbox 360 UI,
/// time-extension requests, dashboard return, or shutdown.
pub fn dispatch_response<F>(
    cfg: &Config,
    resp: &Response,
    req_buf: &mut [u8; 256],
    resp_buf: &mut [u8; 1024],
    mut exchange: F,
    exit_dash: fn() -> !,
) where
    F: FnMut(&Config, &[u8], &mut [u8]) -> Option<usize>,
{
    match resp.action {
        Action::None => {}
        Action::Toast => {
            if !resp.message().is_empty() {
                show_toast(resp.message());
            }
        }
        Action::Pause => {
            let body = if resp.message().is_empty() {
                b"Console paused by parent.".as_slice()
            } else {
                resp.message()
            };
            if resp.req_status == RequestStatus::Pending {
                let _ = show_message_box_dyn(
                    b"Screen Time Paused",
                    body,
                    &[b"Check Now"],
                    MessageBoxIcon::Information,
                );
            } else if show_message_box_dyn(
                b"Screen Time Paused",
                body,
                &[b"Check Again", b"Ask to Resume"],
                MessageBoxIcon::Warning,
            ) == Some(1)
                && let Some(n) = format_request_resume_req(req_buf, cfg.server_ip, cfg.server_port)
                && exchange(cfg, &req_buf[..n], resp_buf).is_some()
            {
                show_toast(b"Resume request sent to parent");
            }
        }
        Action::TimeUp => {
            let body = if resp.message().is_empty() {
                b"Daily screen time limit reached or outside allowed hours.".as_slice()
            } else {
                resp.message()
            };
            if resp.req_status == RequestStatus::Pending {
                if show_message_box_dyn(
                    b"Time's Up - Waiting",
                    body,
                    &[b"Check Now", b"Shut Down"],
                    MessageBoxIcon::Information,
                ) == Some(1)
                {
                    power_down();
                }
            } else {
                match show_message_box_dyn(
                    b"Screen Time's Up",
                    body,
                    &[b"Ask +15 min", b"Ask +30 min", b"Shut Down"],
                    MessageBoxIcon::Warning,
                ) {
                    Some(0) => {
                        if let Some(n) =
                            format_request_time_req(req_buf, cfg.server_ip, cfg.server_port, 15)
                            && exchange(cfg, &req_buf[..n], resp_buf).is_some()
                        {
                            show_toast(b"Asked parent for +15 minutes");
                        }
                    }
                    Some(1) => {
                        if let Some(n) =
                            format_request_time_req(req_buf, cfg.server_ip, cfg.server_port, 30)
                            && exchange(cfg, &req_buf[..n], resp_buf).is_some()
                        {
                            show_toast(b"Asked parent for +30 minutes");
                        }
                    }
                    Some(2) => power_down(),
                    _ => {}
                }
            }
        }
        Action::ExitDash => {
            show_toast(b"Returning to dashboard...");
            thread::sleep(Duration::from_secs(2));
            exit_dash();
        }
        Action::Shutdown => {
            show_toast(b"Screen time ended - shutting down");
            thread::sleep(Duration::from_secs(2));
            power_down();
        }
    }
}

/// Fail-closed lock dialog shown after `offline_grace_sec` without `xbox-guard` contact.
pub fn handle_offline(cfg: &Config, tracker: &mut OfflineTracker, now_ms: u32) {
    if !tracker.should_lock_offline(now_ms) {
        return;
    }
    match show_message_box_dyn(
        b"Controller Offline",
        b"Cannot reach xbox-guard on the local network.\nReconnect network or enter Parent PIN.",
        &[b"Retry Connection", b"Parent PIN", b"Shut Down"],
        MessageBoxIcon::Error,
    ) {
        Some(1) => {
            if let Some(entered) = show_passcode_prompt(
                b"Parent PIN",
                b"Enter 4-button Parent PIN to unlock for 30 minutes.",
            ) {
                if cfg.passcode_matches(&entered) {
                    tracker.grant_pin_bypass(TickCount::now().milliseconds());
                    show_toast(b"Parent PIN accepted (+30m offline)");
                } else {
                    show_toast(b"Incorrect Parent PIN");
                }
            }
        }
        Some(2) => power_down(),
        _ => {}
    }
}

fn read_nt_file(nt_path: &[u8], out: &mut [u8]) -> Option<usize> {
    let ansi = AnsiString::new(nt_path)?;
    let attrs = ObjectAttributes {
        root_directory: Handle::NULL,
        object_name: &ansi,
        attributes: ObjectAttributeFlags::CASE_INSENSITIVE,
    };
    let mut handle = Handle::NULL;
    let mut iosb = IoStatusBlock {
        status: NtStatus::SUCCESS,
        information: 0,
    };

    // SAFETY: `ansi`, `attrs`, `handle`, and `iosb` are live stack locals.
    let st = unsafe {
        xboxkrnl::NtOpenFile(
            &mut handle,
            AccessMask::GENERIC_READ | AccessMask::SYNCHRONIZE,
            &attrs,
            &mut iosb,
            ShareAccess::READ | ShareAccess::WRITE | ShareAccess::DELETE,
            CreateOptions::SYNCHRONOUS_IO_NONALERT | CreateOptions::NON_DIRECTORY_FILE,
        )
    };
    if !st.is_success() || handle == Handle::NULL {
        return None;
    }

    let offset = 0u64;
    let mut read_iosb = IoStatusBlock {
        status: NtStatus::SUCCESS,
        information: 0,
    };
    // SAFETY: `handle` is open and `out` is writable for `size_of::<u8>() * out.len()`.
    let rst = unsafe {
        xboxkrnl::NtReadFile(
            handle,
            Handle::NULL,
            0,
            0,
            &mut read_iosb,
            out.as_mut_ptr(),
            out.len() as u32,
            &offset,
        )
    };
    // SAFETY: `handle` is closed exactly once before returning.
    unsafe {
        let _ = xboxkrnl::NtClose(handle);
    }

    if !rst.is_success() {
        return None;
    }
    let _ = size_of::<IoStatusBlock>();
    Some((read_iosb.information as usize).min(out.len()))
}
