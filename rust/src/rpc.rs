//! Thin wrapper over `discord-rich-presence`: turns a [`PlayerInfo`] into a
//! Discord activity and manages the connection (with lazy reconnect).

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Button, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

use crate::diag;
use crate::model::PlayerInfo;

const GITHUB_URL: &str = "https://github.com/Kxnrl/NetEase-Cloud-Music-DiscordRPC";

pub struct Rpc {
    client: DiscordIpcClient,
    app_id: String,
    connected: bool,
}

impl Rpc {
    pub fn new(app_id: &str) -> anyhow::Result<Self> {
        let client = DiscordIpcClient::new(app_id)
            .map_err(|e| anyhow::anyhow!("failed to create RPC client {app_id}: {e}"))?;
        Ok(Self { client, app_id: app_id.to_string(), connected: false })
    }

    /// Connect if not already connected. Returns whether we're connected.
    pub fn ensure_connected(&mut self) -> bool {
        if self.connected {
            return true;
        }
        match self.client.connect() {
            Ok(_) => {
                self.connected = true;
                diag!("[rpc] connected {}", self.app_id);
                true
            }
            Err(e) => {
                diag!("[rpc] connect failed ({}): {e}", self.app_id);
                false
            }
        }
    }

    pub fn clear(&mut self) {
        if !self.connected {
            return;
        }
        if let Err(e) = self.client.clear_activity() {
            diag!("[rpc] clear failed ({}): {e}", self.app_id);
            self.connected = false;
        }
    }

    pub fn update(&mut self, info: &PlayerInfo) {
        if !self.ensure_connected() {
            return;
        }

        let start = now_unix() - info.schedule as i64;
        let end = start + info.duration as i64;

        let details = format!("🎵 {}", info.title);
        let state = format!("🎤 {}", info.artists);
        let album = format!("💿 {}", info.album);

        let activity = Activity::new()
            .activity_type(ActivityType::Listening)
            .details(&details)
            .state(&state)
            .timestamps(Timestamps::new().start(start).end(end))
            .assets(
                Assets::new()
                    .large_image(&info.cover)
                    .large_text(&album)
                    .small_image("timg")
                    .small_text("NetEase CloudMusic"),
            )
            .buttons(vec![
                Button::new("🎧 Listen", &info.url),
                Button::new("👏 View App on GitHub", GITHUB_URL),
            ]);

        if let Err(e) = self.client.set_activity(activity) {
            diag!("[rpc] set_activity failed ({}): {e}", self.app_id);
            self.connected = false;
        }
    }
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
