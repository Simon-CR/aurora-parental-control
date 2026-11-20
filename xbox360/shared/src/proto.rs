//! Zero-allocation HTTP/1.0 wire helpers for the `xbox-guard` LAN protocol.

use crate::config::{parse_u32, trim_ascii};

/// Console operation mode reported on every heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginMode {
    Title,
    System,
}

impl PluginMode {
    pub const fn as_str(self) -> &'static [u8] {
        match self {
            Self::Title => b"title",
            Self::System => b"system",
        }
    }
}

/// Directive returned by `xbox-guard` in the `action=` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Toast,
    Pause,
    TimeUp,
    ExitDash,
    Shutdown,
}

impl Action {
    pub fn parse(raw: &[u8]) -> Self {
        if raw.eq_ignore_ascii_case(b"toast") {
            Self::Toast
        } else if raw.eq_ignore_ascii_case(b"pause") {
            Self::Pause
        } else if raw.eq_ignore_ascii_case(b"time_up") {
            Self::TimeUp
        } else if raw.eq_ignore_ascii_case(b"exit_dash") {
            Self::ExitDash
        } else if raw.eq_ignore_ascii_case(b"shutdown") {
            Self::Shutdown
        } else {
            Self::None
        }
    }
}

/// Approval state returned in `req_status=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    None,
    Pending,
    Approved,
    Denied,
}

impl RequestStatus {
    pub fn parse(raw: &[u8]) -> Self {
        if raw.eq_ignore_ascii_case(b"pending") {
            Self::Pending
        } else if raw.eq_ignore_ascii_case(b"approved") {
            Self::Approved
        } else if raw.eq_ignore_ascii_case(b"denied") {
            Self::Denied
        } else {
            Self::None
        }
    }
}

/// Fixed-size representation of an `xbox-guard` response payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    pub action: Action,
    pub message_buf: [u8; 160],
    pub message_len: usize,
    pub remaining_sec: u32,
    pub used_sec: u32,
    pub daily_max_sec: u32,
    pub window_open: bool,
    pub paused: bool,
    pub req_status: RequestStatus,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            action: Action::None,
            message_buf: [0; 160],
            message_len: 0,
            remaining_sec: 0,
            used_sec: 0,
            daily_max_sec: 0,
            window_open: false,
            paused: false,
            req_status: RequestStatus::None,
        }
    }
}

impl Response {
    pub fn message(&self) -> &[u8] {
        &self.message_buf[..self.message_len]
    }

    /// Parses an HTTP/1.0 or HTTP/1.1 plain-text response (`key=value\n` lines).
    pub fn parse_http(raw: &[u8]) -> Option<Self> {
        let body = extract_http_body(raw)?;
        let mut resp = Self::default();
        let mut saw_key = false;

        for line in body.split(|&b| b == b'\n') {
            let line = trim_ascii(line.strip_suffix(b"\r").unwrap_or(line));
            if line.is_empty() {
                continue;
            }
            let Some(eq) = line.iter().position(|&b| b == b'=') else {
                continue;
            };
            let key = trim_ascii(&line[..eq]);
            let val = trim_ascii(&line[eq + 1..]);
            saw_key = true;

            if key.eq_ignore_ascii_case(b"action") {
                resp.action = Action::parse(val);
            } else if key.eq_ignore_ascii_case(b"message") {
                let n = val.len().min(resp.message_buf.len());
                resp.message_buf[..n].copy_from_slice(&val[..n]);
                resp.message_len = n;
            } else if key.eq_ignore_ascii_case(b"remaining_sec") {
                if let Some(v) = parse_u32(val) {
                    resp.remaining_sec = v;
                }
            } else if key.eq_ignore_ascii_case(b"used_sec") {
                if let Some(v) = parse_u32(val) {
                    resp.used_sec = v;
                }
            } else if key.eq_ignore_ascii_case(b"daily_max_sec") {
                if let Some(v) = parse_u32(val) {
                    resp.daily_max_sec = v;
                }
            } else if key.eq_ignore_ascii_case(b"window_open") {
                resp.window_open = val == b"1" || val.eq_ignore_ascii_case(b"true");
            } else if key.eq_ignore_ascii_case(b"paused") {
                resp.paused = val == b"1" || val.eq_ignore_ascii_case(b"true");
            } else if key.eq_ignore_ascii_case(b"req_status") {
                resp.req_status = RequestStatus::parse(val);
            }
        }

        saw_key.then_some(resp)
    }
}

