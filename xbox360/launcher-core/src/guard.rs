//! Configuration parsing, xbox-guard policy state, and plain HTTP/1.0 wire helpers.

pub const DEFAULT_SERVER_PORT: u16 = 8420;
pub const DEFAULT_CACHE_DIR: &str = r"\Device\Harddisk0\Partition1\PeerPressure\Launcher\cache";
pub const DEFAULT_AURORA_PATH: &str = r"Hdd1:\PeerPressure\Dashboard\Aurora.xex";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherConfig {
    pub server_ip: Option<[u8; 4]>,
    pub server_port: u16,
    pub poll_interval_sec: u32,
    pub offline_grace_sec: u32,
    pub parent_pin: [u16; 4],
    pub aurora_path: String,
    pub cache_dir: String,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            server_ip: None,
            server_port: DEFAULT_SERVER_PORT,
            poll_interval_sec: 5,
            offline_grace_sec: 30,
            parent_pin: [
                VK_PAD_DPAD_UP,
                VK_PAD_DPAD_DOWN,
                VK_PAD_DPAD_LEFT,
                VK_PAD_DPAD_RIGHT,
            ],
            aurora_path: String::from(DEFAULT_AURORA_PATH),
            cache_dir: String::from(DEFAULT_CACHE_DIR),
        }
    }
}

impl LauncherConfig {
    pub fn server_host_header(&self) -> Option<String> {
        let ip = self.server_ip?;
        Some(format!(
            "{}.{}.{}.{}:{}",
            ip[0], ip[1], ip[2], ip[3], self.server_port
        ))
    }

