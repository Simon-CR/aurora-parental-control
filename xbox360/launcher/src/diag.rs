//! Immediate write-through HDD log (`shelf.log`), live ring buffer, and HTTP telemetry.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, ThreadId};

use xbox360::time::TickCount;
use xbox360_sys::xboxkrnl::{
    self, AccessMask, AnsiString, CreateDisposition, CreateOptions, FileAttributes, Handle,
    IoStatusBlock, NtStatus, ObjectAttributeFlags, ObjectAttributes, ShareAccess,
};

const LOG_PATH: &[u8] = br"\Device\Harddisk0\Partition1\PeerPressure\Launcher\shelf.log";
const END_OF_FILE: u64 = u64::MAX;
const MAX_LOG_LINES: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    Ui,
    Worker,
    Debug,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanicAction {
    ParkUiIfDebug,
    ParkWorkerOrDebug,
    Unregistered,
}

struct DiagState {
    stage: String,
    detail: String,
    lines: VecDeque<String>,
}

impl DiagState {
    const fn new() -> Self {
        Self {
            stage: String::new(),
            detail: String::new(),
            lines: VecDeque::new(),
        }
    }

    fn push_line(&mut self, line: String) {
        if self.lines.len() >= MAX_LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

pub struct DiagSnapshot {
    pub uptime_ms: u32,
    pub stage: String,
    pub detail: String,
    pub frame: u32,
    pub heartbeat_age_ms: Option<u32>,
    pub panicked: bool,
    pub panic_msg: Option<String>,
}

static SERVER: Mutex<Option<([u8; 4], u16)>> = Mutex::new(None);
static LOG_LOCK: Mutex<()> = Mutex::new(());
static STATE: Mutex<DiagState> = Mutex::new(DiagState::new());
static PANIC_TEXT: Mutex<Option<String>> = Mutex::new(None);

static UI_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);
static WORKER_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);
static DEBUG_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

static BOOT_TICK_MS: AtomicU32 = AtomicU32::new(0);
static FRAME_COUNT: AtomicU32 = AtomicU32::new(0);
static LAST_HEARTBEAT_MS: AtomicU32 = AtomicU32::new(0);
static HEARTBEAT_SEEN: AtomicBool = AtomicBool::new(false);
static PANIC_RECORDED: AtomicBool = AtomicBool::new(false);
static WORKER_PANICKED: AtomicBool = AtomicBool::new(false);
static DEBUG_PANICKED: AtomicBool = AtomicBool::new(false);

pub fn register_thread(role: ThreadRole) {
    let tid = thread::current().id();
    let slot = match role {
        ThreadRole::Ui => &UI_THREAD,
        ThreadRole::Worker => &WORKER_THREAD,
        ThreadRole::Debug => &DEBUG_THREAD,
    };
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(tid);
}

pub fn worker_panicked() -> bool {
    WORKER_PANICKED.load(Ordering::Acquire)
}

pub fn debug_panicked() -> bool {
    DEBUG_PANICKED.load(Ordering::Acquire)
}

pub fn clear_worker_panicked() {
    WORKER_PANICKED.store(false, Ordering::Release);
}

pub fn clear_debug_panicked() {
    DEBUG_PANICKED.store(false, Ordering::Release);
}

fn write_log_unlocked(disposition: CreateDisposition, offset: u64, bytes: &[u8]) {
    let Some(name) = AnsiString::new(LOG_PATH) else {
        return;
    };
    let attrs = ObjectAttributes {
        root_directory: Handle::NULL,
        object_name: &name,
        attributes: ObjectAttributeFlags::CASE_INSENSITIVE,
    };
    let mut handle = Handle::NULL;
    let mut iosb = IoStatusBlock {
        status: NtStatus::SUCCESS,
        information: 0,
    };

    // SAFETY: `name`, `attrs`, `handle`, and `iosb` remain live for `NtCreateFile`.
    let st = unsafe {
        xboxkrnl::NtCreateFile(
            &mut handle,
            AccessMask::GENERIC_WRITE | AccessMask::SYNCHRONIZE,
            &attrs,
            &mut iosb,
            core::ptr::null(),
            FileAttributes::NORMAL,
            ShareAccess::READ | ShareAccess::WRITE | ShareAccess::DELETE,
            disposition,
            CreateOptions::SYNCHRONOUS_IO_NONALERT
                | CreateOptions::NON_DIRECTORY_FILE
                | CreateOptions::WRITE_THROUGH,
        )
    };
    if !st.is_success() || handle == Handle::NULL {
        return;
    }

    let len = bytes.len().min(u32::MAX as usize) as u32;
    let mut write_iosb = IoStatusBlock {
        status: NtStatus::SUCCESS,
        information: 0,
    };
    // SAFETY: `handle` is open and `bytes` is readable for `len` bytes.
    unsafe {
        let _ = xboxkrnl::NtWriteFile(
            handle,
            Handle::NULL,
            0,
            0,
            &mut write_iosb,
            bytes.as_ptr(),
            len,
            &offset,
        );
        let _ = xboxkrnl::NtClose(handle);
    }
}

fn write_log_bytes(disposition: CreateDisposition, offset: u64, bytes: &[u8]) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_log_unlocked(disposition, offset, bytes);
}

pub fn uptime_ms() -> u32 {
    TickCount::now()
        .milliseconds()
        .wrapping_sub(BOOT_TICK_MS.load(Ordering::Relaxed))
}

