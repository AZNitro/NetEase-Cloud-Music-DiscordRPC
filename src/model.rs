//! The platform-agnostic data model shared between the player readers and the
//! Discord RPC layer. Mirrors the C# `PlayerInfo` record.

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    pub identity: String,
    pub title: String,
    pub artists: String,
    pub album: String,
    pub cover: String,
    /// Elapsed playback position, in seconds.
    pub schedule: f64,
    /// Total track length, in seconds.
    pub duration: f64,
    pub url: String,
    pub paused: bool,
}
