//! Thin wrapper over `discord-rich-presence`: turns a [`PlayerInfo`] into a
//! Discord activity and manages the connection (with lazy reconnect).

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Button, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

use crate::diag;
use crate::model::PlayerInfo;

const GITHUB_URL: &str = "https://github.com/AZNitro/NetEase-Cloud-Music-DiscordRPC";

pub struct Rpc {
    client: DiscordIpcClient,
    app_id: String,
    /// Shown as the Discord small-image tooltip (e.g. "NetEase CloudMusic").
    brand: String,
    connected: bool,
}

impl Rpc {
    pub fn new(app_id: &str, brand: &str) -> anyhow::Result<Self> {
        let client = DiscordIpcClient::new(app_id)
            .map_err(|e| anyhow::anyhow!("failed to create RPC client {app_id}: {e}"))?;
        Ok(Self {
            client,
            app_id: app_id.to_string(),
            brand: brand.to_string(),
            connected: false,
        })
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

        // With a known duration, show a start→end progress bar; otherwise show a
        // count-up timer (start only) so an approximate position still displays.
        let timestamps = if info.duration > 0.0 {
            Timestamps::new().start(start).end(start + info.duration as i64)
        } else {
            Timestamps::new().start(start)
        };

        let details = format!("🎵 {}", info.title);
        let state = format!("🎤 {}", info.artists);
        let album = format!("💿 {}", info.album);

        // Discord requires a non-empty image key; fall back to the bundled asset.
        let large_image = if info.cover.is_empty() { "timg" } else { info.cover.as_str() };

        let mut assets = Assets::new()
            .large_image(large_image)
            .small_image("timg")
            .small_text(&self.brand);
        if !info.album.is_empty() {
            assets = assets.large_text(&album);
        }

        // Discord rejects empty button URLs; skip the Listen button if we have none.
        let mut buttons = Vec::with_capacity(2);
        if !info.url.is_empty() {
            buttons.push(Button::new("🎧 Listen", &info.url));
        }
        buttons.push(Button::new("👏 View App on GitHub", GITHUB_URL));

        let activity = Activity::new()
            .activity_type(ActivityType::Listening)
            .details(&details)
            .state(&state)
            .timestamps(timestamps)
            .assets(assets)
            .buttons(buttons);

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
