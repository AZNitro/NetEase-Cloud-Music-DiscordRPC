//! Tiny dependency-free diagnostic logger.
//!
//! Every line is written to **both** stderr (so you can watch live in a
//! terminal) and a `music-rpc.log` file next to the executable (so you can copy
//! it out after a run). This is deliberately verbose: because the app reads
//! another process's memory, the interesting failures are invisible unless every
//! risky step logs the concrete addresses and values it saw.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// Open the log file. Safe to call more than once; only the first call takes
/// effect.
pub fn init() {
    let path = log_path();
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();

    match &file {
        Some(_) => eprintln!("[log] writing diagnostics to {}", path.display()),
        None => eprintln!("[log] could not open {} (stderr only)", path.display()),
    }

    let _ = LOG_FILE.set(Mutex::new(file));
}

fn log_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("music-rpc.log");
        }
    }
    PathBuf::from("music-rpc.log")
}

/// Write one timestamped diagnostic line. Prefer the [`crate::diag!`] macro.
pub fn write_line(msg: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!("[{ts}] {msg}");

    eprintln!("{line}");

    if let Some(lock) = LOG_FILE.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    }
}

/// Format and write a diagnostic line, `println!`-style.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::logging::write_line(&format!($($arg)*))
    };
}
