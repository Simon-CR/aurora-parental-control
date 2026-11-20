//! Configuration parser for `screentime.cfg` without heap allocation.

/// Virtual-key codes accepted by `XamShowMessageBoxUI` in `XMB_PASSCODEMODE`.
pub const VK_PAD_X: u16 = 0x5802;
pub const VK_PAD_Y: u16 = 0x5803;
pub const VK_PAD_RSHOULDER: u16 = 0x5804;
pub const VK_PAD_LSHOULDER: u16 = 0x5805;
pub const VK_PAD_LTRIGGER: u16 = 0x5806;
pub const VK_PAD_RTRIGGER: u16 = 0x5807;
pub const VK_PAD_DPAD_UP: u16 = 0x5810;
pub const VK_PAD_DPAD_DOWN: u16 = 0x5811;
pub const VK_PAD_DPAD_LEFT: u16 = 0x5812;
pub const VK_PAD_DPAD_RIGHT: u16 = 0x5813;

/// Runtime plugin settings loaded from `screentime.cfg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub server_ip: [u8; 4],
    pub server_port: u16,
    pub poll_interval_sec: u32,
    pub offline_grace_sec: u32,
    pub parent_pin: [u16; 4],
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_ip: [127, 0, 0, 1],
            server_port: 8420,
            poll_interval_sec: 5,
            offline_grace_sec: 30,
            parent_pin: [
                VK_PAD_DPAD_UP,
                VK_PAD_DPAD_DOWN,
                VK_PAD_DPAD_LEFT,
                VK_PAD_DPAD_RIGHT,
            ],
        }
    }
}

impl Config {
    /// Returns true when the entered 4-button passcode matches the configured PIN.
    pub fn passcode_matches(&self, entered: &[u16; 4]) -> bool {
        *entered == self.parent_pin
    }

    /// Parses `screentime.cfg` text. Requires a valid `server_ip` so an
    /// unconfigured plugin does not silently run without a server IP.
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let mut cfg = Self::default();
        let mut has_ip = false;

        for line in raw.split(|&b| b == b'\n') {
            let line = trim_ascii(strip_cr(line));
            if line.is_empty() || line[0] == b'#' || line[0] == b';' {
                continue;
            }
            let Some(eq) = line.iter().position(|&b| b == b'=') else {
                continue;
            };
            let key = trim_ascii(&line[..eq]);
            let val = trim_ascii(&line[eq + 1..]);

            if key.eq_ignore_ascii_case(b"server_ip") {
                if let Some(ip) = parse_ipv4(val) {
                    cfg.server_ip = ip;
                    has_ip = true;
                }
            } else if key.eq_ignore_ascii_case(b"server_port") {
                if let Some(p) = parse_u32(val)
                    && (1..=65535).contains(&p)
                {
                    cfg.server_port = p as u16;
                }
            } else if key.eq_ignore_ascii_case(b"poll_interval_sec") {
                if let Some(s) = parse_u32(val) {
                    cfg.poll_interval_sec = s.clamp(2, 60);
                }
            } else if key.eq_ignore_ascii_case(b"offline_grace_sec") {
                if let Some(g) = parse_u32(val) {
                    cfg.offline_grace_sec = g.clamp(10, 600);
                }
            } else if key.eq_ignore_ascii_case(b"parent_pin") {
                if let Some(pin) = parse_pin(val) {
                    cfg.parent_pin = pin;
                }
            }
        }

        has_ip.then_some(cfg)
    }
}

fn strip_cr(b: &[u8]) -> &[u8] {
    b.strip_suffix(b"\r").unwrap_or(b)
}

pub(crate) fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = s.split_first() {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let Some((&last, rest)) = s.split_last() {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Parses dotted-decimal IPv4 bytes without heap allocation.
pub fn parse_ipv4(raw: &[u8]) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut count = 0usize;
    for part in raw.split(|&b| b == b'.') {
        if count >= 4 {
            return None;
        }
        let val = parse_u32(part)?;
        if val > 255 {
            return None;
        }
        octets[count] = val as u8;
        count += 1;
    }
    (count == 4).then_some(octets)
}

/// Parses an unsigned decimal integer.
pub fn parse_u32(raw: &[u8]) -> Option<u32> {
    if raw.is_empty() {
        return None;
    }
    let mut acc = 0u32;
    for &b in raw {
        if !b.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
    }
    Some(acc)
}

/// Parses either `U,D,L,R` comma-separated tokens or compact 4-char `UDLR`.
pub fn parse_pin(raw: &[u8]) -> Option<[u16; 4]> {
    let raw = trim_ascii(raw);
    if raw.contains(&b',') {
        let mut out = [0u16; 4];
        let mut idx = 0usize;
        for tok in raw.split(|&b| b == b',') {
            if idx >= 4 {
                return None;
            }
            out[idx] = pin_token_to_vk(trim_ascii(tok))?;
            idx += 1;
        }
        (idx == 4).then_some(out)
    } else if raw.len() == 4 {
        let mut out = [0u16; 4];
        for (i, &b) in raw.iter().enumerate() {
            out[i] = pin_token_to_vk(&[b])?;
        }
        Some(out)
    } else {
        None
    }
}

fn pin_token_to_vk(tok: &[u8]) -> Option<u16> {
    if tok.eq_ignore_ascii_case(b"U") {
        Some(VK_PAD_DPAD_UP)
    } else if tok.eq_ignore_ascii_case(b"D") {
        Some(VK_PAD_DPAD_DOWN)
    } else if tok.eq_ignore_ascii_case(b"L") {
        Some(VK_PAD_DPAD_LEFT)
    } else if tok.eq_ignore_ascii_case(b"R") {
        Some(VK_PAD_DPAD_RIGHT)
    } else if tok.eq_ignore_ascii_case(b"X") {
        Some(VK_PAD_X)
    } else if tok.eq_ignore_ascii_case(b"Y") {
        Some(VK_PAD_Y)
    } else if tok.eq_ignore_ascii_case(b"LB") {
        Some(VK_PAD_LSHOULDER)
    } else if tok.eq_ignore_ascii_case(b"RB") {
        Some(VK_PAD_RSHOULDER)
    } else if tok.eq_ignore_ascii_case(b"LT") {
        Some(VK_PAD_LTRIGGER)
    } else if tok.eq_ignore_ascii_case(b"RT") {
        Some(VK_PAD_RTRIGGER)
    } else {
        None
    }
}
