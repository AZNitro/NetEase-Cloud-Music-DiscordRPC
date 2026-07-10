//! First-run detection. Fixes the inverted C# flag:
//! `IsFirstLoad = File.Exists` was backwards — here "config absent" means first run.

use std::fs;
use std::path::PathBuf;

fn config_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Vanessa").join("config.json")
}

/// `true` when this is the first launch (config file does not exist yet).
pub fn is_first_run() -> bool {
    !config_path().exists()
}

/// Persist a minimal config so subsequent launches are not treated as first-run.
pub fn mark_initialized() {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&path, "{}");
}
