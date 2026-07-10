//! Tencent QQ Music reader. Everything comes from a single `CurrentSongInfo`
//! struct in `QQMusic.dll` memory. Port of `Vanessa/Players/Tencent.cs`
//! (target is 32-bit, so pointers and lengths are 32-bit).
//!
//! ```text
//! struct CurrentSongInfo {          // Size 0x74
//!     std::string song;             // 0x00
//!     std::string artist;           // 0x18
//!     std::string album;            // 0x30
//!     std::string albumThumbnailUrl;// 0x48
//!     uint32_t    songId;           // 0x60
//!     // pad 0x04
//!     uint32_t    duration;         // 0x68  (ms)
//!     uint32_t    schedule;         // 0x6C  (ms)
//!     uint32_t    playStatus;       // 0x70  (0 paused, 1 playing, 3 buffering)
//! };
//! ```

use super::MusicPlayer;
use crate::diag;
use crate::model::PlayerInfo;
use crate::platform::memory::{module_base_size, read_std_string_x86, ProcessMemory};

const CURRENT_SONG_INFO_PATTERN: &str = "A2 ? ? ? ? A3 ? ? ? ? C7 05 ? ? ? ? ? ? ? ? A2 ? ? ? ? A3 ? ? ? ? C7 05 ? ? ? ? ? ? ? ? A2 ? ? ? ? A3";

/// Size of a 32-bit MSVC `std::string`.
const STD: usize = 0x18;

pub struct Tencent {
    pid: u32,
    mem: ProcessMemory,
    base: usize,
}

impl Tencent {
    pub fn new(pid: u32) -> anyhow::Result<Self> {
        let (module_base, size) = module_base_size(pid, "QQMusic.dll")?;
        diag!("[tencent] QQMusic.dll base=0x{module_base:X} size=0x{size:X} pid={pid}");

        let mem = ProcessMemory::open(pid)?;

        let m = mem
            .find_pattern(CURRENT_SONG_INFO_PATTERN, module_base)?
            .ok_or_else(|| anyhow::anyhow!("CurrentSongInfo pattern not found"))?;
        // `mov moffs, al` — the absolute 32-bit address follows the opcode byte.
        let base = mem.read_u32(m + 1)? as usize;
        diag!("[tencent] pattern match=0x{m:X} CurrentSongInfo=0x{base:X}");

        Ok(Self { pid, mem, base })
    }

    fn read_info(&self) -> Option<PlayerInfo> {
        let id = self.mem.read_u32(self.base + STD * 4).ok()?;
        if id == 0 {
            return None;
        }
        let duration_ms = self.mem.read_i32(self.base + STD * 4 + 8).ok()?;
        let schedule_ms = self.mem.read_i32(self.base + STD * 4 + 12).ok()?;
        let status = self.mem.read_i32(self.base + STD * 4 + 16).ok()?;

        let info = PlayerInfo {
            identity: id.to_string(),
            title: read_std_string_x86(&self.mem, self.base).unwrap_or_default(),
            artists: read_std_string_x86(&self.mem, self.base + STD).unwrap_or_default(),
            album: read_std_string_x86(&self.mem, self.base + STD * 2).unwrap_or_default(),
            cover: read_std_string_x86(&self.mem, self.base + STD * 3).unwrap_or_default(),
            schedule: schedule_ms as f64 * 0.001,
            duration: duration_ms as f64 * 0.001,
            paused: status == 0,
            url: format!("https://y.qq.com/n/ryqq/songDetail/{id}"),
        };
        diag!("[tencent] {info:?}");
        Some(info)
    }
}

impl MusicPlayer for Tencent {
    fn validate(&self, pid: u32) -> bool {
        self.pid == pid
    }

    fn player_info(&self) -> Option<PlayerInfo> {
        self.read_info()
    }
}
