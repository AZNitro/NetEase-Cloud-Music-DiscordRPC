// The first testable build is a console app so diagnostics stream live to the
// terminal. Once the memory reads are confirmed, flip to a tray app by enabling
// the line below (which hides the console window).
// #![windows_subsystem = "windows"]

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
