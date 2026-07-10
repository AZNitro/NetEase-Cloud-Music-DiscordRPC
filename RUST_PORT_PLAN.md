# Rust Port Plan — Music Discord RPC

A plan to rewrite the current C# / .NET 9 (WinForms) application in Rust, preserving
behaviour and the "ultra-low footprint" spirit of the project while gaining a smaller,
dependency-light, single static binary.

---

## 1. What the app does today

`MusicRpc` (project codename *Vanessa*) is a **Windows-only** system-tray daemon that
mirrors the currently-playing song in **NetEase Cloud Music** or **Tencent QQ Music**
to a Discord **Rich Presence** activity.

High-level flow:

1. Enforce a single running instance (named `Mutex`).
2. On first run, register the app for auto-start (`HKCU\...\Run`).
3. Initialise **two** Discord RPC clients, one per music app (different Discord App IDs).
4. Show a tray icon with an `AutoStart` toggle and `Exit`.
5. Every **233 ms**, in a background loop:
   - Find the target window by **window class name**
     (`OrpheusBrowserHost` → NetEase, `QQMusic_Daemon_Wnd` → QQ Music) and get its PID.
   - Build/reuse the matching player reader.
   - Read the current track (title / artists / album / cover / position / duration / paused).
   - `Update` or `ClearPresence` on the corresponding Discord client.

### How the track data is obtained (the hard part)

This is not an API integration — it reads the music clients' **process memory** directly.

- **NetEase** (`cloudmusic.dll`, **64-bit**):
  - AOB **pattern scan** of the module's `.text` section to locate two RIP-relative
    references — the `AudioPlayer` object and the playback `schedule` (position) global.
  - Play status / song id / duration are read at fixed offsets from the `AudioPlayer` pointer
    (`0x60` status, `0x50` play-info → SSO `std::string` song id, `0xA8` duration).
  - Track **metadata** (title/artists/album/cover) is *not* read from memory — it is parsed
    from an on-disk JSON playlist at
    `%LocalAppData%\NetEase\CloudMusic\WebData\file\playingList`, matched by song id.
- **QQ Music** (`QQMusic.dll`, **32-bit**):
  - AOB pattern scan to find the absolute address of a `CurrentSongInfo` struct.
  - Everything (title/artist/album/thumbnail URL as `std::string`s, plus id/duration/
    schedule/status as ints) is read straight from that struct at fixed offsets.

### Source map

| Area | C# file | Responsibility |
|---|---|---|
| Entry point, tray, poll loop | `Vanessa/Program.cs` | Mutex, RPC clients, tray menu, 233 ms update loop |
| Player abstraction | `Vanessa/MusicPlayer.cs` | `IMusicPlayer { Validate(pid); GetPlayerInfo(); }` |
| NetEase reader | `Vanessa/Players/NetEase.cs` | Pattern scan + memory + on-disk playlist JSON |
| QQ Music reader | `Vanessa/Players/Tencent.cs` | Pattern scan + memory struct read |
| Memory + pattern scan | `Vanessa/Win32Api/Memory.cs` | `OpenProcess`/`ReadProcessMemory`, PE `.text` parse, AOB scan |
| Window lookup | `Vanessa/Win32Api/User32.cs` | `EnumWindows` + class-name match → PID |
| Auto-start | `Vanessa/Win32Api/AutoStart.cs` | `HKCU\...\Run` registry value |
| Data model | `Vanessa/Models/PlayerInfo.cs` | `PlayerInfo` record |
| Config | `Vanessa/Configurations.cs`, `Constants.cs` | Barely used; writes `{}` to a config file |
| Discord IPC | `submodule/discord-rpc-csharp` | Lachee's DiscordRPC library (named-pipe IPC) |

---

## 2. Goals & constraints for the Rust version

- **Behavioural parity** with the current app (same presence content, same detection).
- **Windows-only in practice.** The whole value proposition is reading Windows builds of
  these two apps' memory; there is no cross-platform target. But the code will be structured
  with a clean `platform` boundary so the platform-agnostic parts (model, RPC update logic)
  are testable off-Windows.
