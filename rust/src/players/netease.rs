//! NetEase Cloud Music reader with two modes, chosen automatically:
//!
//! * **Memory mode** (64-bit client): the precise reader — AOB pattern scan of
//!   `cloudmusic.dll` for the AudioPlayer + Schedule, exact position/status, song
//!   id from memory, metadata from the on-disk playlist / web API.
//! * **Title mode** (32-bit client, or if the patterns don't resolve): reads
//!   `Song - Artist` straight from the `OrpheusBrowserHost` window title — which
//!   is version-independent — and enriches cover/album/duration from the local
//!   playlist (matched by name) or the NetEase web API. Progress is approximated
//!   from when the title last changed.
//!
//! The 64-bit patterns are x64 machine code and cannot match a 32-bit client, so
//! title mode is what makes the 32-bit client work at all.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;

use super::MusicPlayer;
use crate::diag;
use crate::model::PlayerInfo;
use crate::platform::memory::{module_base_size, read_std_string_x64, ClockFormat, ProcessMemory};
use crate::platform::window::find_by_class;

const NETEASE_CLASS: &str = "OrpheusBrowserHost";

const AUDIO_PLAYER_PATTERN: &str = "48 8D 0D ? ? ? ? E8 ? ? ? ? 48 8D 0D ? ? ? ? E8 ? ? ? ? 90 48 8D 0D ? ? ? ? E8 ? ? ? ? 48 8D 05 ? ? ? ? 48 8D A5 ? ? ? ? 5F 5D C3 CC CC CC CC CC 48 89 4C 24 ? 55 57 48 81 EC ? ? ? ? 48 8D 6C 24 ? 48 8D 7C 24";
const AUDIO_SCHEDULE_PATTERN: &str = "66 0F 2E 0D ? ? ? ? 7A ? 75 ? 66 0F 2E 15";

const STATUS_WAITING: i32 = 0;
const STATUS_PAUSED: i32 = 2;

enum Source {
    /// Precise 64-bit reader: resolved AudioPlayer + Schedule pointers.
    Memory { audio_player: usize, schedule_ptr: usize },
    /// 32-bit / fallback: song from window title, position from an auto-found clock.
    Title,
}

/// Tracks when the current window title first appeared, to approximate playback
/// position in title mode.
struct TitleProgress {
    title: String,
    started: Instant,
}

/// Extra metadata resolved for a title-mode song (title/artist come from the
/// window title; this fills in the rest).
#[derive(Clone, Default)]
struct Enrichment {
    id: String,
    album: String,
    cover: String,
    duration: f64,
}

pub struct NetEase {
    pid: u32,
    mem: ProcessMemory,
    source: Source,
    playlist_path: PathBuf,
    /// title mode: cache enrichment keyed by song name (avoids re-hitting disk/API).
    enrich_cache: RefCell<Option<(String, Enrichment)>>,
    /// title mode fallback: approximate progress from when the title changed.
    progress: RefCell<Option<TitleProgress>>,
    /// title mode: auto-discovered playback-clock (address + numeric format), once found.
    clock: RefCell<Option<(usize, ClockFormat)>>,
    /// title mode: failed discovery attempts (we stop trying after a few).
    clock_fails: RefCell<u32>,
    /// title mode: last (position, instant) for pause detection.
    pause_tracker: RefCell<Option<(f64, Instant)>>,
}

/// How many times to attempt playback-clock discovery before giving up and
/// using approximate progress (each attempt blocks ~2.4s while scanning).
const CLOCK_MAX_ATTEMPTS: u32 = 3;

