//! Pure-Rust wire protocol parser and frame helpers for the Shelf remote debug server.

use core::fmt;

pub const DEFAULT_DEBUG_PORT: u16 = 7331;
pub const BANNER: &str = "SHELFDBG/1 ready (type 'help')";
pub const END_SENTINEL: &str = ".END";
pub const MAX_CMD_BYTES: usize = 256;
pub const DEFAULT_LOG_LINES: usize = 64;
pub const MAX_LOG_LINES_QUERY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOverride {
    Normal,
    GpuSolid(u8, u8, u8),
    CpuSolid(u8, u8, u8),
}

impl fmt::Display for RenderOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Normal => f.write_str("normal"),
            Self::GpuSolid(r, g, b) => write!(f, "solid:{r:02X}{g:02X}{b:02X}"),
            Self::CpuSolid(r, g, b) => write!(f, "cpu:{r:02X}{g:02X}{b:02X}"),
        }
    }
}

pub fn encode_render_override(mode: RenderOverride) -> u32 {
    match mode {
        RenderOverride::Normal => 0,
        RenderOverride::GpuSolid(r, g, b) => {
            0x0100_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        }
        RenderOverride::CpuSolid(r, g, b) => {
            0x0200_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        }
    }
}

pub fn decode_render_override(raw: u32) -> RenderOverride {
    let r = ((raw >> 16) & 0xFF) as u8;
    let g = ((raw >> 8) & 0xFF) as u8;
    let b = (raw & 0xFF) as u8;
    match raw >> 24 {
        1 => RenderOverride::GpuSolid(r, g, b),
        2 => RenderOverride::CpuSolid(r, g, b),
        _ => RenderOverride::Normal,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugCommand {
    Help,
    Ping,
    Status,
    Log { lines: usize },
    Gpu,
    Reg { offset: u16 },
    Dve { register: u32 },
    Modes,
    VdMode { flags: u32 },
    Solid { color: Option<(u8, u8, u8)> },
    Cpu { color: Option<(u8, u8, u8)> },
    Dash { force: bool },
    Aurora { force: bool },
    Reboot,
    Quit,
}

fn ensure_no_args<'a>(verb: &str, mut rest: impl Iterator<Item = &'a str>) -> Result<(), String> {
    if let Some(extra) = rest.next() {
        return Err(format!("ERR unexpected argument '{extra}' for '{verb}'"));
    }
    Ok(())
}

fn parse_hex_u32(tok: &str) -> Result<u32, String> {
    let stripped = tok
        .strip_prefix("0x")
        .or_else(|| tok.strip_prefix("0X"))
        .unwrap_or(tok);
    if stripped.is_empty() || !stripped.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("ERR invalid hex value '{tok}'"));
    }
    u32::from_str_radix(stripped, 16).map_err(|_| format!("ERR invalid hex value '{tok}'"))
}

fn parse_rgb_hex(tok: &str) -> Result<(u8, u8, u8), String> {
    let hex = tok.strip_prefix('#').unwrap_or(tok);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("ERR expected 6-digit hex RRGGBB or 'off', got '{tok}'"));
    }
    let val = u32::from_str_radix(hex, 16)
        .map_err(|_| format!("ERR expected 6-digit hex RRGGBB or 'off', got '{tok}'"))?;
    Ok((
        ((val >> 16) & 0xFF) as u8,
        ((val >> 8) & 0xFF) as u8,
        (val & 0xFF) as u8,
    ))
}

fn parse_color_arg<'a>(
    verb: &str,
    mut rest: impl Iterator<Item = &'a str>,
) -> Result<Option<(u8, u8, u8)>, String> {
    let arg = rest
        .next()
        .ok_or_else(|| format!("ERR usage: {verb} <RRGGBB|off>"))?;
    ensure_no_args(verb, rest)?;
    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("normal") {
        return Ok(None);
    }
    parse_rgb_hex(arg).map(Some)
}

fn parse_exit_force<'a>(verb: &str, mut rest: impl Iterator<Item = &'a str>) -> Result<bool, String> {
    let Some(arg) = rest.next() else {
        return Ok(false);
    };
    ensure_no_args(verb, rest)?;
    if arg.eq_ignore_ascii_case("force") {
        Ok(true)
    } else {
        Err(format!("ERR usage: {verb} [force]"))
    }
}

