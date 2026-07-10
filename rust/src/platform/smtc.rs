//! Play/pause via Windows System Media Transport Controls (SMTC).
//!
//! Chromium-based NetEase exposes Now Playing to Windows; SMTC playback status
//! is far more reliable than WASAPI peak meters (which often read 0 while music
//! is playing and previously blanked Discord).

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::diag;

thread_local! {
    static COM_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_com() {
    COM_READY.with(|ready| {
        if !ready.get() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            ready.set(true);
        }
    });
}

/// `Some(true)` if a NetEase SMTC session is Playing; `Some(false)` if it exists
/// but is Paused/Stopped; `None` if no matching session (caller should assume
/// playing so Discord is not blanked).
pub fn netease_playing(name_substr: &str) -> Option<bool> {
    ensure_com();
    match query(name_substr) {
        Ok(v) => v,
        Err(e) => {
            diag!("[smtc] query failed: {e}");
            None
        }
    }
}

fn query(name_substr: &str) -> windows::core::Result<Option<bool>> {
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
    let sessions = manager.GetSessions()?;
    let needle = name_substr.to_ascii_lowercase();
    let mut matched = false;
    let mut playing = false;
    let mut seen: Vec<String> = Vec::new();

    for session in sessions {
        let app_id = session
            .SourceAppUserModelId()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let app_lc = app_id.to_ascii_lowercase();
        seen.push(app_id.clone());

        if !app_lc.contains(&needle) {
            continue;
        }

        matched = true;
        let status = session.GetPlaybackInfo()?.PlaybackStatus()?;
        let is_playing =
            status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        playing |= is_playing;
        diag!("[smtc] app={app_id:?} status={status:?} playing={is_playing}");
    }

    if !matched {
        diag!("[smtc] no session matching '{needle}'; seen={seen:?}");
        return Ok(None);
    }
    Ok(Some(playing))
}
