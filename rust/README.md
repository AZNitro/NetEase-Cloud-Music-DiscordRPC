# Music Discord RPC — Rust port

A Rust rewrite of the C# app in this repo, focused on **NetEase Cloud Music**
only. See [`../RUST_PORT_PLAN.md`](../RUST_PORT_PLAN.md) for the original design.

> **Status: tray app.** Window detection, memory reading, pattern scanning, the
> NetEase reader, Discord Rich Presence, tray icon, auto-start, and
> single-instance guard are in place. Diagnostics go to `music-rpc.log` next to
> the executable (Windows subsystem — no console window).

## What works

- Cross-compiles / type-checks for Windows (`cargo check --target x86_64-pc-windows-gnu`).
- Unit tests for the AOB pattern scanner and poll-loop clear/retry helpers.
- Tray icon with **AutoStart** toggle and **Exit**.
- Single-instance mutex (`MusicDiscordRpc`).
- First-run auto-start (fixed: config *absent* ⇒ first run).
- Presence cleared when the NetEase window disappears.
- Failed reader attaches retry with backoff (startup DLL race).
- NetEase web API over HTTPS (WinHTTP / Schannel).

The NetEase reader auto-selects a mode by client bitness:

- **64-bit client → memory mode:** precise position/status from `cloudmusic.dll`
  memory (AOB pattern scan), metadata from the local playlist / web API by song id
  (cached per song; falls back to an id-only presence if metadata is missing).
- **32-bit client → title mode:** `Song - Artist` from the `OrpheusBrowserHost`
  window title; cover/album/duration from playlist or web API search; position
  tracked while the title is shown. (WASAPI play/pause was removed — it
  false-paused on Chromium NetEase and cleared Discord.)

## Build & run (on Windows)

Requires the [Rust toolchain](https://rustup.rs/).

```powershell
cd rust
cargo run --release
```

With **Discord** open and **NetEase Cloud Music** playing, the tray icon appears
and Rich Presence updates. Diagnostics are appended to `music-rpc.log` next to
the executable (`rust/target/release/music-rpc.log` when running from cargo).

## If something's wrong

Send `music-rpc.log`. Typical lines:

- `AudioPlayer pattern not found` → the app updated and the AOB signature needs refreshing.
- `[loop] failed to init ...; retrying soon` → DLL not ready yet (will retry).
- `[rpc] connect failed` → Discord isn't running / IPC pipe unavailable.
- `no NetEase window found (waiting)` forever → window class name changed.

## Tests

```powershell
cargo test                                   # pure-logic tests (run anywhere)
cargo check --target x86_64-pc-windows-gnu   # type-check the Windows code from any OS
```

## Layout

```
src/
├── main.rs            entry (windows subsystem)
├── lib.rs             module wiring
├── app.rs             tray, single-instance, first-run auto-start
├── config.rs          first-run config file (fixed polarity)
├── logging.rs         file diagnostic logger
├── loop_logic.rs      clear/retry helpers (unit-tested)
├── model.rs           PlayerInfo
├── pattern.rs         AOB signature parse + scan (unit-tested)
├── rpc.rs             PlayerInfo -> Discord activity
├── updater.rs         233 ms poll loop (worker thread)
├── players/           netease.rs, MusicPlayer trait
└── platform/          memory, window, http, autostart, instance
assets/
└── icon.ico           tray icon
```
