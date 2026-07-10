//! Find a top-level window by its class name and return its title + owning PID.
//! Port of the C# `Win32Api/User32.cs` `GetWindowTitle(match, ...)`.

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

struct Finder {
    target_class: String,
    found_pid: u32,
    found_title: String,
}

/// Enumerate all top-level windows; the *last* window whose class matches
/// `class_name` wins (mirroring the C#). Returns `(title, pid)` for any match
/// with a valid PID.
///
/// Unlike the C#, this does **not** require a non-empty window title: the title
/// is discarded by every caller, and the daemon windows we target may legitimately
/// have no title, which would otherwise make detection silently fail.
pub fn find_by_class(class_name: &str) -> Option<(String, u32)> {
    let mut finder = Finder {
        target_class: class_name.to_string(),
        found_pid: 0,
        found_title: String::new(),
    };

    // SAFETY: FFI; `enum_proc` only dereferences the pointer we pass here, and
    // `finder` outlives the (synchronous) EnumWindows call.
    unsafe {
        EnumWindows(Some(enum_proc), &mut finder as *mut Finder as LPARAM);
    }

    if finder.found_pid != 0 {
        Some((finder.found_title, finder.found_pid))
    } else {
        None
    }
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
                finder.found_pid = pid;
                finder.found_title = window_title(hwnd);
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