impl NetEase {
    pub fn new(pid: u32) -> anyhow::Result<Self> {
        let (base, size) = module_base_size(pid, "cloudmusic.dll")?;
        diag!("[netease] cloudmusic.dll base=0x{base:X} size=0x{size:X} pid={pid}");

        let mem = ProcessMemory::open(pid)?;

        let source = match mem.is_wow64() {
            Some(false) => match resolve_memory_pointers(&mem, base) {
                Ok((audio_player, schedule_ptr)) => {
                    diag!("[netease] 64-bit client: using precise in-memory reader");
                    Source::Memory { audio_player, schedule_ptr }
                }
                Err(e) => {
                    diag!("[netease] 64-bit pattern setup failed ({e}); using window-title reader");
                    Source::Title
                }
            },
            _ => {
                diag!(
                    "[netease] 32-bit client: using window-title reader + memory-scanned clock \
                     (the memory patterns are x64-only)"
                );
                Source::Title
            }
        };

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
            source,
            playlist_path,
            enrich_cache: RefCell::new(None),
            progress: RefCell::new(None),
            clock: RefCell::new(None),
            clock_fails: RefCell::new(0),
            pause_tracker: RefCell::new(None),
        })
    }

    // --- memory mode (64-bit) ---

    fn read_from_memory(&self, audio_player: usize, schedule_ptr: usize) -> Option<PlayerInfo> {
        let status = self.mem.read_i32(audio_player + 0x60).ok()?;
        diag!("[netease] status={status}");
        if status == STATUS_WAITING {
            return None;
        }

        let id = current_song_id(&self.mem, audio_player)?;
        diag!("[netease] current song id = {id:?}");
        if id.is_empty() {
            return None;
        }

        let track = track_by_id(&self.playlist_path, &id)?;

        let info = PlayerInfo {
            identity: id.clone(),
            title: track.title,
            artists: track.artists,
            album: track.album,
            cover: track.cover,
            duration: self.mem.read_f64(audio_player + 0xA8).ok()?,
            schedule: self.mem.read_f64(schedule_ptr).ok()?,
            paused: status == STATUS_PAUSED,
            url: format!("https://music.163.com/#/song?id={id}"),
        };
        diag!("[netease] {info:?}");
        Some(info)
    }

    // --- title mode (32-bit) ---

    fn read_from_title(&self) -> Option<PlayerInfo> {
        let (title, _pid) = find_by_class(NETEASE_CLASS)?;
        if title.trim().is_empty() {
            return None;
        }

        let (song, artists) = parse_title(&title);
        if song.is_empty() {
            return None;
        }

        let enrich = self.enrich(&song, &artists);
        let duration = enrich.duration;

        // Prefer the real playback clock (accurate position + pause detection);
        // fall back to approximate title timing if it couldn't be found.
        let (schedule, paused) = match self.clock_address(duration) {
            Some((addr, fmt)) => {
                let value = self.mem.read_clock_seconds(addr, fmt).unwrap_or(0.0).max(0.0);
                let paused = self.detect_pause(value);
                let sched = if duration > 0.0 { value.min(duration) } else { value };
                (sched, paused)
            }
            None => (self.approx_elapsed(&title, duration), false),
        };

        let url = if enrich.id.is_empty() {
            "https://music.163.com/".to_string()
        } else {
            format!("https://music.163.com/#/song?id={}", enrich.id)
        };

        let info = PlayerInfo {
            identity: enrich.id,
            title: song,
            artists,
            album: enrich.album,
            cover: enrich.cover,
            duration,
            schedule,
            paused,
            url,
        };
        diag!("[netease] (title) {info:?}");
        Some(info)
    }

    /// Return the auto-discovered playback-clock address, running discovery (a
    /// ~2.4s blocking scan) if needed. Retries a few times across reads before
    /// giving up and letting the caller fall back to approximate progress.
    fn clock_address(&self, duration: f64) -> Option<(usize, ClockFormat)> {
        if let Some(found) = *self.clock.borrow() {
            return Some(found);
        }
        if *self.clock_fails.borrow() >= CLOCK_MAX_ATTEMPTS {
            return None;
        }

        match self.mem.find_playback_clock(duration) {
            Some(found) => {
                *self.clock.borrow_mut() = Some(found);
                Some(found)
            }
            None => {
                let mut fails = self.clock_fails.borrow_mut();
                *fails += 1;
                diag!(
                    "[netease] clock not found (attempt {}/{}); \
                     is a song actually playing? using approximate position for now",
                    *fails,
                    CLOCK_MAX_ATTEMPTS
                );
                None
            }
        }
    }

    /// Pause detection for title mode: the clock value not advancing between
    /// reads (while wall-clock time passed) means playback is paused.
    fn detect_pause(&self, value: f64) -> bool {
        let now = Instant::now();
        let mut guard = self.pause_tracker.borrow_mut();
        let paused = match *guard {
            Some((last_value, last_time)) => {
                let dt = now.duration_since(last_time).as_secs_f64();
                dt > 0.15 && (value - last_value) < 0.03
            }
            None => false,
        };
        *guard = Some((value, now));
        paused
    }

    /// Fallback progress: seconds since the window title last changed.
    fn approx_elapsed(&self, title: &str, duration: f64) -> f64 {
        let mut guard = self.progress.borrow_mut();
        let elapsed = match guard.as_ref() {
            Some(p) if p.title == title => p.started.elapsed().as_secs_f64(),
            _ => {
                *guard = Some(TitleProgress { title: title.to_string(), started: Instant::now() });
                0.0
            }
        };
        if duration > 0.0 {
            elapsed.min(duration)
        } else {
            elapsed
        }
    }

    /// Resolve cover/album/duration/id for a title-mode song, cached by name.
    fn enrich(&self, song: &str, artists: &str) -> Enrichment {
        if let Some((name, e)) = &*self.enrich_cache.borrow() {
            if name == song {
                return e.clone();
            }
        }

        let enrich = enrich_by_name(&self.playlist_path, song)
            .or_else(|| {
                diag!("[netease] '{song}' not in local playlist; trying web API search");
                enrich_by_search(song, artists)
            })
            .unwrap_or_default();

        *self.enrich_cache.borrow_mut() = Some((song.to_string(), enrich.clone()));
        enrich
    }
}

