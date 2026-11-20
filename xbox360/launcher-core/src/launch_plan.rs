//! Xbox 360 NT device path translation and executable/container launch planning.

use crate::catalog::GameEntry;

/// Planned execution target for either a loose `.xex`/`.xbe` or an STFS/GOD container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchTarget {
    Executable {
        device_path: String,
        mount_dir: String,
        drive_name: String,
        device_root: String,
        drive_path: String,
    },
    Package {
        package_device_path: String,
        inner_xex: String,
    },
}

/// Translates Aurora drive names (`Usb0:`, `Hdd1:`, etc.) to Xbox 360 NT object paths.
pub fn device_to_nt_root(device: &str) -> Option<&'static str> {
    let norm = device.trim().trim_end_matches(':').to_ascii_lowercase();
    match norm.as_str() {
        "usb0" => Some(r"\Device\Mass0"),
        "usb1" => Some(r"\Device\Mass1"),
        "usb2" => Some(r"\Device\Mass2"),
        "hdd1" => Some(r"\Device\Harddisk0\Partition1"),
        "hddx" => Some(r"\Device\Harddisk0\Partition2"),
        "memunit0" => Some(r"\Device\Mu0"),
        "cdrom0" | "dvd" => Some(r"\Device\Cdrom0"),
        _ => None,
    }
}

fn canonical_drive_name(device: &str) -> String {
    let trimmed = device.trim().trim_end_matches(':');
    format!("{trimmed}:")
}

fn clean_slashes(path: &str) -> String {
    path.trim()
        .replace('/', "\\")
        .trim_matches('\\')
        .to_string()
}

/// Builds a launch plan for an entry from Aurora's `ContentItems` catalog.
pub fn plan_game_launch(entry: &GameEntry) -> Result<LaunchTarget, String> {
    let device_root = device_to_nt_root(&entry.device)
        .ok_or_else(|| format!("unsupported storage device {:?}", entry.device))?;
    let exe_clean = clean_slashes(&entry.executable);
    if exe_clean.is_empty() {
        return Err(format!("title {} has empty executable path", entry.title_id));
    }

    let dir_clean = clean_slashes(&entry.directory);
    let drive_name = canonical_drive_name(&entry.device);

    let (mount_dir, device_path, drive_path) = if dir_clean.is_empty() {
        (
            String::from(device_root),
            format!("{device_root}\\{exe_clean}"),
            format!("{drive_name}\\{exe_clean}"),
        )
    } else {
        (
            format!("{device_root}\\{dir_clean}"),
            format!("{device_root}\\{dir_clean}\\{exe_clean}"),
            format!("{drive_name}\\{dir_clean}\\{exe_clean}"),
        )
    };

    let lower_exe = exe_clean.to_ascii_lowercase();
    let is_loose_xex = entry.file_type == 1
        || (entry.file_type != 3 && (lower_exe.ends_with(".xex") || lower_exe.ends_with(".xbe")));

    if is_loose_xex {
        Ok(LaunchTarget::Executable {
            device_path,
            mount_dir,
            drive_name,
            device_root: String::from(device_root),
            drive_path,
        })
    } else {
        let inner_xex = if entry.content_group == 3 {
            String::from("default.xbe")
        } else {
            String::from("default.xex")
        };
        Ok(LaunchTarget::Package {
            package_device_path: device_path,
            inner_xex,
        })
    }
}

