//! NetEase Cloud Music reader.
//!
//! Playback *state* (status / song id / position / duration) is always read from
//! the desktop app's `cloudmusic.dll` memory — only the running app knows how far
//! into a song you are. Track *metadata* (title/artists/album/cover) is resolved
//! by song id, preferring the desktop app's on-disk `playingList` JSON and
//! **falling back to NetEase's web API** when the file doesn't have it.
//!
//! Port of `Vanessa/Players/NetEase.cs` (target is 64-bit), plus the API fallback.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::MusicPlayer;
use crate::diag;
use crate::model::PlayerInfo;
use crate::platform::memory::{module_base_size, read_std_string_x64, ProcessMemory};

const AUDIO_PLAYER_PATTERN: &str = "48 8D 0D ? ? ? ? E8 ? ? ? ? 48 8D 0D ? ? ? ? E8 ? ? ? ? 90 48 8D 0D ? ? ? ? E8 ? ? ? ? 48 8D 05 ? ? ? ? 48 8D A5 ? ? ? ? 5F 5D C3 CC CC CC CC CC 48 89 4C 24 ? 55 57 48 81 EC ? ? ? ? 48 8D 6C 24 ? 48 8D 7C 24";
const AUDIO_SCHEDULE_PATTERN: &str = "66 0F 2E 0D ? ? ? ? 7A ? 75 ? 66 0F 2E 15";

// PlayStatus (read at audio_player + 0x60): 0 Waiting, 1 Playing, 2 Paused.
const STATUS_WAITING: i32 = 0;
const STATUS_PAUSED: i32 = 2;

#[derive(Clone)]
struct TrackMeta {
    title: String,
    artists: String,
    album: String,
    cover: String,
}

pub struct NetEase {
    pid: u32,
    mem: ProcessMemory,
    audio_player: usize,
    schedule_ptr: usize,
    playlist_path: PathBuf,
    /// Cache of the last resolved metadata, keyed by song id, so we read the JSON
    /// file / call the API only once per song change (not every 233 ms tick).
    meta_cache: RefCell<Option<(String, TrackMeta)>>,
}

impl NetEase {
    pub fn new(pid: u32) -> anyhow::Result<Self> {
        let (base, size) = module_base_size(pid, "cloudmusic.dll")?;
        diag!("[netease] cloudmusic.dll base=0x{base:X} size=0x{size:X} pid={pid}");

        let mem = ProcessMemory::open(pid)?;

        match mem.is_wow64() {
            Some(true) => diag!(
                "[netease] WARNING: target is a 32-bit (WOW64) process — the 64-bit \
                 AudioPlayer/Schedule patterns are written for the x64 client and will not match"
            ),
            Some(false) => diag!("[netease] target process is 64-bit (native)"),
            None => diag!("[netease] could not determine target bitness"),
        }

        // AudioPlayer: `lea rcx, [rip+disp32]` — resolve the RIP-relative target.
        let app_match = mem
            .find_pattern(AUDIO_PLAYER_PATTERN, base)?
            .ok_or_else(|| anyhow::anyhow!("AudioPlayer pattern not found"))?;
        let text = app_match + 3;
        let disp = mem.read_i32(text)?;
        let audio_player = (text as isize + disp as isize + 4) as usize;
        diag!("[netease] AudioPlayer match=0x{app_match:X} disp={disp} ptr=0x{audio_player:X}");

        // Schedule: `ucomisd xmm1, [rip+disp32]`.
        let asp_match = mem
            .find_pattern(AUDIO_SCHEDULE_PATTERN, base)?
            .ok_or_else(|| anyhow::anyhow!("Schedule pattern not found"))?;
        let text2 = asp_match + 4;
        let disp2 = mem.read_i32(text2)?;
        let schedule_ptr = (text2 as isize + disp2 as isize + 4) as usize;
        diag!("[netease] Schedule match=0x{asp_match:X} disp={disp2} ptr=0x{schedule_ptr:X}");

        let playlist_path = local_appdata()
            .join("NetEase")
            .join("CloudMusic")
            .join("WebData")
            .join("file")
            .join("playingList");
        diag!("[netease] playlist path = {}", playlist_path.display());

        Ok(Self {
            pid,
            mem,
            audio_player,
            schedule_ptr,
            playlist_path,
            meta_cache: RefCell::new(None),
        })
    }

    fn status(&self) -> Option<i32> {
        self.mem.read_i32(self.audio_player + 0x60).ok()
    }

    fn duration(&self) -> Option<f64> {
        self.mem.read_f64(self.audio_player + 0xA8).ok()
    }

    fn schedule(&self) -> Option<f64> {
        self.mem.read_f64(self.schedule_ptr).ok()
    }

    fn current_song_id(&self) -> Option<String> {
        let info = self.mem.read_i64(self.audio_player + 0x50).ok()?;
        if info == 0 {
            return Some(String::new());
        }
        let str_base = info as usize + 0x10;
        let s = read_std_string_x64(&self.mem, str_base)?;
        // The stored id looks like "<id>_<something>"; keep the part before '_'.
        Some(s.split('_').next().unwrap_or("").to_string())
    }

