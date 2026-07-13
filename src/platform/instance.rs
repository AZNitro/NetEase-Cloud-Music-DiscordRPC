//! Single-instance guard via a named mutex (`MusicDiscordRpc`), matching the C# app.

use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

/// RAII wrapper that keeps the mutex held for the process lifetime.
pub struct InstanceLock {
    handle: HANDLE,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Acquire the single-instance mutex. On conflict, shows a message box and returns `None`.
pub fn try_acquire() -> Option<InstanceLock> {
    // UTF-16 "MusicDiscordRpc\0"
    let name: Vec<u16> = "MusicDiscordRpc\0".encode_utf16().collect();
    let handle = unsafe { CreateMutexW(ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
            let title: Vec<u16> = "Error\0".encode_utf16().collect();
            let body: Vec<u16> = "MusicDiscordRpc is already running.\0".encode_utf16().collect();
            MessageBoxW(ptr::null_mut(), body.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
        }
        return None;
    }
    Some(InstanceLock { handle })
}