    /// Parses `screentime.cfg` settings; unknown keys are ignored safely.
    pub fn parse_cfg(raw: &str) -> Self {
        let mut cfg = Self::default();
        for raw_line in raw.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim().trim_matches('"');

            if key.eq_ignore_ascii_case("server_ip") {
                cfg.server_ip = parse_ipv4(val);
            } else if key.eq_ignore_ascii_case("server_port") {
                if let Ok(port) = val.parse::<u32>()
                    && (1..=65535).contains(&port)
                {
                    cfg.server_port = port as u16;
                }
            } else if key.eq_ignore_ascii_case("poll_interval_sec") {
                if let Ok(sec) = val.parse::<u32>() {
                    cfg.poll_interval_sec = sec.clamp(2, 60);
                }
            } else if key.eq_ignore_ascii_case("offline_grace_sec") {
                if let Ok(sec) = val.parse::<u32>() {
                    cfg.offline_grace_sec = sec.clamp(10, 600);
                }
            } else if key.eq_ignore_ascii_case("parent_pin") {
                if let Some(pin) = parse_pin(val) {
                    cfg.parent_pin = pin;
                }
            } else if key.eq_ignore_ascii_case("aurora_path") && !val.is_empty() {
                cfg.aurora_path = String::from(val);
            } else if key.eq_ignore_ascii_case("cache_dir") && !val.is_empty() {
                cfg.cache_dir = String::from(val);
            }
        }
        cfg
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuardAction {
    #[default]
    None,
    Toast,
    Pause,
    TimeUp,
    ExitDash,
    Shutdown,
}

impl GuardAction {
    pub fn parse(raw: &str) -> Self {
        let s = raw.trim();
        if s.eq_ignore_ascii_case("toast") {
            Self::Toast
        } else if s.eq_ignore_ascii_case("pause") {
            Self::Pause
        } else if s.eq_ignore_ascii_case("time_up") {
            Self::TimeUp
        } else if s.eq_ignore_ascii_case("exit_dash") {
            Self::ExitDash
        } else if s.eq_ignore_ascii_case("shutdown") {
            Self::Shutdown
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestStatus {
    #[default]
    None,
    Pending,
    Approved,
    Denied,
}

impl RequestStatus {
    pub fn parse(raw: &str) -> Self {
        let s = raw.trim();
        if s.eq_ignore_ascii_case("pending") {
            Self::Pending
        } else if s.eq_ignore_ascii_case("approved") {
            Self::Approved
        } else if s.eq_ignore_ascii_case("denied") {
            Self::Denied
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardStatus {
    pub action: GuardAction,
    pub message: String,
    pub remaining_sec: u32,
    pub used_sec: u32,
    pub daily_max_sec: u32,
    pub window_open: bool,
    pub paused: bool,
    pub req_status: RequestStatus,
    pub online: bool,
}

impl Default for GuardStatus {
    fn default() -> Self {
        Self {
            action: GuardAction::None,
            message: String::new(),
            remaining_sec: 0,
            used_sec: 0,
            daily_max_sec: 0,
            window_open: true,
            paused: false,
            req_status: RequestStatus::None,
            online: false,
        }
    }
}

impl GuardStatus {
    pub fn is_launch_blocked(&self, pin_override: bool) -> bool {
        if pin_override {
            return false;
        }
        matches!(
            self.action,
            GuardAction::Pause | GuardAction::TimeUp | GuardAction::ExitDash | GuardAction::Shutdown
        )
    }

    pub fn status_badge_text(&self, pin_override: bool) -> String {
        if pin_override {
            return String::from("PIN Override Active");
        }
        if !self.online {
            return String::from("Offline Cache Mode");
        }
        match self.action {
            GuardAction::Pause => String::from("Paused by Parent"),
            GuardAction::TimeUp => String::from("Time Limit Reached"),
            GuardAction::ExitDash | GuardAction::Shutdown => String::from("Locked by Parent"),
            GuardAction::None | GuardAction::Toast => {
                let mins = self.remaining_sec.div_ceil(60);
                format!("{mins}m left today")
            }
        }
    }
}

/// Validates HTTP/1.x status code and returns `(status_code, body_bytes)`.
/// Safe for both UTF-8 text and binary JPEG payloads.
pub fn parse_http_response(raw: &[u8]) -> Result<(u16, &[u8]), String> {
    let header_end_crlf = find_subslice(raw, b"\r\n\r\n");
    let header_end_lf = find_subslice(raw, b"\n\n");
    let (hdr_end, body_start) = match (header_end_crlf, header_end_lf) {
        (Some(a), Some(b)) if a < b => (a, a + 4),
        (Some(a), None) => (a, a + 4),
        (_, Some(b)) => (b, b + 2),
        (None, None) => return Err(String::from("incomplete HTTP headers")),
    };

    let header_str = std::str::from_utf8(&raw[..hdr_end])
        .map_err(|_| String::from("HTTP headers not UTF-8"))?;
    let status_line = header_str.lines().next().unwrap_or_default().trim();
    if !status_line.starts_with("HTTP/1.") {
        return Err(format!("unexpected status line: {status_line:?}"));
    }
    let code_str = status_line.split_whitespace().nth(1).unwrap_or_default();
    let code = code_str
        .parse::<u16>()
        .map_err(|_| format!("invalid HTTP status code in {status_line:?}"))?;
    Ok((code, &raw[body_start..]))
}

/// Parses key=value lines returned by `GET /api/v1/xbox/heartbeat`.
pub fn parse_heartbeat_body(body: &str) -> Result<GuardStatus, String> {
    let mut st = GuardStatus {
        online: true,
        ..GuardStatus::default()
    };
    let mut saw_field = false;
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        saw_field = true;

        if key.eq_ignore_ascii_case("action") {
            st.action = GuardAction::parse(val);
        } else if key.eq_ignore_ascii_case("message") {
            st.message = String::from(val);
        } else if key.eq_ignore_ascii_case("remaining_sec") {
            st.remaining_sec = val.parse::<u32>().unwrap_or(0);
        } else if key.eq_ignore_ascii_case("used_sec") {
            st.used_sec = val.parse::<u32>().unwrap_or(0);
        } else if key.eq_ignore_ascii_case("daily_max_sec") {
            st.daily_max_sec = val.parse::<u32>().unwrap_or(0);
        } else if key.eq_ignore_ascii_case("window_open") {
            st.window_open = val == "1" || val.eq_ignore_ascii_case("true");
        } else if key.eq_ignore_ascii_case("paused") {
            st.paused = val == "1" || val.eq_ignore_ascii_case("true");
        } else if key.eq_ignore_ascii_case("req_status") {
            st.req_status = RequestStatus::parse(val);
        }
    }
    if !saw_field {
        return Err(String::from("empty heartbeat payload"));
    }
    Ok(st)
}

pub fn format_get(path: &str, host_header: &str) -> Vec<u8> {
    format!("GET {path} HTTP/1.0\r\nHost: {host_header}\r\nConnection: close\r\n\r\n").into_bytes()
}

pub fn format_post(path: &str, host_header: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.0\r\nHost: {host_header}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

pub fn heartbeat_path(title_id: &str) -> String {
    format!("/api/v1/xbox/heartbeat?title_id={title_id}&mode=launcher")
}

pub fn request_time_path(minutes: u32) -> String {
    format!("/api/v1/xbox/request-time?minutes={minutes}")
}

pub const fn request_resume_path() -> &'static str {
    "/api/v1/xbox/request-resume"
}

pub const fn catalog_path() -> &'static str {
    "/api/v1/launcher/catalog"
}

pub const fn userstate_path() -> &'static str {
    "/api/v1/launcher/userstate"
}

pub fn favorite_path(title_id: &str, fav: bool) -> String {
    format!(
        "/api/v1/launcher/favorite?title_id={title_id}&fav={}",
        if fav { 1 } else { 0 }
    )
}

pub fn played_path(title_id: &str) -> String {
    format!("/api/v1/launcher/played?title_id={title_id}")
}

pub fn poster_path(title_id: &str) -> String {
    format!("/api/v1/launcher/poster/{title_id}.jpg")
}

pub fn backdrop_path(title_id: &str) -> String {
    format!("/api/v1/launcher/backdrop/{title_id}.jpg")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_ipv4(raw: &str) -> Option<[u8; 4]> {
    let mut parts = raw.trim().split('.');
    let a = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    let c = parts.next()?.parse::<u8>().ok()?;
    let d = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([a, b, c, d])
}

fn parse_pin(raw: &str) -> Option<[u16; 4]> {
    let s = raw.trim();
    if s.contains(',') {
        let mut out = [0u16; 4];
        let mut count = 0;
        for tok in s.split(',') {
            if count >= 4 {
                return None;
            }
            out[count] = pin_token_to_vk(tok.trim())?;
            count += 1;
        }
        (count == 4).then_some(out)
    } else if s.len() == 4 {
        let mut out = [0u16; 4];
        for (i, ch) in s.chars().enumerate() {
            let mut buf = [0u8; 4];
            let sub = ch.encode_utf8(&mut buf);
            out[i] = pin_token_to_vk(sub)?;
        }
        Some(out)
    } else {
        None
    }
}

fn pin_token_to_vk(tok: &str) -> Option<u16> {
    if tok.eq_ignore_ascii_case("U") {
        Some(VK_PAD_DPAD_UP)
    } else if tok.eq_ignore_ascii_case("D") {
        Some(VK_PAD_DPAD_DOWN)
    } else if tok.eq_ignore_ascii_case("L") {
        Some(VK_PAD_DPAD_LEFT)
    } else if tok.eq_ignore_ascii_case("R") {
        Some(VK_PAD_DPAD_RIGHT)
    } else if tok.eq_ignore_ascii_case("X") {
        Some(VK_PAD_X)
    } else if tok.eq_ignore_ascii_case("Y") {
        Some(VK_PAD_Y)
    } else if tok.eq_ignore_ascii_case("LB") {
        Some(VK_PAD_LSHOULDER)
    } else if tok.eq_ignore_ascii_case("RB") {
        Some(VK_PAD_RSHOULDER)
    } else if tok.eq_ignore_ascii_case("LT") {
        Some(VK_PAD_LTRIGGER)
    } else if tok.eq_ignore_ascii_case("RT") {
        Some(VK_PAD_RTRIGGER)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cfg_with_optional_launcher_keys() {
        let cfg = LauncherConfig::parse_cfg(
            "server_ip = 192.168.10.25\nserver_port = 9000\npoll_interval_sec = 7\nparent_pin = X,Y,LB,RB\naurora_path = Hdd1:\\Dash\\Aurora.xex\ncache_dir = \\Device\\Harddisk0\\Partition1\\Cache\n",
        );
        assert_eq!(cfg.server_ip, Some([192, 168, 10, 25]));
        assert_eq!(cfg.server_port, 9000);
        assert_eq!(cfg.poll_interval_sec, 7);
        assert_eq!(
            cfg.parent_pin,
            [VK_PAD_X, VK_PAD_Y, VK_PAD_LSHOULDER, VK_PAD_RSHOULDER]
        );
        assert_eq!(cfg.aurora_path, r"Hdd1:\Dash\Aurora.xex");
        assert_eq!(cfg.cache_dir, r"\Device\Harddisk0\Partition1\Cache");
        assert_eq!(
            cfg.server_host_header(),
            Some(String::from("192.168.10.25:9000"))
        );
    }

    #[test]
    fn parses_binary_jpeg_http_response_without_utf8_corruption() {
        let mut raw = b"HTTP/1.0 200 OK\r\nContent-Type: image/jpeg\r\n\r\n".to_vec();
        let jpeg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x99, 0xFF, 0xD9];
        raw.extend_from_slice(&jpeg_bytes);
        let (code, body) = parse_http_response(&raw).expect("valid binary http response");
        assert_eq!(code, 200);
        assert_eq!(body, jpeg_bytes.as_slice());
    }

    #[test]
    fn parses_heartbeat_and_enforces_lock_flags() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\naction=pause\nmessage=Paused by parent\nremaining_sec=1200\nused_sec=4200\ndaily_max_sec=5400\nwindow_open=1\npaused=1\nreq_status=pending\n";
        let (code, body_bytes) = parse_http_response(raw).unwrap();
        assert_eq!(code, 200);
        let st = parse_heartbeat_body(std::str::from_utf8(body_bytes).unwrap()).unwrap();
        assert_eq!(st.action, GuardAction::Pause);
        assert!(st.paused);
        assert_eq!(st.req_status, RequestStatus::Pending);
        assert!(st.is_launch_blocked(false));
        assert!(!st.is_launch_blocked(true));
    }
}