    /// Resolve metadata for `id`: cache → local playlist JSON → NetEase web API.
    fn metadata_for(&self, id: &str) -> Option<TrackMeta> {
        if let Some((cached_id, meta)) = &*self.meta_cache.borrow() {
            if cached_id == id {
                return Some(meta.clone());
            }
        }

        let meta = if let Some(m) = from_local_playlist(&self.playlist_path, id) {
            diag!("[netease] metadata source: local playlist ({id})");
            m
        } else {
            diag!("[netease] local playlist has no id {id}; falling back to web API");
            from_web_api(id)?
        };

        *self.meta_cache.borrow_mut() = Some((id.to_string(), meta.clone()));
        Some(meta)
    }

    fn read_info(&self) -> Option<PlayerInfo> {
        let status = self.status()?;
        diag!("[netease] status={status}");
        if status == STATUS_WAITING {
            return None;
        }

        let id = self.current_song_id()?;
        diag!("[netease] current song id = {id:?}");
        if id.is_empty() {
            return None;
        }

        let meta = self.metadata_for(&id)?;

        let info = PlayerInfo {
            identity: id.clone(),
            title: meta.title,
            artists: meta.artists,
            album: meta.album,
            cover: meta.cover,
            duration: self.duration()?,
            schedule: self.schedule()?,
            paused: status == STATUS_PAUSED,
            url: format!("https://music.163.com/#/song?id={id}"),
        };
        diag!("[netease] {info:?}");
        Some(info)
    }
}

impl MusicPlayer for NetEase {
    fn validate(&self, pid: u32) -> bool {
        self.pid == pid
    }

    fn player_info(&self) -> Option<PlayerInfo> {
        self.read_info()
    }
}

fn local_appdata() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Look the song up in the desktop app's on-disk playlist cache.
fn from_local_playlist(path: &Path, id: &str) -> Option<TrackMeta> {
    let json = std::fs::read_to_string(path).ok()?;
    let playlist: NetEasePlaylist = serde_json::from_str(&json).ok()?;
    let item = playlist.list.iter().find(|x| x.id == id)?;
    let track = item.track.as_ref()?;
    Some(TrackMeta {
        title: track.name.clone(),
        artists: join_names(track.artists.iter().map(|a| a.name.as_str())),
        album: track.album.name.clone(),
        cover: track.album.cover.clone(),
    })
}

/// Fallback: fetch song detail from NetEase's public web API by id.
///
/// Uses plain HTTP so the crate stays TLS-free and cross-compilable. If a network
/// forces HTTPS this will log a failure; adding minreq's `https` feature is the fix.
fn from_web_api(id: &str) -> Option<TrackMeta> {
    let url = format!("http://music.163.com/api/song/detail/?id={id}&ids=%5B{id}%5D");
    diag!("[netease] web API GET {url}");

    let resp = minreq::get(&url)
        .with_header("Referer", "http://music.163.com/")
        .with_header("User-Agent", "Mozilla/5.0")
        .with_timeout(5)
        .send();

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            diag!("[netease] web API request failed: {e}");
            return None;
        }
    };

    let body = resp.as_str().ok()?;
    let parsed: ApiResponse = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => {
            let preview: String = body.chars().take(200).collect();
            diag!("[netease] web API parse error: {e}; body starts: {preview}");
            return None;
        }
    };

    let song = parsed.songs.into_iter().next()?;
    Some(TrackMeta {
        title: song.name,
        artists: join_names(song.artists.iter().map(|a| a.name.as_str())),
        album: song.album.name,
        cover: song.album.pic_url,
    })
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(",")
}

// --- on-disk playlist JSON ---

#[derive(Deserialize)]
struct NetEasePlaylist {
    #[serde(default)]
    list: Vec<NetEasePlaylistItem>,
}

#[derive(Deserialize)]
struct NetEasePlaylistItem {
    id: String,
    track: Option<NetEaseTrack>,
}

#[derive(Deserialize)]
struct NetEaseTrack {
    name: String,
    #[serde(default)]
    artists: Vec<NetEaseArtist>,
    album: NetEaseAlbum,
}

#[derive(Deserialize)]
struct NetEaseArtist {
    name: String,
}

#[derive(Deserialize)]
struct NetEaseAlbum {
    name: String,
    cover: String,
}

// --- web API JSON (music.163.com/api/song/detail) ---

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    songs: Vec<ApiSong>,
}

#[derive(Deserialize)]
struct ApiSong {
    name: String,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    album: ApiAlbum,
}

#[derive(Deserialize)]
struct ApiArtist {
    name: String,
}

#[derive(Deserialize)]
struct ApiAlbum {
    name: String,
    #[serde(rename = "picUrl", default)]
    pic_url: String,
}
