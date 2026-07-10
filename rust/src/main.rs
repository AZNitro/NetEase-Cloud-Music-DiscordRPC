#![windows_subsystem = "windows"]

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    music_rpc::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "MusicRpc only runs on Windows — it works by reading the memory of the \
         Windows builds of NetEase Cloud Music / QQ Music."
    );
}
