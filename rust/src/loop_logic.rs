//! Pure poll-loop helpers (no Win32) so clear/switch behaviour is unit-testable
//! on any host.

/// Which music client currently owns the Discord presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    NetEase,
    Tencent,
}

/// When the detected player changes, which previously-published presence (if any)
/// must be cleared before continuing.
///
/// - Player window gone → clear the last active kind.
/// - Switched NetEase ↔ Tencent → clear the previous kind (two Discord app IDs).
/// - Same kind / nothing published yet → clear nothing.
pub fn presence_to_clear(
    last_rpc: Option<PlayerKind>,
    detected: Option<PlayerKind>,
) -> Option<PlayerKind> {
    match (last_rpc, detected) {
        (Some(prev), None) => Some(prev),
        (Some(prev), Some(cur)) if prev != cur => Some(prev),
        _ => None,
    }
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
        assert_eq!(
            presence_to_clear(Some(PlayerKind::NetEase), None),
            Some(PlayerKind::NetEase)
        );
    }

    #[test]
    fn clears_previous_when_switching_apps() {
        assert_eq!(
            presence_to_clear(Some(PlayerKind::NetEase), Some(PlayerKind::Tencent)),
            Some(PlayerKind::NetEase)
        );
        assert_eq!(
            presence_to_clear(Some(PlayerKind::Tencent), Some(PlayerKind::NetEase)),
            Some(PlayerKind::Tencent)
        );
    }

    #[test]
    fn no_clear_when_same_or_idle() {
        assert_eq!(
            presence_to_clear(Some(PlayerKind::NetEase), Some(PlayerKind::NetEase)),
            None
        );
        assert_eq!(presence_to_clear(None, Some(PlayerKind::Tencent)), None);
        assert_eq!(presence_to_clear(None, None), None);
    }

    #[test]
    fn retry_backoff() {
        assert!(!should_retry_attach(1_000, 2_000));
        assert!(should_retry_attach(2_000, 2_000));
        assert!(should_retry_attach(2_500, 2_000));
    }
}
