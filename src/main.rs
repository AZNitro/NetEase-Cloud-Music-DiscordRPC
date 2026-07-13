// Silent tray app: no console window when double-clicked. Diagnostics still go to
// music-rpc.log next to the exe. To debug with a live console + Ctrl+C, comment
// this line out and rebuild.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    music_rpc::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "MusicRpc only runs on Windows — it works by reading the memory of the \
         Windows build of NetEase Cloud Music."
    );
}
