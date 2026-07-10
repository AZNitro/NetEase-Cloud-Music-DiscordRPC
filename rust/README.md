# Music Discord RPC — Rust port

A Rust rewrite of the C# app in this repo. See [`../RUST_PORT_PLAN.md`](../RUST_PORT_PLAN.md)
for the full design.

> **Status: testable console MVP.** The hard 80% is done — window detection,
> cross-process memory reading, pattern scanning, the NetEase/QQ Music readers,
> Discord Rich Presence, and the poll loop. It runs as a **console app with heavy
> diagnostic logging** so the first round of real-world testing is easy. The tray
> icon, auto-start, and single-instance guard are intentionally deferred until the
> memory reads are confirmed on a live machine.

## What already works (and is verified)

- Cross-compiles cleanly for Windows (`cargo check --target x86_64-pc-windows-gnu`).
- Unit tests for the AOB pattern scanner pass (`cargo test`).
- No clippy warnings.

The NetEase reader auto-selects a mode by client bitness:

- **64-bit client → memory mode:** precise position/status from `cloudmusic.dll`
  memory (AOB pattern scan), metadata from the local playlist / web API by song id.
- **32-bit client → title mode:** `Song - Artist` is read straight from the
  `OrpheusBrowserHost` window title (version-independent). Cover/album/duration are
  filled in from the local playlist (matched by name) or the NetEase web API search.
  Real position **and pause** come from the playback clock, which is
  **auto-discovered** (no version-specific offsets): a one-time ~2.4s scan finds the
  `double` in memory that ticks up like a play position. If discovery fails (e.g.
  nothing playing during the scan), it falls back to approximate title timing.

The web API path uses plain HTTP and is best-effort — if it fails, the presence
still shows song + artist.

The one thing that **cannot** be verified without a Windows machine running the music
apps + Discord is whether the memory offsets/patterns read correctly on your build.
That's what the diagnostic log is for.

## Build & run (on Windows)

Requires the [Rust toolchain](https://rustup.rs/).

```powershell
cd rust
cargo run --release
```

Leave it running in the terminal with **Discord open** and **NetEase Cloud Music**
or **QQ Music** playing a song. You'll see live diagnostics, and the same lines are
written to `music-rpc.log` next to the executable
(`rust/target/release/music-rpc.log`).

## What a healthy log looks like

```
[..] MusicRpc starting (console diagnostic build)
[..] [rpc] connected 481562643958595594
[..] [loop] detected NetEase player window
[..] [netease] cloudmusic.dll base=0x7FF... size=0x... pid=1234
[..] [netease] AudioPlayer match=0x... disp=... ptr=0x...
[..] [netease] Schedule match=0x... disp=... ptr=0x...
[..] [netease] status=1
[..] [netease] current song id = "123456"
[..] [netease] metadata source: local playlist (123456)
[..] [netease] PlayerInfo { title: "...", artists: "...", ... }
```

If the local playlist doesn't have the song you'll instead see the API fallback:

```
[..] [netease] local playlist has no id 123456; falling back to web API
[..] [netease] web API GET http://music.163.com/api/song/detail/?id=123456&ids=%5B123456%5D
```

If the API call fails (`web API request failed` / `parse error`), it likely needs
HTTPS — enabling minreq's `https` feature is the fix.

## If something's wrong

Send me `music-rpc.log` (or the terminal output). The failure will be one specific
line, e.g.:

- `AudioPlayer pattern not found` / `CurrentSongInfo pattern not found`
  → the app updated and the AOB signature needs refreshing.
- `[netease] current song id = ""` or a garbled string
  → an offset drifted in this build.
- `[rpc] connect failed` → Discord isn't running / IPC pipe unavailable.
- `no player window found (waiting)` forever → window class name changed.

Each of these points at exactly one constant to update, so the fix is usually small.

## Tests

```powershell
cargo test                                   # pure-logic tests (run anywhere)
cargo check --target x86_64-pc-windows-gnu   # type-check the Windows code from any OS
```

## Layout

```
src/
├── main.rs            entry point (console for now)
├── lib.rs             module wiring; platform-agnostic vs #[cfg(windows)]
├── logging.rs         dual stderr + file diagnostic logger
├── model.rs           PlayerInfo
├── pattern.rs         AOB signature parse + scan (unit-tested)
├── rpc.rs             PlayerInfo -> Discord activity
├── updater.rs         the 233 ms poll loop
├── players/           netease.rs, tencent.rs, MusicPlayer trait
└── platform/          memory.rs (read + PE scan), window.rs (find by class)
```

## Remaining work (Phase 4+)

- Tray icon + menu (`AutoStart` toggle, `Exit`) and `#![windows_subsystem = "windows"]`.
- Auto-start registry entry (`HKCU\...\Run`).
- Single-instance mutex.
- GitHub Actions workflow: `cargo build --release` on `windows-latest`.
