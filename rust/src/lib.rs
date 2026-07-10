//! Discord Rich Presence for NetEase Cloud Music and Tencent QQ Music.
//!
//! Platform-agnostic modules (`logging`, `model`, `pattern`) compile everywhere
//! so their logic can be unit-tested on any host. Everything that touches Win32
//! is gated behind `#[cfg(windows)]`.

pub mod logging;
pub mod model;
pub mod pattern;

#[cfg(windows)]
pub mod platform;
#[cfg(windows)]
pub mod players;
#[cfg(windows)]
pub mod rpc;
#[cfg(windows)]
pub mod updater;

/// Run the application (Windows only).
#[cfg(windows)]
pub fn run() -> anyhow::Result<()> {
    updater::run()
}