impl MusicPlayer for NetEase {
    fn validate(&self, pid: u32) -> bool {
        self.pid == pid
    }

    fn player_info(&self) -> Option<PlayerInfo> {
        match self.source {
            Source::Memory { audio_player, schedule_ptr } => {
                self.read_from_memory(audio_player, schedule_ptr)
            }
            Source::Title => self.read_from_title(),
        }
    }
}

/// Resolve the AudioPlayer + Schedule pointers via AOB pattern scan (64-bit only).
fn resolve_memory_pointers(mem: &ProcessMemory, base: usize) -> anyhow::Result<(usize, usize)> {
    let app_match = mem
        .find_pattern(AUDIO_PLAYER_PATTERN, base)?
        .ok_or_else(|| anyhow::anyhow!("AudioPlayer pattern not found"))?;
    let text = app_match + 3;
    let disp = mem.read_i32(text)?;
    let audio_player = (text as isize + disp as isize + 4) as usize;

    let asp_match = mem
        .find_pattern(AUDIO_SCHEDULE_PATTERN, base)?
        .ok_or_else(|| anyhow::anyhow!("Schedule pattern not found"))?;
    let text2 = asp_match + 4;
    let disp2 = mem.read_i32(text2)?;
    let schedule_ptr = (text2 as isize + disp2 as isize + 4) as usize;

    diag!("[netease] audio_player=0x{audio_player:X} schedule=0x{schedule_ptr:X}");
    Ok((audio_player, schedule_ptr))
}

fn current_song_id(mem: &ProcessMemory, audio_player: usize) -> Option<String> {
    let info = mem.read_i64(audio_player + 0x50).ok()?;
    if info == 0 {
        return Some(String::new());
    }
    let str_base = info as usize + 0x10;
    let s = read_std_string_x64(mem, str_base)?;
    Some(s.split('_').next().unwrap_or("").to_string())
}

/// Split a `Song - Artist` window title. Splits on the first " - " (NetEase uses
/// it as the song/artist separator; multiple artists are joined with "/").
fn parse_title(title: &str) -> (String, String) {
    match title.split_once(" - ") {
        Some((song, artist)) => (song.trim().to_string(), artist.trim().to_string()),
        None => (title.trim().to_string(), String::new()),
    }
}

fn local_appdata() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
}

// --- metadata resolution ---

struct FullTrack {
    title: String,
    artists: String,
    album: String,
    cover: String,
}

/// Memory-mode: full track from the on-disk playlist by id, then web API by id.
fn track_by_id(path: &Path, id: &str) -> Option<FullTrack> {
    if let Some(t) = playlist_track(path, |item| item.id == id) {
        diag!("[netease] metadata source: local playlist (id {id})");
        return Some(t);
    }
    diag!("[netease] id {id} not in local playlist; trying web API");
    api_detail(id).map(|d| FullTrack {
        title: d.title,
        artists: d.artists,
        album: d.album,
        cover: d.cover,
    })
}

/// Title-mode: enrichment (id/album/cover/duration) from the local playlist,
/// matched by song name.
fn enrich_by_name(path: &Path, song: &str) -> Option<Enrichment> {
    let json = std::fs::read_to_string(path).ok()?;
    let playlist: NetEasePlaylist = serde_json::from_str(&json).ok()?;
    let item = playlist
        .list
        .iter()
        .find(|it| it.track.as_ref().is_some_and(|t| t.name == song))?;
    let track = item.track.as_ref()?;
    diag!("[netease] enrichment source: local playlist ('{song}')");
    Some(Enrichment {
        id: item.id.clone(),
        album: track.album.name.clone(),
        cover: track.album.cover.clone(),
        duration: track.duration.map(|ms| ms * 0.001).unwrap_or(0.0),
    })
}

