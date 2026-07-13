//! Find top-level windows by class name and return their titles + owning PIDs.
//! Port of the C# `Win32Api/User32.cs`, extended to return every match (with its
//! title) so the caller can log them and pick the most useful one.

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

struct Finder {
    target_class: String,
    matches: Vec<(String, u32)>,
}

/// Every top-level window whose class equals `class_name`, as `(title, pid)`.
pub fn find_all_by_class(class_name: &str) -> Vec<(String, u32)> {
    let mut finder = Finder {
        target_class: class_name.to_string(),
        matches: Vec::new(),
    };

    // SAFETY: FFI; `enum_proc` only touches the pointer we pass, and `finder`
    // outlives the synchronous EnumWindows call.
    unsafe {
        EnumWindows(Some(enum_proc), &mut finder as *mut Finder as LPARAM);
    }

    finder.matches
}

/// One window for `class_name`, preferring a match with a non-empty title (that's
/// the one that carries the "Song - Artist" text), else the last match. Returns
/// `(title, pid)`.
pub fn find_by_class(class_name: &str) -> Option<(String, u32)> {
    let matches = find_all_by_class(class_name);
    matches
        .iter()
        .rev()
        .find(|(title, _)| !title.is_empty())
        .cloned()
        .or_else(|| matches.last().cloned())
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    let finder = &mut *(lparam as *mut Finder);

    let mut class_buf = [0u16; 256];
    let len = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
    if len > 0 {
        let class = String::from_utf16_lossy(&class_buf[..len as usize]);
        if class == finder.target_class {
            let mut pid: u32 = 0;
            let tid = GetWindowThreadProcessId(hwnd, &mut pid);
            if tid != 0 && pid != 0 {
                finder.matches.push((window_title(hwnd), pid));
            }
        }
    }

    1 // TRUE: keep enumerating
}

unsafe fn window_title(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if got <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..got as usize])
}
