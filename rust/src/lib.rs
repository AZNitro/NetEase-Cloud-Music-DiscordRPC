//! Discord Rich Presence for NetEase Cloud Music and Tencent QQ Music.
//!
//! Platform-agnostic modules (`logging`, `model`, `pattern`, `loop_logic`) compile
//! everywhere so their logic can be unit-tested on any host. Everything that
//! touches Win32 is gated behind `#[cfg(windows)]`.

pub mod logging;
pub mod loop_logic;
pub mod model;
pub mod pattern;

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod config;
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
    app::run()
}
