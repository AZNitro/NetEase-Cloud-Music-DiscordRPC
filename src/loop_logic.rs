//! Pure poll-loop helpers (no Win32) so clear/retry behaviour is unit-testable
//! on any host.

/// When the NetEase window disappears, clear any previously published presence.
pub fn should_clear_presence(had_presence: bool, player_detected: bool) -> bool {
    had_presence && !player_detected
}

/// Whether a failed reader attach should be retried now.
pub fn should_retry_attach(now_ms: u128, next_retry_ms: u128) -> bool {
    now_ms >= next_retry_ms
}

/// Backoff after a failed attach attempt (milliseconds).
pub const ATTACH_RETRY_MS: u128 = 2_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_when_player_gone() {
        assert!(should_clear_presence(true, false));
    }

    #[test]
    fn no_clear_while_detected_or_idle() {
        assert!(!should_clear_presence(true, true));
        assert!(!should_clear_presence(false, false));
        assert!(!should_clear_presence(false, true));
    }

    #[test]
    fn retry_backoff() {
        assert!(!should_retry_attach(1_000, 2_000));
        assert!(should_retry_attach(2_000, 2_000));
        assert!(should_retry_attach(2_500, 2_000));
    }
}
