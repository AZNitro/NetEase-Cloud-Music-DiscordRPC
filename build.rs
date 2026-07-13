// Embed the app icon (and version info) into the .exe, so it shows the icon in
// Explorer instead of the generic one. Only runs when building on Windows; on
// other hosts it is a no-op, so cross-checks don't need a resource compiler.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // Non-fatal: without a resource compiler the exe just keeps the
            // default icon; the build still succeeds.
            println!("cargo:warning=icon embedding skipped: {e}");
        }
    }
}