/// Parses one newline-stripped command line without panicking on any input.
pub fn parse_command(line: &str) -> Result<Option<DebugCommand>, String> {
    if line.len() > MAX_CMD_BYTES {
        return Err(format!("ERR command exceeds {MAX_CMD_BYTES} bytes"));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let mut parts = trimmed.split_ascii_whitespace();
    let Some(raw_verb) = parts.next() else {
        return Ok(None);
    };
    let verb = raw_verb.to_ascii_lowercase();
    let cmd = match verb.as_str() {
        "help" | "?" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Help
        }
        "ping" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Ping
        }
        "status" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Status
        }
        "log" => {
            let lines = match parts.next() {
                None => DEFAULT_LOG_LINES,
                Some(tok) => {
                    ensure_no_args(&verb, parts)?;
                    let n = tok
                        .parse::<usize>()
                        .map_err(|_| format!("ERR invalid line count '{tok}'"))?;
                    n.clamp(1, MAX_LOG_LINES_QUERY)
                }
            };
            DebugCommand::Log { lines }
        }
        "gpu" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Gpu
        }
        "reg" => {
            let tok = parts.next().ok_or_else(|| String::from("ERR usage: reg <hex_word_offset>"))?;
            ensure_no_args(&verb, parts)?;
            let raw = parse_hex_u32(tok)?;
            let offset = u16::try_from(raw)
                .map_err(|_| format!("ERR GPU register offset out of range (0x0000..=0xFFFF): {tok}"))?;
            DebugCommand::Reg { offset }
        }
        "dve" => {
            let tok = parts.next().ok_or_else(|| String::from("ERR usage: dve <hex_reg>"))?;
            ensure_no_args(&verb, parts)?;
            let register = parse_hex_u32(tok)?;
            if register > 0xFFFF {
                return Err(format!("ERR DVE register out of range: {tok}"));
            }
            DebugCommand::Dve { register }
        }
        "modes" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Modes
        }
        "vdmode" => {
            let tok = parts.next().ok_or_else(|| String::from("ERR usage: vdmode <hex_flags>"))?;
            ensure_no_args(&verb, parts)?;
            let flags = parse_hex_u32(tok)?;
            DebugCommand::VdMode { flags }
        }
        "solid" => DebugCommand::Solid {
            color: parse_color_arg(&verb, parts)?,
        },
        "cpu" => DebugCommand::Cpu {
            color: parse_color_arg(&verb, parts)?,
        },
        "dash" => DebugCommand::Dash {
            force: parse_exit_force(&verb, parts)?,
        },
        "aurora" => DebugCommand::Aurora {
            force: parse_exit_force(&verb, parts)?,
        },
        "reboot" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Reboot
        }
        "quit" | "exit" => {
            ensure_no_args(&verb, parts)?;
            DebugCommand::Quit
        }
        _ => return Err(format!("ERR unknown command '{raw_verb}' (type 'help')")),
    };
    Ok(Some(cmd))
}

/// Wraps a command reply body with the protocol terminator sentinel `.END`.
pub fn format_frame(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + END_SENTINEL.len() + 2);
    out.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(END_SENTINEL);
    out.push('\n');
    out
}

