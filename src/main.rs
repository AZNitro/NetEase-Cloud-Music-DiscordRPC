// Console subsystem so Ctrl+C works during testing. Tray Exit also quits.
// (Switch back to windows_subsystem once you no longer need a console.)
// #![windows_subsystem = "windows"]

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