/// Title-mode fallback: search the NetEase web API by `song artist`. Best-effort
/// — any failure returns `None` and the presence still shows song + artist.
fn enrich_by_search(song: &str, artists: &str) -> Option<Enrichment> {
    let query = url_encode(&format!("{song} {artists}"));
    let url = format!(
        "http://music.163.com/api/search/get/web?type=1&offset=0&limit=1&s={query}"
    );
    diag!("[netease] web API search GET {url}");

    let resp = minreq::get(&url)
        .with_header("Referer", "http://music.163.com/")
        .with_header("User-Agent", "Mozilla/5.0")
        .with_timeout(5)
        .send()
        .map_err(|e| diag!("[netease] search request failed: {e}"))
        .ok()?;
    let body = resp.as_str().ok()?;

    let parsed: SearchResp = serde_json::from_str(body)
        .map_err(|e| {
            let preview: String = body.chars().take(200).collect();
            diag!("[netease] search parse error: {e}; body: {preview}")
        })
        .ok()?;

    let song0 = parsed.result?.songs.into_iter().next()?;
    let id = song0.id.to_string();

    // The search result's album often lacks a cover; fetch song detail for it.
    let (album, cover) = match &song0.album {
        Some(a) if !a.pic_url.is_empty() => (a.name.clone(), a.pic_url.clone()),
        _ => api_detail(&id)
            .map(|d| (d.album, d.cover))
            .unwrap_or_default(),
    };

    Some(Enrichment {
        id,
        album,
        cover,
        duration: song0.duration as f64 * 0.001,
    })
}

/// NetEase song-detail API by id → full track.
fn api_detail(id: &str) -> Option<FullTrack> {
    let url = format!("http://music.163.com/api/song/detail/?id={id}&ids=%5B{id}%5D");
    diag!("[netease] web API detail GET {url}");

    let resp = minreq::get(&url)
        .with_header("Referer", "http://music.163.com/")
        .with_header("User-Agent", "Mozilla/5.0")
        .with_timeout(5)
        .send()
        .map_err(|e| diag!("[netease] detail request failed: {e}"))
        .ok()?;
    let body = resp.as_str().ok()?;

    let parsed: DetailResp = serde_json::from_str(body)
        .map_err(|e| {
            let preview: String = body.chars().take(200).collect();
            diag!("[netease] detail parse error: {e}; body: {preview}")
        })
        .ok()?;

    let song = parsed.songs.into_iter().next()?;
    Some(FullTrack {
        title: song.name,
        artists: join_names(song.artists.iter().map(|a| a.name.as_str())),
        album: song.album.name,
        cover: song.album.pic_url,
    })
}

/// Load and search the playlist for a matching item, returning its full track.
fn playlist_track(path: &Path, pred: impl Fn(&NetEasePlaylistItem) -> bool) -> Option<FullTrack> {
    let json = std::fs::read_to_string(path).ok()?;
    let playlist: NetEasePlaylist = serde_json::from_str(&json).ok()?;
    let item = playlist.list.iter().find(|it| pred(it))?;
    let track = item.track.as_ref()?;
    Some(FullTrack {
        title: track.name.clone(),
        artists: join_names(track.artists.iter().map(|a| a.name.as_str())),
        album: track.album.name.clone(),
        cover: track.album.cover.clone(),
    })
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(",")
}

/// Minimal percent-encoding for a query string (RFC 3986 unreserved kept as-is).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
    /// Track length in milliseconds, when present.
    #[serde(default)]
    duration: Option<f64>,
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

// --- web API JSON ---

#[derive(Deserialize)]
struct DetailResp {
    #[serde(default)]
    songs: Vec<DetailSong>,
}

#[derive(Deserialize)]
struct DetailSong {
    name: String,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    album: DetailAlbum,
}

#[derive(Deserialize)]
struct DetailAlbum {
    name: String,
    #[serde(rename = "picUrl", default)]
    pic_url: String,
}

#[derive(Deserialize)]
struct ApiArtist {
    name: String,
}

#[derive(Deserialize)]
struct SearchResp {
    result: Option<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<SearchSong>,
}

#[derive(Deserialize)]
struct SearchSong {
    id: i64,
    album: Option<SearchAlbum>,
    #[serde(default)]
    duration: i64,
}

#[derive(Deserialize)]
struct SearchAlbum {
    #[serde(default)]
    name: String,
    #[serde(rename = "picUrl", default)]
    pic_url: String,
}
