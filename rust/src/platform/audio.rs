//! Detect whether an app is actively playing audio, via the Windows Core Audio
//! (WASAPI) session API. This is how play/pause is derived for NetEase's 32-bit
//! client, where the window title can't report it and the position isn't at a
//! stable memory address.
//!
//! Everything returns `None` on any failure, so a transient COM error is treated
//! as "unknown" (the caller assumes playing) and never looks like a false pause.

use windows::core::Interface;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionEnumerator,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::diag;

/// Audio peak above this counts as "producing sound" (paused playback reads ~0).
const PEAK_THRESHOLD: f64 = 0.0005;

thread_local! {
    static COM_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_com() {
    COM_READY.with(|ready| {
        if !ready.get() {
            // Safe to call more than once; "already initialised" is fine. We do
            // not uninitialise for the process lifetime.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            ready.set(true);
        }
    });
}

/// `Some(true)` if any render audio session owned by a process whose executable
/// name contains `name_substr` (case-insensitive) is actually producing sound
/// (peak level above threshold); `Some(false)` if such sessions exist but are all
/// silent (paused); `None` if none are found or the query fails.
pub fn any_active_session(name_substr: &str) -> Option<bool> {
    ensure_com();
    match query(name_substr) {
        Ok(result) => result,
        Err(e) => {
            diag!("[audio] session query failed: {e}");
            None
        }
    }
}

fn query(name_substr: &str) -> windows::core::Result<Option<bool>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let sessions: IAudioSessionEnumerator = manager.GetSessionEnumerator()?;
        let count = sessions.GetCount()?;

        let mut matched = false;
        let mut producing_sound = false;
        let mut other_names: Vec<String> = Vec::new();

        for i in 0..count {
            let control = sessions.GetSession(i)?;
            let control2: IAudioSessionControl2 = control.cast()?;
            let pid = control2.GetProcessId().unwrap_or(0);
            let name = if pid != 0 {
                process_image_name(pid).unwrap_or_default().to_ascii_lowercase()
            } else {
                String::new()
            };

            if !name.contains(name_substr) {
                if !name.is_empty() {
                    other_names.push(name);
                }
                continue;
            }

            matched = true;
            let active = control.GetState()? == AudioSessionStateActive;
            // Peak level is the reliable pause signal: a paused session reads ~0
            // even if its state stays "Active".
            let peak = match control.cast::<IAudioMeterInformation>() {
                Ok(meter) => sample_peak(&meter),
                Err(_) => None,
            };
            let playing = match peak {
                Some(p) => p > PEAK_THRESHOLD,
                None => active, // no meter available; fall back to session state
            };
            producing_sound |= playing;
        }

        if !matched {
            diag!("[audio] no '{name_substr}' session among {count}; names seen: {other_names:?}");
            return Ok(None);
        }
        Ok(Some(producing_sound))
    }
}

/// Single peak sample — no sleeps. Callers already throttle WASAPI queries
/// (~400 ms), and a rolling "last known playing" cache absorbs brief zeros.
unsafe fn sample_peak(meter: &IAudioMeterInformation) -> Option<f64> {
    meter.GetPeakValue().ok().map(|p| p as f64)
}

/// The executable file name (e.g. `cloudmusic.exe`) of a process, lowercased by
/// the caller. Uses `windows-sys` to avoid extra `windows` features.
fn process_image_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: FFI; the handle is checked and always closed.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 || len == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
    }
}