- **Tiny footprint**, honouring the project's stated obsession with low memory
  (the C# loop even calls `GC.Collect()` each tick). Rust gives deterministic, allocation-light
  operation with no runtime/GC and a single self-contained `.exe`.
- **No unsafe leaks.** Fix the handle leaks and logic bugs found during analysis (see §7).

---

## 3. Crate selection

| Need | Crate | Notes |
|---|---|---|
| Win32 bindings | **`windows`** (official) | `OpenProcess`, `ReadProcessMemory`, `EnumWindows`, `GetClassNameW`, `GetWindowThreadProcessId`, `CreateToolhelp32Snapshot`/`Module32FirstW` for module base+size, `CreateMutexW`. Use feature-gated submodules to keep it lean. |
| Discord Rich Presence | **`discord-rich-presence`** (vionya) | Verified to support everything used today: `ActivityType::Listening`, `Timestamps` (start+end → time bar), `Assets` (large/small image key+text), and `buttons` (≤2). Pure-Rust IPC over the Discord named pipe. |
| Tray icon + menu | **`tray-icon`** | `TrayIconBuilder` + `Menu`/`MenuItem`; needs a Win32 message loop on the creating thread. Pair with a minimal **`winit`** (or `tao`) event loop, or `windows` `GetMessageW` loop. `trayicon` (Ciantic) is a lighter Windows-only alternative if we want zero winit. |
| JSON | **`serde`** + **`serde_json`** | Deserialize the NetEase `playingList`. |
| Auto-start | **`winreg`** (or **`auto-launch`**) | Write/read/delete `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. `winreg` keeps us closest to current behaviour. |
| Errors | **`anyhow`** (app) + **`thiserror`** (library errors) | |
| Logging (optional) | **`log`** + **`env_logger`** | Replace `Debug.Print`/`MessageBox` error spam with real logging. |
| Icon/version resource | **`winres`** or **`embed-resource`** (build-dep) | Embed `icon.ico` and Windows version info into the exe. |

Single-instance can be done directly with `CreateMutexW` + `GetLastError()==ERROR_ALREADY_EXISTS`
(no extra crate needed), mirroring the C# `Mutex`.

---

## 4. Target module layout

```
music-rpc/
├── Cargo.toml
├── build.rs                      # embed icon + version resource (winres)
├── assets/icon.ico
└── src/
    ├── main.rs                   # #![windows_subsystem = "windows"]; mutex, wiring, tray+loop
    ├── app.rs                    # tray icon, menu, event loop, worker-thread lifecycle
    ├── updater.rs                # the 233 ms poll loop; picks player, drives RPC
    ├── model.rs                  # PlayerInfo struct
    ├── config.rs                 # first-run detection + config file
    ├── rpc.rs                    # thin wrapper: PlayerInfo -> discord activity; connect/reconnect
    ├── players/
    │   ├── mod.rs                # trait MusicPlayer { validate(pid)->bool; player_info()->Option<PlayerInfo> }
    │   ├── netease.rs
    │   └── tencent.rs
    └── platform/                 # all #[cfg(windows)] Win32 code behind one boundary
        ├── mod.rs
        ├── process_memory.rs     # OwnedHandle + typed reads (u8/i16/i32/u32/i64/f32/f64/bytes)
        ├── pattern.rs            # signature parse + PE .text scan (port of Memory.FindPattern)
        ├── window.rs             # find window by class name -> (title, pid)
        └── autostart.rs          # registry Run key
```

Rationale: `model.rs`, `rpc.rs`, and the `MusicPlayer` trait are platform-agnostic and unit-testable
anywhere; everything touching Win32 is quarantined under `platform/` and the player readers.

---

## 5. Key type & API mappings

**`PlayerInfo`** (`model.rs`) — direct port of the record:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    pub identity: String,
    pub title:    String,
    pub artists:  String,
    pub album:    String,
    pub cover:    String,
    pub schedule: f64,   // seconds elapsed
    pub duration: f64,   // seconds total
    pub url:      String,
    pub paused:   bool,
}
```

**Player trait** (`players/mod.rs`):

```rust
pub trait MusicPlayer {
    fn validate(&self, pid: u32) -> bool;         // is this reader still bound to that PID?
    fn player_info(&self) -> Option<PlayerInfo>;  // None => nothing playing / not resolvable
}
```

**Process memory** (`platform/process_memory.rs`) — RAII over the leaked C# handle. Because the two
targets differ in bitness, expose width-explicit reads and a `read_ptr` that the caller selects:

```rust
pub struct ProcessMemory { handle: OwnedHandle }   // closes on Drop (fixes handle leak)

impl ProcessMemory {
    pub fn open(pid: u32) -> Result<Self>;                 // OpenProcess(PROCESS_VM_READ|QUERY_INFORMATION)
    pub fn read_bytes(&self, addr: usize, len: usize) -> Result<Vec<u8>>;
    pub fn read_i32(&self, addr: usize, off: usize) -> Result<i32>;
    pub fn read_u32(&self, addr: usize, off: usize) -> Result<u32>;
    pub fn read_i64(&self, addr: usize, off: usize) -> Result<i64>;
    pub fn read_f32(&self, addr: usize, off: usize) -> Result<f32>;
    pub fn read_f64(&self, addr: usize, off: usize) -> Result<f64>;
    // ...read_i16, etc.
}
```

**Discord RPC** (`rpc.rs`) — the `Program.UpdateThread` body becomes:

```rust
// paused -> clear; else build activity
let start = now_unix() - info.schedule as i64;
let activity = Activity::new()
    .activity_type(ActivityType::Listening)
    .details(&format!("🎵 {}", info.title))
    .state(&format!("🎤 {}", info.artists))
    .timestamps(Timestamps::new().start(start).end(start + info.duration as i64))
    .assets(Assets::new()
        .large_image(&info.cover).large_text(&format!("💿 {}", info.album))
        .small_image("timg").small_text("NetEase CloudMusic"))
    .buttons(vec![
        Button::new("🎧 Listen", &info.url),
        Button::new("👏 View App on GitHub", "https://github.com/Kxnrl/NetEase-Cloud-Music-DiscordRPC"),
    ]);
client.set_activity(activity)?;
```

---

## 6. The tricky ports (call these out early)

### 6.1 AOB pattern scanning + PE parse
Port `Memory.FindPattern` faithfully:
- Parse a signature string like `"48 8D 0D ? ? ? ?"` into `Vec<Option<u8>>` (`?` → `None`).
- Read the target's PE headers *through* `ReadProcessMemory` (as the C# does): `e_lfanew` at `0x3C`,
  section count, optional-header size, then walk section headers to find `.text`
  (the magic `0x747865742E` is the little-endian bytes of `".text"`), read that whole section
  into a buffer, and scan.
- Keep the C# semantics exactly (first-byte fast-path, wildcard skip).

### 6.2 Two different pointer resolutions (and two bitnesses)
- **NetEase (64-bit, RIP-relative):** at match `app`, `disp = read_i32(app+3)`,
  `ptr = (app+3) + disp + 4`. Schedule pattern uses `app+4` similarly.
- **QQ Music (32-bit, absolute):** `addr = read_i32(match+1)` gives the absolute struct address.

The Rust `ProcessMemory` must therefore read **32-bit pointers** for QQ Music and **64-bit pointers**
for NetEase. Model this explicitly (e.g. `read_u32` vs `read_i64` at the call sites, matching the C#)
so a future maintainer can't accidentally read the wrong width.

### 6.3 Reading a remote `std::string` (MSVC SSO)
Both readers decode an MSVC `std::basic_string` with small-string optimisation:
- length at `base + 0x10`; if `len <= 15` the bytes are inline at `base`, else the pointer to the
  heap buffer is at `base`.
- **NetEase**: 64-bit — length is `i64`, buffer pointer is `i64`, `StdStringSize` effectively 0x20.
- **QQ Music**: 32-bit — length is `i32`, buffer pointer is `i32`, `StdStringSize = 0x18`.

Provide one helper per bitness (`read_std_string_x64`, `read_std_string_x86`) and decode as UTF-8.

### 6.4 Window discovery
`User32.GetWindowTitle(className, out title, out pid)` → `platform::window::find_by_class(name)`
returning `Option<(String, u32)>` via `EnumWindows` + `GetClassNameW` + `GetWindowThreadProcessId`.
Callback carries state through the `lparam` (box a closure, or use a `static`-free trampoline).

### 6.5 Tray + threading
- Tray icon and its menu must live on a thread running a Win32 message loop (main thread).
- Run the 233 ms **poll+RPC loop on a worker thread**; keep each `DiscordIpcClient` on that worker
  (avoids `Send` concerns with the IPC socket).
- Communicate `Exit` and `AutoStart toggle` from the tray (main thread) to the worker via a
  `std::sync::mpsc` channel and/or `AtomicBool`. On `Exit`, hide the tray icon and stop the loop.

### 6.6 Single instance & auto-start
- `CreateMutexW(NULL, TRUE, "MusicDiscordRpc")`; if `GetLastError()==ERROR_ALREADY_EXISTS`, show a
  message box and exit — same UX as today.
- Auto-start: write `NCM-DiscordRpc = <exe path>` under `HKCU\...\Run` via `winreg`;
  `check()` compares the stored value to the current exe path.

---

## 7. Bugs to fix (don't port these faithfully)

Found while reading the C#; the Rust version should correct them:

1. **Handle leak.** `Memory.FindPattern` does `new ProcessMemory(processId)` (→ `OpenProcess`) and
   never closes the handle; the player constructors open yet another. Fix with an `OwnedHandle`
   that closes on `Drop`, opened **once** per player and reused.
2. **Inverted first-run flag.** `Configurations.IsFirstLoad = File.Exists(_path)` is backwards
   (true means the file *already* existed), so `Program` re-enables auto-start on essentially every
   run except the very first. In Rust, treat "config file absent" as first run and only then enable
   auto-start.
3. **Wrong PID in the QQ Music branch.** `Program.UpdateThread` validates the cached instance with
   `lastInstance.Validate(netEaseProcessId)` inside the *Tencent* branch (should be `tencentId`),
   so the QQ Music reader is rebuilt every tick. Use the correct PID.
4. **Error UX.** Every loop exception pops a `MessageBox`, which can spam modal dialogs. Replace with
   `log::error!` + optional throttled balloon notification.
5. **`GC.Collect()` per tick** — simply not needed; drop it.

---

## 8. Phased implementation

Each phase is independently verifiable.

- **Phase 0 — Scaffold.** `cargo new`, `Cargo.toml` deps, `build.rs` embedding `icon.ico` + version
  info, `#![windows_subsystem = "windows"]`, release profile tuned for size
  (`opt-level="z"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip=true`).
- **Phase 1 — Platform primitives.** `process_memory.rs`, `pattern.rs`, `window.rs`, `autostart.rs`.
  Unit-test the signature parser and the pattern matcher against a synthetic in-memory buffer
  (no target process needed).
- **Phase 2 — Players.** `model.rs`, `MusicPlayer` trait, `netease.rs` (+ playlist JSON via serde),
  `tencent.rs`. Manual verification against live NetEase / QQ Music.
- **Phase 3 — RPC + loop.** `rpc.rs` wrapper, `updater.rs` poll loop, reconnect handling. Verify a
  real presence appears in Discord with correct title/artist/cover/time bar/buttons.
- **Phase 4 — Tray + lifecycle.** `app.rs` tray, menu (`AutoStart` toggle with ✓/✗, `Exit`),
  single-instance mutex, first-run auto-start. Verify parity with the C# tray UX.
- **Phase 5 — Packaging & parity pass.** GitHub Actions `windows-latest` build (replace the current
  `dotnet` workflow with `cargo build --release`), artifact upload, and a side-by-side parity check
  vs the C# build for both players (playing, paused, switching apps, nothing playing, app closed).

---

## 9. Risks & open questions

- **Version-specific offsets/patterns.** The AOB signatures and struct offsets are tied to specific
  NetEase/QQ Music builds. The port copies the *current* constants verbatim; it does not make them
  more robust. Keep them in one clearly-labelled place (as today) for easy updating, and consider a
  config/override file so users can patch offsets without a rebuild.
- **AV / anti-cheat friction.** `OpenProcess` + `ReadProcessMemory` against third-party apps can trip
  antivirus heuristics. A Rust exe is no worse than the C# one, but expect the same false positives;
  document it and request only `PROCESS_VM_READ | PROCESS_QUERY_INFORMATION`.
- **Discord activity-type fidelity.** Confirmed `discord-rich-presence` exposes `ActivityType::Listening`
  and the start+end timestamp "time bar", matching the C# `ActivityType.Listening` + `Timestamps`.
- **Tray/event-loop threading.** `tray-icon` requires the message loop on the tray's thread; the design
  in §6.5 keeps the tray on main and the RPC loop on a worker to avoid `Send`/loop-blocking issues.
- **Two Discord App IDs.** Keep both (`481562643958595594` NetEase, `903485504899665990` Tencent) and
  the two-client design so each app shows its own Discord application identity.

---

## 10. Rough dependency block (starting point)

```toml
[package]
name = "music-rpc"
version = "3.0.0"
edition = "2021"

[dependencies]
discord-rich-presence = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
winreg = "0.52"
tray-icon = "0.19"
winit = "0.30"                 # or `tao`; or drop for a raw GetMessageW loop
log = "0.4"
env_logger = "0.11"

[target.'cfg(windows)'.dependencies.windows]
version = "0.58"
features = [
  "Win32_Foundation",
  "Win32_System_Threading",
  "Win32_System_Diagnostics_ToolHelp",
  "Win32_System_Diagnostics_Debug",   # ReadProcessMemory
  "Win32_UI_WindowsAndMessaging",
]

[build-dependencies]
winres = "0.1"

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

*(Crate versions are indicative — pin to the latest at implementation time.)*

---

## 11. Effort estimate

Small codebase (~700 LoC of C#). A faithful port is roughly:

- Phase 0–1: ~1 day (platform primitives + tests are the foundation).
- Phase 2: ~1–1.5 days (the two readers, incl. SSO string + JSON, live testing).
- Phase 3–4: ~1 day (RPC wrapper, loop, tray, lifecycle).
- Phase 5: ~0.5 day (CI + parity pass).

**≈ 3–4 focused days** for behavioural parity, most of it spent validating memory reads against live
clients rather than writing code.
