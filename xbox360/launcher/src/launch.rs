//! Xbox 360 kernel/XAM title and container launching (loose .xex, XBLA, and GOD).

use std::ffi::CString;

use launcher_core::launch_plan::LaunchTarget;
use xbox360_sys::{xam, xboxkrnl};

const XAM_CONTENT_LAUNCH_IMAGE_FROM_FILE_INTERNAL: u32 = 0x0257;
const XAM_CONTENT_OPEN_FILE: u32 = 0x0269;
const XCONTENT_ANY_USER: u32 = 0xFF;
const XCONTENT_MOUNT_FOR_EXEC: u32 = 0x0400_0043;

type XamContentLaunchImageFromFileInternalFn =
    unsafe extern "C" fn(package_path: *const u8, xex_name: *const u8) -> u32;

type XamContentOpenFileFn = unsafe extern "C" fn(
    user_index: u32,
    root_name: *const u8,
    file_name: *const u8,
    flags: u32,
    cache_size: u32,
    license_mask: *mut u32,
    overlapped: *mut u8,
) -> u32;

unsafe fn resolve_xam_export(ordinal: u32) -> Result<u32, String> {
    let mut xam_handle: u32 = 0;
    let st = unsafe { xboxkrnl::XexGetModuleHandle(c"xam.xex".as_ptr().cast(), &mut xam_handle) };
    if !st.is_success() || xam_handle == 0 {
        return Err(format!("XexGetModuleHandle(xam.xex) failed: 0x{:08X}", st.0));
    }
    let mut addr: u32 = 0;
    let st2 = unsafe { xboxkrnl::XexGetProcedureAddress(xam_handle, ordinal, &mut addr) };
    if !st2.is_success() || addr == 0 {
        return Err(format!(
            "XexGetProcedureAddress(xam.xex, 0x{ordinal:X}) failed: 0x{:08X}",
            st2.0
        ));
    }
    Ok(addr)
}

/// Executes a launch plan after graphics engines have been dropped and shut down.
/// Returns `Err` only if the kernel/XAM loader refused to switch titles.
pub fn execute_launch(target: &LaunchTarget) -> Result<(), String> {
    match target {
        LaunchTarget::Executable {
            device_path,
            mount_dir,
            drive_name,
            device_root,
            drive_path,
        } => launch_executable(device_path, mount_dir, drive_name, device_root, drive_path),
        LaunchTarget::Package {
            package_device_path,
            inner_xex,
        } => launch_package(package_device_path, inner_xex),
    }
}

fn launch_executable(
    device_path: &str,
    mount_dir: &str,
    drive_name: &str,
    device_root: &str,
    drive_path: &str,
) -> Result<(), String> {
    let c_path =
        CString::new(device_path).map_err(|_| String::from("executable path contains NUL"))?;
    let c_mount =
        CString::new(mount_dir).map_err(|_| String::from("mount directory contains NUL"))?;

    unsafe {
        xam::XamLoaderLaunchTitleEx(
            c_path.as_ptr().cast(),
            c_mount.as_ptr().cast(),
            core::ptr::null(),
            0,
        );
    }

    // Fallback: create a kernel symbolic link for the drive letter and retry via XamLoaderLaunchTitle.
    let link_name = format!(r"\??\{drive_name}");
    if let (Some(link_s), Some(target_s)) = (
        xboxkrnl::AnsiString::new(link_name.as_bytes()),
        xboxkrnl::AnsiString::new(device_root.as_bytes()),
    ) {
        unsafe {
            let _ = xboxkrnl::ObDeleteSymbolicLink(&link_s);
            let _ = xboxkrnl::ObCreateSymbolicLink(&link_s, &target_s);
        }
        if let Ok(c_drive) = CString::new(drive_path) {
            unsafe {
                xam::XamLoaderLaunchTitle(c_drive.as_ptr().cast(), 0);
            }
        }
    }

    Err(format!("Console loader refused executable: {device_path}"))
}

fn launch_package(package_device_path: &str, inner_xex: &str) -> Result<(), String> {
    let c_pkg = CString::new(package_device_path)
        .map_err(|_| String::from("package path contains NUL"))?;
    let c_xex =
        CString::new(inner_xex).map_err(|_| String::from("inner xex name contains NUL"))?;

    let launch_addr = unsafe { resolve_xam_export(XAM_CONTENT_LAUNCH_IMAGE_FROM_FILE_INTERNAL) }?;
    let launch_from_file: XamContentLaunchImageFromFileInternalFn =
        unsafe { core::mem::transmute(launch_addr as usize) };

    let rc = unsafe { launch_from_file(c_pkg.as_ptr().cast(), c_xex.as_ptr().cast()) };

    // Fallback sequence: mount container under `con:` and boot `con:\<inner_xex>`.
    let open_addr = unsafe { resolve_xam_export(XAM_CONTENT_OPEN_FILE) }?;
    let content_open: XamContentOpenFileFn = unsafe { core::mem::transmute(open_addr as usize) };
    let open_rc = unsafe {
        content_open(
            XCONTENT_ANY_USER,
            c"con".as_ptr().cast(),
            c_pkg.as_ptr().cast(),
            XCONTENT_MOUNT_FOR_EXEC,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    if open_rc == 0 {
        let con_exe = format!(r"con:\{inner_xex}");
        if let Ok(c_con_exe) = CString::new(con_exe) {
            unsafe {
                xam::XamLoaderLaunchTitleEx(
                    c_con_exe.as_ptr().cast(),
                    c"con:".as_ptr().cast(),
                    core::ptr::null(),
                    0,
                );
            }
        }
    }

    Err(format!(
        "Console loader refused package (LaunchImage=0x{rc:08X}, OpenFile=0x{open_rc:08X}): {package_device_path}"
    ))
}
