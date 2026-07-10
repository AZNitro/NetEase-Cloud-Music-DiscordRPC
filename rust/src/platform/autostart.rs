//! HKCU auto-start registration. Port of `Win32Api/AutoStart.cs`.

use std::path::PathBuf;

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::diag;

const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "NCM-DiscordRpc";

fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

fn exe_path_string() -> Option<String> {
    exe_path().map(|p| p.to_string_lossy().into_owned())
}

/// Enable or disable launching at Windows logon.
pub fn set(enable: bool) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = match hkcu.create_subkey(RUN_SUBKEY) {
        Ok(k) => k,
        Err(e) => {
            diag!("[autostart] cannot open Run key: {e}");
            return;
        }
    };

    if enable {
        let Some(exe) = exe_path_string() else {
            diag!("[autostart] cannot resolve current exe path");
            return;
        };
        match key.set_value(VALUE_NAME, &exe) {
            Ok(()) => diag!("[autostart] enabled → {exe}"),
            Err(e) => diag!("[autostart] enable failed: {e}"),
        }
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => diag!("[autostart] disabled"),
            // Value already absent is fine.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                diag!("[autostart] already disabled");
            }
            Err(e) => diag!("[autostart] disable failed: {e}"),
        }
    }
}

/// `true` when the Run key points at this executable.
pub fn check() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(key) = hkcu.open_subkey(RUN_SUBKEY) else {
        return false;
    };
    let Ok(value): Result<String, _> = key.get_value(VALUE_NAME) else {
        return false;
    };
    exe_path_string().is_some_and(|exe| exe == value)
}
