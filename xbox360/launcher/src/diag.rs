//! Immediate write-through HDD log (`shelf.log`) + live HTTP boot telemetry.

use std::sync::Mutex;

use xbox360_sys::xboxkrnl::{
    self, AccessMask, AnsiString, CreateDisposition, CreateOptions, FileAttributes, Handle,
    IoStatusBlock, NtStatus, ObjectAttributeFlags, ObjectAttributes, ShareAccess,
};

const LOG_PATH: &[u8] = br"\Device\Harddisk0\Partition1\PeerPressure\Launcher\shelf.log";
const END_OF_FILE: u64 = u64::MAX;

static SERVER: Mutex<Option<([u8; 4], u16)>> = Mutex::new(None);
static LOG_LOCK: Mutex<()> = Mutex::new(());

fn write_log_bytes(disposition: CreateDisposition, offset: u64, bytes: &[u8]) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// Truncates `shelf.log` on launch so every session starts with a clean trace.
pub fn reset_log() {
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

/// Records a boot or runtime stage to HDD (`shelf.log`), kernel debug output,
/// and (once configured) `GET /api/v1/launcher/diag`.
pub fn stage(name: &str, detail: &str) {
    let line = format!("[{name}] {detail}\n");
    write_log_bytes(CreateDisposition::OpenIf, END_OF_FILE, line.as_bytes());
    println!("shelf [{name}] {detail}");

    let server = {
        let guard = SERVER.lock().unwrap_or_else(|e| e.into_inner());
        *guard
    };
    if let Some((ip, port)) = server {
        crate::xnet::send_diag(ip, port, name, detail);
    }
}