/// Truncates `shelf.log` and resets live boot counters at process start.
pub fn reset_log() {
    BOOT_TICK_MS.store(TickCount::now().milliseconds(), Ordering::Relaxed);
    FRAME_COUNT.store(0, Ordering::Relaxed);
    LAST_HEARTBEAT_MS.store(0, Ordering::Relaxed);
    HEARTBEAT_SEEN.store(false, Ordering::Relaxed);
    PANIC_RECORDED.store(false, Ordering::Relaxed);
    WORKER_PANICKED.store(false, Ordering::Relaxed);
    DEBUG_PANICKED.store(false, Ordering::Relaxed);

    if let Ok(mut pt) = PANIC_TEXT.lock() {
        *pt = None;
    }
    if let Ok(mut st) = STATE.lock() {
        st.stage = String::from("boot");
        st.detail.clear();
        st.lines.clear();
        st.push_line(String::from("--- shelf_launcher boot ---"));
    }

    write_log_bytes(
        CreateDisposition::OverwriteIf,
        0,
        b"--- shelf_launcher boot ---\n",
    );
}

/// Registers the `xbox-guard` endpoint for live HTTP diagnostic stages.
pub fn set_server(ip: [u8; 4], port: u16) {
    let mut guard = SERVER.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((ip, port));
}

/// Updates lock-free frame counters each UI loop iteration.
pub fn record_frame(frame_num: u32) {
    FRAME_COUNT.store(frame_num.saturating_add(1), Ordering::Relaxed);
    LAST_HEARTBEAT_MS.store(TickCount::now().milliseconds(), Ordering::Relaxed);
    HEARTBEAT_SEEN.store(true, Ordering::Release);
}

fn thread_matches(slot: &Mutex<Option<ThreadId>>, tid: ThreadId) -> bool {
    slot.try_lock()
        .ok()
        .and_then(|g| *g)
        .is_some_and(|registered| registered == tid)
}

/// Records a panic without deadlocking on contended mutexes and classifies thread recovery.
pub fn handle_panic(msg: &str) -> PanicAction {
    PANIC_RECORDED.store(true, Ordering::Release);
    let clean = msg.replace(['\r', '\n'], " ");
    let entry = format!("[+{}ms] [panic] {clean}", uptime_ms());

    if let Ok(mut pt) = PANIC_TEXT.try_lock() {
        *pt = Some(clean.clone());
    }
    if let Ok(mut st) = STATE.try_lock() {
        st.stage = String::from("panic");
        st.detail = clean;
        st.push_line(entry.clone());
    }
    println!("shelf {entry}");

    if let Ok(_log_guard) = LOG_LOCK.try_lock() {
        let mut disk_line = entry;
        disk_line.push('\n');
        write_log_unlocked(CreateDisposition::OpenIf, END_OF_FILE, disk_line.as_bytes());
    }

    let tid = thread::current().id();
    if thread_matches(&WORKER_THREAD, tid) {
        WORKER_PANICKED.store(true, Ordering::Release);
        return PanicAction::ParkWorkerOrDebug;
    }
    if thread_matches(&DEBUG_THREAD, tid) {
        DEBUG_PANICKED.store(true, Ordering::Release);
        return PanicAction::ParkWorkerOrDebug;
    }
    if thread_matches(&UI_THREAD, tid) {
        return PanicAction::ParkUiIfDebug;
    }
    PanicAction::Unregistered
}

/// Records a boot or runtime stage to RAM, `shelf.log`, debug output, and `xbox-guard`.
pub fn stage(name: &str, detail: &str) {
    let clean_detail = detail.replace(['\r', '\n'], " ");
    let entry = format!("[+{}ms] [{name}] {clean_detail}", uptime_ms());

    {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        st.stage = String::from(name);
        st.detail = clean_detail.clone();
        st.push_line(entry.clone());
    }

    let mut disk_line = entry.clone();
    disk_line.push('\n');
    write_log_bytes(CreateDisposition::OpenIf, END_OF_FILE, disk_line.as_bytes());
    println!("shelf {entry}");

    let server = {
        let guard = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        *guard
    };
    if let Some((ip, port)) = server {
        crate::xnet::send_diag(ip, port, name, &clean_detail);
    }
}

/// Non-blocking snapshot of launcher diagnostics for the remote debug server.
pub fn snapshot() -> DiagSnapshot {
    let uptime = uptime_ms();
    let frame = FRAME_COUNT.load(Ordering::Relaxed);
    let heartbeat_age_ms = HEARTBEAT_SEEN.load(Ordering::Acquire).then(|| {
        TickCount::now()
            .milliseconds()
            .wrapping_sub(LAST_HEARTBEAT_MS.load(Ordering::Relaxed))
    });
    let panicked = PANIC_RECORDED.load(Ordering::Acquire);
    let panic_msg = PANIC_TEXT.try_lock().ok().and_then(|pt| pt.clone());

    match STATE.try_lock() {
        Ok(st) => DiagSnapshot {
            uptime_ms: uptime,
            stage: st.stage.clone(),
            detail: st.detail.clone(),
            frame,
            heartbeat_age_ms,
            panicked,
            panic_msg,
        },
        Err(_) => DiagSnapshot {
            uptime_ms: uptime,
            stage: String::from("<busy>"),
            detail: String::from("diag state mutex contended"),
            frame,
            heartbeat_age_ms,
            panicked,
            panic_msg,
        },
    }
}

/// Non-blocking tail of the in-memory diagnostic ring buffer.
pub fn recent_lines(limit: usize) -> Vec<String> {
    let cap = limit.clamp(1, MAX_LOG_LINES);
    match STATE.try_lock() {
        Ok(st) => {
            let skip = st.lines.len().saturating_sub(cap);
            st.lines.iter().skip(skip).cloned().collect()
        }
        Err(_) => vec![String::from("(log lock busy)")],
    }
}