pub fn help_text() -> &'static str {
    concat!(
        "Shelf Remote Debug Console (SHELFDBG/1)\n",
        "  ping                 Health check with uptime, stage, frame count, and render mode\n",
        "  status               Full boot stage, panic state, Vd display info, and scanout regs\n",
        "  log [N]              Recent in-memory log lines (1..=128, default 64)\n",
        "  gpu                  Dump Xenos ring, status, and display controller MMIO registers\n",
        "  reg <hex>            Read one GPU MMIO register (word index, e.g. 1841 or 0x1844)\n",
        "  dve <hex>            Read one video encoder register via VdReadDVERegisterUlong\n",
        "  modes                Enumerate attached display modes via VdEnumerateVideoModes\n",
        "  vdmode <hex>         Call VdSetDisplayMode(flags) and report updated Vd info\n",
        "  solid <RRGGBB|off>   Override GPU UI frames with solid clear color (or 'off')\n",
        "  cpu <RRGGBB|off>     Override display with direct CPU scanout color (or 'off')\n",
        "  dash [force]         Exit cleanly to dashboard (or force immediate Xam call)\n",
        "  aurora [force]       Launch configured Aurora.xex cleanly (or force immediate)\n",
        "  reboot               Quiesced SMC/kernel reboot (FirmwareRequest::RebootQuiesce)\n",
        "  quit                 Close this debug session",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_basic_commands() {
        assert_eq!(parse_command(""), Ok(None));
        assert_eq!(parse_command("   \t\r\n "), Ok(None));
        assert_eq!(parse_command("HELP"), Ok(Some(DebugCommand::Help)));
        assert_eq!(parse_command("?"), Ok(Some(DebugCommand::Help)));
        assert_eq!(parse_command("ping"), Ok(Some(DebugCommand::Ping)));
        assert_eq!(parse_command("Status"), Ok(Some(DebugCommand::Status)));
        assert_eq!(parse_command("gpu"), Ok(Some(DebugCommand::Gpu)));
        assert_eq!(parse_command("modes"), Ok(Some(DebugCommand::Modes)));
        assert_eq!(parse_command("reboot"), Ok(Some(DebugCommand::Reboot)));
        assert_eq!(parse_command("quit"), Ok(Some(DebugCommand::Quit)));
        assert_eq!(parse_command("exit"), Ok(Some(DebugCommand::Quit)));
    }

    #[test]
    fn parses_numeric_and_hex_arguments() {
        assert_eq!(parse_command("log"), Ok(Some(DebugCommand::Log { lines: 64 })));
        assert_eq!(parse_command("log 0"), Ok(Some(DebugCommand::Log { lines: 1 })));
        assert_eq!(parse_command("log 999"), Ok(Some(DebugCommand::Log { lines: 128 })));
        assert_eq!(parse_command("reg 1841"), Ok(Some(DebugCommand::Reg { offset: 0x1841 })));
        assert_eq!(parse_command("reg 0x1844"), Ok(Some(DebugCommand::Reg { offset: 0x1844 })));
        assert_eq!(parse_command("reg 0X05D0"), Ok(Some(DebugCommand::Reg { offset: 0x05D0 })));
        assert_eq!(parse_command("dve 00"), Ok(Some(DebugCommand::Dve { register: 0 })));
        assert_eq!(parse_command("dve 0x1a"), Ok(Some(DebugCommand::Dve { register: 0x1a })));
        assert_eq!(parse_command("vdmode 0"), Ok(Some(DebugCommand::VdMode { flags: 0 })));
        assert_eq!(
            parse_command("vdmode 0x10"),
            Ok(Some(DebugCommand::VdMode { flags: 0x10 }))
        );
    }

    #[test]
    fn parses_color_and_exit_variants() {
        assert_eq!(
            parse_command("solid FF00AA"),
            Ok(Some(DebugCommand::Solid { color: Some((0xFF, 0x00, 0xAA)) }))
        );
        assert_eq!(
            parse_command("solid #112233"),
            Ok(Some(DebugCommand::Solid { color: Some((0x11, 0x22, 0x33)) }))
        );
        assert_eq!(parse_command("solid OFF"), Ok(Some(DebugCommand::Solid { color: None })));
        assert_eq!(
            parse_command("cpu c87a4b"),
            Ok(Some(DebugCommand::Cpu { color: Some((0xC8, 0x7A, 0x4B)) }))
        );
        assert_eq!(parse_command("cpu normal"), Ok(Some(DebugCommand::Cpu { color: None })));
        assert_eq!(parse_command("dash"), Ok(Some(DebugCommand::Dash { force: false })));
        assert_eq!(parse_command("dash FORCE"), Ok(Some(DebugCommand::Dash { force: true })));
        assert_eq!(parse_command("aurora"), Ok(Some(DebugCommand::Aurora { force: false })));
        assert_eq!(
            parse_command("aurora force"),
            Ok(Some(DebugCommand::Aurora { force: true }))
        );
    }

    #[test]
    fn rejects_invalid_inputs_without_panicking() {
        assert!(parse_command(&"a".repeat(MAX_CMD_BYTES + 1)).is_err());
        assert!(parse_command("ping extra").is_err());
        assert!(parse_command("log abc").is_err());
        assert!(parse_command("reg").is_err());
        assert!(parse_command("reg +10").is_err());
        assert!(parse_command("reg 10000").is_err());
        assert!(parse_command("reg 0x").is_err());
        assert!(parse_command("dve +01").is_err());
        assert!(parse_command("dve 10000").is_err());
        assert!(parse_command("vdmode xyz").is_err());
        assert!(parse_command("solid").is_err());
        assert!(parse_command("solid FFF").is_err());
        assert!(parse_command("solid +12345").is_err());
        assert!(parse_command("cpu café12").is_err());
        assert!(parse_command("dash later").is_err());
        assert!(parse_command("bogus").is_err());
    }

    #[test]
    fn render_override_roundtrips_and_formats() {
        for mode in [
            RenderOverride::Normal,
            RenderOverride::GpuSolid(0x12, 0x34, 0xAB),
            RenderOverride::CpuSolid(0xC8, 0x7A, 0x4B),
        ] {
            let encoded = encode_render_override(mode);
            assert_eq!(decode_render_override(encoded), mode);
        }
        assert_eq!(decode_render_override(0xFF00_0000), RenderOverride::Normal);
        assert_eq!(RenderOverride::Normal.to_string(), "normal");
        assert_eq!(RenderOverride::GpuSolid(0x0A, 0xBC, 0xEF).to_string(), "solid:0ABCEF");
        assert_eq!(RenderOverride::CpuSolid(0xC8, 0x7A, 0x4B).to_string(), "cpu:C87A4B");
    }

    #[test]
    fn format_frame_appends_end_sentinel() {
        assert_eq!(format_frame(""), ".END\n");
        assert_eq!(format_frame("pong"), "pong\n.END\n");
        assert_eq!(format_frame("line1\nline2\n"), "line1\nline2\n.END\n");
        assert!(BANNER.starts_with("SHELFDBG/1"));
        assert!(help_text().contains("modes"));
    }
}
