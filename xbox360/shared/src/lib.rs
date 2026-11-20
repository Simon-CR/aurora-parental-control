#![cfg_attr(not(test), no_std)]

pub mod config;
pub mod proto;
pub mod state;
pub mod utf16;

#[cfg(target_os = "xbox360")]
pub mod xbox_api;

#[cfg(test)]
mod tests {
    use crate::config::{
        Config, VK_PAD_DPAD_DOWN, VK_PAD_DPAD_LEFT, VK_PAD_DPAD_RIGHT, VK_PAD_DPAD_UP,
        VK_PAD_LTRIGGER, VK_PAD_RSHOULDER, VK_PAD_X, VK_PAD_Y, parse_ipv4, parse_pin,
    };
    use crate::proto::{
        Action, PluginMode, RequestStatus, Response, format_heartbeat_req,
        format_request_resume_req, format_request_time_req,
    };
    use crate::state::{OfflineTracker, PIN_BYPASS_MS};
    use crate::utf16::encode_utf16_lossy;

    #[test]
    fn parse_valid_config_and_pin() {
        let raw = b"\
# comment\r\n
server_ip = 192.168.1.10 \r\n
server_port=9000\n
poll_interval_sec=10\n
offline_grace_sec=45\n
parent_pin=X, Y, RB, LT\n";
        let cfg = Config::parse(raw).expect("valid config");
        assert_eq!(cfg.server_ip, [192, 168, 1, 10]);
        assert_eq!(cfg.server_port, 9000);
        assert_eq!(cfg.poll_interval_sec, 10);
        assert_eq!(cfg.offline_grace_sec, 45);
        assert_eq!(
            cfg.parent_pin,
            [VK_PAD_X, VK_PAD_Y, VK_PAD_RSHOULDER, VK_PAD_LTRIGGER]
        );
        assert!(cfg.passcode_matches(&[VK_PAD_X, VK_PAD_Y, VK_PAD_RSHOULDER, VK_PAD_LTRIGGER]));
        assert!(!cfg.passcode_matches(&[
            VK_PAD_DPAD_UP,
            VK_PAD_DPAD_DOWN,
            VK_PAD_DPAD_LEFT,
            VK_PAD_DPAD_RIGHT
        ]));
    }

    #[test]
    fn reject_missing_ip_or_bad_ip() {
        assert_eq!(Config::parse(b"server_port=8420\n"), None);
        assert_eq!(parse_ipv4(b"10.0.0.256"), None);
        assert_eq!(parse_ipv4(b"10.0.0"), None);
        assert_eq!(parse_ipv4(b"192.168.1.50"), Some([192, 168, 1, 50]));
        assert_eq!(
            parse_pin(b"UDLR"),
            Some([
                VK_PAD_DPAD_UP,
                VK_PAD_DPAD_DOWN,
                VK_PAD_DPAD_LEFT,
                VK_PAD_DPAD_RIGHT
            ])
        );
        assert_eq!(parse_pin(b"UDL"), None);
    }

    #[test]
    fn clamp_config_bounds() {
        let cfg = Config::parse(b"server_ip=192.168.1.2\npoll_interval_sec=0\noffline_grace_sec=9999\n")
            .unwrap();
        assert_eq!(cfg.poll_interval_sec, 2);
        assert_eq!(cfg.offline_grace_sec, 600);
    }

    #[test]
    fn format_requests_and_parse_responses() {
        let mut buf = [0u8; 256];
        let n = format_heartbeat_req(&mut buf, [192, 168, 1, 50], 8420, 0x4D53_07E6, PluginMode::Title)
            .unwrap();
        let req = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(req.starts_with("GET /api/v1/xbox/heartbeat?title_id=4D5307E6&mode=title HTTP/1.0\r\n"));
        assert!(req.contains("Host: 192.168.1.50:8420\r\n"));

        let n2 = format_request_time_req(&mut buf, [192, 168, 1, 50], 8420, 15).unwrap();
        assert!(core::str::from_utf8(&buf[..n2])
            .unwrap()
            .starts_with("POST /api/v1/xbox/request-time?minutes=15 HTTP/1.0\r\n"));

        let n3 = format_request_resume_req(&mut buf, [192, 168, 1, 50], 8420).unwrap();
        assert!(core::str::from_utf8(&buf[..n3])
            .unwrap()
            .starts_with("POST /api/v1/xbox/request-resume HTTP/1.0\r\n"));

        let raw_http = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n\
action=toast\n\
message=5 minutes left - save your game\n\
remaining_sec=300\n\
used_sec=3300\n\
daily_max_sec=3600\n\
window_open=1\n\
paused=0\n\
req_status=pending\n";
        let resp = Response::parse_http(raw_http).unwrap();
        assert_eq!(resp.action, Action::Toast);
        assert_eq!(resp.message(), b"5 minutes left - save your game");
        assert_eq!(resp.remaining_sec, 300);
        assert_eq!(resp.used_sec, 3300);
        assert_eq!(resp.daily_max_sec, 3600);
        assert!(resp.window_open);
        assert!(!resp.paused);
        assert_eq!(resp.req_status, RequestStatus::Pending);
    }

    #[test]
    fn offline_grace_and_pin_bypass_state() {
        let mut tracker = OfflineTracker::new(1000, 30);
        assert!(!tracker.should_lock_offline(29_000));
        assert!(tracker.should_lock_offline(31_000));

        tracker.grant_pin_bypass(31_000);
        assert!(!tracker.should_lock_offline(31_000 + PIN_BYPASS_MS - 1));
        assert!(tracker.should_lock_offline(31_000 + PIN_BYPASS_MS + 1));

        tracker.record_contact(31_000 + PIN_BYPASS_MS + 1);
        assert!(!tracker.should_lock_offline(31_000 + PIN_BYPASS_MS + 100));
    }

    #[test]
    fn encode_utf16_ascii_and_utf8() {
        let mut out = [0u16; 16];
        let _ = encode_utf16_lossy("Time's up!".as_bytes(), &mut out);
        assert_eq!(
            &out[..11],
            &[
                b'T' as u16,
                b'i' as u16,
                b'm' as u16,
                b'e' as u16,
                b'\'' as u16,
                b's' as u16,
                b' ' as u16,
                b'u' as u16,
                b'p' as u16,
                b'!' as u16,
                0
            ]
        );
    }
}
