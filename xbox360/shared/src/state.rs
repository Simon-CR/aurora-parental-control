//! Fail-closed offline grace and temporary Parent PIN unlock tracking.

/// Duration granted by a correct offline Parent PIN entry (30 minutes).
pub const PIN_BYPASS_MS: u32 = 30 * 60 * 1000;

/// Connectivity state machine shared between Title DLL and System Module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfflineTracker {
    last_contact_ms: u32,
    pin_bypass_until_ms: Option<u32>,
    grace_ms: u32,
}

impl OfflineTracker {
    pub fn new(now_ms: u32, offline_grace_sec: u32) -> Self {
        Self {
            last_contact_ms: now_ms,
            pin_bypass_until_ms: None,
            grace_ms: offline_grace_sec.saturating_mul(1000),
        }
    }

    /// Records a successful HTTP response from `xbox-guard`.
    pub fn record_contact(&mut self, now_ms: u32) {
        self.last_contact_ms = now_ms;
    }

    /// Grants a temporary offline bypass after a valid Parent PIN entry.
    pub fn grant_pin_bypass(&mut self, now_ms: u32) {
        self.pin_bypass_until_ms = Some(now_ms.wrapping_add(PIN_BYPASS_MS));
    }

    /// True if a Parent PIN bypass window is currently active.
    pub fn is_pin_bypassed(&self, now_ms: u32) -> bool {
        let Some(until) = self.pin_bypass_until_ms else {
            return false;
        };
        // Signed wrap-safe comparison for 32-bit GetTickCount milliseconds.
        (until.wrapping_sub(now_ms) as i32) > 0
    }

    /// True when offline longer than `offline_grace_sec` and not bypassed.
    pub fn should_lock_offline(&self, now_ms: u32) -> bool {
        if self.is_pin_bypassed(now_ms) {
            return false;
        }
        now_ms.wrapping_sub(self.last_contact_ms) >= self.grace_ms
    }
}