/// Plans launching Aurora directly from a Windows-style path such as
/// `Hdd1:\PeerPressure\Dashboard\Aurora.xex`.
pub fn plan_aurora_launch(aurora_path: &str) -> Result<LaunchTarget, String> {
    let trimmed = if aurora_path.trim().is_empty() {
        r"Hdd1:\PeerPressure\Dashboard\Aurora.xex"
    } else {
        aurora_path.trim()
    };

    let normalized = trimmed.replace('/', "\\");
    let Some(colon_idx) = normalized.find(':') else {
        return Err(format!("Aurora path missing drive prefix: {aurora_path}"));
    };
    let device = &normalized[..=colon_idx];
    let device_root =
        device_to_nt_root(device).ok_or_else(|| format!("unsupported drive {device}"))?;
    let rest = clean_slashes(&normalized[colon_idx + 1..]);
    if rest.is_empty() {
        return Err(format!("Aurora path missing executable: {aurora_path}"));
    }

    let (dir_part, exe_part) = match rest.rfind('\\') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => ("", rest.as_str()),
    };

    let drive_name = canonical_drive_name(device);
    let (mount_dir, device_path, drive_path) = if dir_part.is_empty() {
        (
            String::from(device_root),
            format!("{device_root}\\{exe_part}"),
            format!("{drive_name}\\{exe_part}"),
        )
    } else {
        (
            format!("{device_root}\\{dir_part}"),
            format!("{device_root}\\{dir_part}\\{exe_part}"),
            format!("{drive_name}\\{dir_part}\\{exe_part}"),
        )
    };

    Ok(LaunchTarget::Executable {
        device_path,
        mount_dir,
        drive_name,
        device_root: String::from(device_root),
        drive_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_entry(file_type: u32, content_group: u32, dir: &str, exe: &str) -> GameEntry {
        GameEntry {
            id: 1,
            title_id: String::from("45410912"),
            media_id: String::from("0E7A7EB3"),
            title_name: String::from("Sample"),
            genre_flag: 2,
            content_group,
            file_type,
            device: String::from("Usb0:"),
            directory: String::from(dir),
            executable: String::from(exe),
            publisher: String::new(),
            release_year: String::from("2012"),
            has_poster: true,
            has_backdrop: true,
            description: String::new(),
            favorite: false,
            last_played_at: 0,
            play_count: 0,
        }
    }

    #[test]
    fn plans_loose_xex_portal_2() {
        let entry = mk_entry(1, 1, r"\games\portal 2", "default.xex");
        let plan = plan_game_launch(&entry).expect("portal 2 plan");
        assert_eq!(
            plan,
            LaunchTarget::Executable {
                device_path: String::from(r"\Device\Mass0\games\portal 2\default.xex"),
                mount_dir: String::from(r"\Device\Mass0\games\portal 2"),
                drive_name: String::from("Usb0:"),
                device_root: String::from(r"\Device\Mass0"),
                drive_path: String::from(r"Usb0:\games\portal 2\default.xex"),
            }
        );
    }

    #[test]
    fn plans_usb_god_skate_3() {
        let entry = mk_entry(3, 1, r"\games\skate 3\454108e6\00007000", "5c087c2c");
        let plan = plan_game_launch(&entry).expect("skate 3 god plan");
        assert_eq!(
            plan,
            LaunchTarget::Package {
                package_device_path: String::from(
                    r"\Device\Mass0\games\skate 3\454108e6\00007000\5c087c2c"
                ),
                inner_xex: String::from("default.xex"),
            }
        );
    }

    #[test]
    fn plans_xbla_castle_crashers_and_og_xbox() {
        let xbla = mk_entry(
            3,
            2,
            r"\content\0000000000000000\584108b7\000d0000",
            "f9432a7fe407a3c196c95d0ffd4a540937fd4eec58",
        );
        assert_eq!(
            plan_game_launch(&xbla).unwrap(),
            LaunchTarget::Package {
                package_device_path: String::from(
                    r"\Device\Mass0\content\0000000000000000\584108b7\000d0000\f9432a7fe407a3c196c95d0ffd4a540937fd4eec58"
                ),
                inner_xex: String::from("default.xex"),
            }
        );

        let og = mk_entry(3, 3, r"\content\0000000000000000\4d530064\00005000", "xbox1pkg");
        assert_eq!(
            plan_game_launch(&og).unwrap(),
            LaunchTarget::Package {
                package_device_path: String::from(
                    r"\Device\Mass0\content\0000000000000000\4d530064\00005000\xbox1pkg"
                ),
                inner_xex: String::from("default.xbe"),
            }
        );
    }

    #[test]
    fn plans_xexmenu_package_and_aurora_executable() {
        let xexmenu = mk_entry(
            3,
            5,
            r"\content\0000000000000000\c0de9999\00080000",
            "c0de99990f586558",
        );
        assert_eq!(
            plan_game_launch(&xexmenu).unwrap(),
            LaunchTarget::Package {
                package_device_path: String::from(
                    r"\Device\Mass0\content\0000000000000000\c0de9999\00080000\c0de99990f586558"
                ),
                inner_xex: String::from("default.xex"),
            }
        );

        let aurora = plan_aurora_launch("").expect("default aurora plan");
        assert_eq!(
            aurora,
            LaunchTarget::Executable {
                device_path: String::from(
                    r"\Device\Harddisk0\Partition1\PeerPressure\Dashboard\Aurora.xex"
                ),
                mount_dir: String::from(r"\Device\Harddisk0\Partition1\PeerPressure\Dashboard"),
                drive_name: String::from("Hdd1:"),
                device_root: String::from(r"\Device\Harddisk0\Partition1"),
                drive_path: String::from(r"Hdd1:\PeerPressure\Dashboard\Aurora.xex"),
            }
        );
    }
}