/// Validates HTTP 200 status line and returns the response body after CRLFCRLF.
pub fn extract_http_body(raw: &[u8]) -> Option<&[u8]> {
    let first_nl = raw.iter().position(|&b| b == b'\n')?;
    let status_line = &raw[..first_nl];
    if !status_line.starts_with(b"HTTP/1.") {
        return None;
    }
    let space = status_line.iter().position(|&b| b == b' ')?;
    let rest = trim_ascii(&status_line[space + 1..]);
    if !rest.starts_with(b"200") {
        return None;
    }

    if let Some(idx) = find_subslice(raw, b"\r\n\r\n") {
        Some(&raw[idx + 4..])
    } else if let Some(idx) = find_subslice(raw, b"\n\n") {
        Some(&raw[idx + 2..])
    } else {
        None
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
    overflowed: bool,
}

impl<'a> BufWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            overflowed: false,
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if self.pos + bytes.len() > self.buf.len() {
            self.overflowed = true;
            return;
        }
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
    }

    fn push_u32(&mut self, mut n: u32) {
        let mut tmp = [0u8; 10];
        let mut idx = tmp.len();
        loop {
            idx -= 1;
            tmp[idx] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.push_bytes(&tmp[idx..]);
    }

    fn push_hex_u32(&mut self, val: u32) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = [0u8; 8];
        for (i, slot) in out.iter_mut().enumerate() {
            let shift = (7 - i) * 4;
            *slot = HEX[((val >> shift) & 0xF) as usize];
        }
        self.push_bytes(&out);
    }

    fn push_host(&mut self, ip: [u8; 4], port: u16) {
        self.push_u32(u32::from(ip[0]));
        self.push_bytes(b".");
        self.push_u32(u32::from(ip[1]));
        self.push_bytes(b".");
        self.push_u32(u32::from(ip[2]));
        self.push_bytes(b".");
        self.push_u32(u32::from(ip[3]));
        self.push_bytes(b":");
        self.push_u32(u32::from(port));
    }

    fn finish(self) -> Option<usize> {
        (!self.overflowed).then_some(self.pos)
    }
}

/// Formats `GET /api/v1/xbox/heartbeat?title_id=<HEX>&mode=<mode> HTTP/1.0`.
pub fn format_heartbeat_req(
    buf: &mut [u8],
    ip: [u8; 4],
    port: u16,
    title_id: u32,
    mode: PluginMode,
) -> Option<usize> {
    let mut w = BufWriter::new(buf);
    w.push_bytes(b"GET /api/v1/xbox/heartbeat?title_id=");
    w.push_hex_u32(title_id);
    w.push_bytes(b"&mode=");
    w.push_bytes(mode.as_str());
    w.push_bytes(b" HTTP/1.0\r\nHost: ");
    w.push_host(ip, port);
    w.push_bytes(b"\r\nConnection: close\r\n\r\n");
    w.finish()
}

/// Formats `POST /api/v1/xbox/request-time?minutes=<min> HTTP/1.0`.
pub fn format_request_time_req(
    buf: &mut [u8],
    ip: [u8; 4],
    port: u16,
    minutes: u32,
) -> Option<usize> {
    let mut w = BufWriter::new(buf);
    w.push_bytes(b"POST /api/v1/xbox/request-time?minutes=");
    w.push_u32(minutes);
    w.push_bytes(b" HTTP/1.0\r\nHost: ");
    w.push_host(ip, port);
    w.push_bytes(b"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    w.finish()
}

/// Formats `POST /api/v1/xbox/request-resume HTTP/1.0`.
pub fn format_request_resume_req(buf: &mut [u8], ip: [u8; 4], port: u16) -> Option<usize> {
    let mut w = BufWriter::new(buf);
    w.push_bytes(b"POST /api/v1/xbox/request-resume HTTP/1.0\r\nHost: ");
    w.push_host(ip, port);
    w.push_bytes(b"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    w.finish()
}
