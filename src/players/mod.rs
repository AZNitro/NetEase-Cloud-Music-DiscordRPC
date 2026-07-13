use crate::model::PlayerInfo;

pub mod netease;

/// A source of "what's playing right now", bound to a specific process.
pub trait MusicPlayer {
    /// Is this reader still bound to `pid`? (Used to reuse readers across ticks.)
    fn validate(&self, pid: u32) -> bool;

    /// The current track, or `None` when nothing resolvable is playing.
    fn player_info(&self) -> Option<PlayerInfo>;
}
