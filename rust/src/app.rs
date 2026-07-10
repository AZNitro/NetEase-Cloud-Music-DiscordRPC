//! Tray icon, menu, and process lifecycle using raw Win32
//! (`Shell_NotifyIcon` + a message-only window). The Discord poll loop runs on
//! a worker thread; this module owns the message pump on the main thread.

use std::mem::{size_of, zeroed};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadImageW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetForegroundWindow, TrackPopupMenu, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED,
    MF_SEPARATOR, MF_STRING, MSG, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TPM_RETURNCMD, WM_APP,
    WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

use crate::diag;
use crate::platform::{autostart, instance};
use crate::updater::{self, Command};

const WM_TRAY: u32 = WM_APP + 1;
const ID_AUTOSTART: usize = 1001;
const ID_EXIT: usize = 1002;

/// Tray HWND for the Ctrl+C handler (posts Exit to the UI thread).
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

struct TrayState {
    cmd_tx: Sender<Command>,
    running: Arc<AtomicBool>,
    nid: NOTIFYICONDATAW,
}

/// Full application entry: single-instance → first-run auto-start → tray + worker.
pub fn run() -> anyhow::Result<()> {
    crate::logging::init();
    diag!("MusicRpc starting (console + tray; Ctrl+C or tray Exit to quit)");

    let _lock = match instance::try_acquire() {
        Some(lock) => lock,
        None => return Ok(()),
    };

    if crate::config::is_first_run() {
        diag!("[app] first run — enabling auto-start");
        autostart::set(true);
        crate::config::mark_initialized();
    }

    let running = Arc::new(AtomicBool::new(true));
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

    let worker_flag = running.clone();
    let worker = thread::spawn(move || {
        if let Err(e) = updater::run_loop(worker_flag, cmd_rx) {
            diag!("[app] poll loop exited with error: {e:#}");
        }
    });

    if let Err(e) = run_tray(running.clone(), cmd_tx) {
        diag!("[app] tray loop error: {e:#}");
        running.store(false, Ordering::SeqCst);
    }

    running.store(false, Ordering::SeqCst);
    let _ = worker.join();
    Ok(())
}

fn run_tray(running: Arc<AtomicBool>, cmd_tx: Sender<Command>) -> anyhow::Result<()> {
    let class_name: Vec<u16> = "MusicRpcTray\0".encode_utf16().collect();

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(ptr::null()) },
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: ptr::null_mut(),
    };

    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        anyhow::bail!("RegisterClassExW failed: {}", std::io::Error::last_os_error());
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            ptr::null_mut(),
            ptr::null_mut(),
            wc.hInstance,
            ptr::null(),
        )
    };
    if hwnd.is_null() {
        anyhow::bail!("CreateWindowExW failed: {}", std::io::Error::last_os_error());
    }

    let icon = load_tray_icon(wc.hInstance);

    let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    write_tip(&mut nid, "NetEase Cloud Music DiscordRPC");

    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    if ok == 0 {
        anyhow::bail!("Shell_NotifyIconW(NIM_ADD) failed");
    }

    let state = Box::new(TrayState {
        cmd_tx,
        running: running.clone(),
        nid,
    });
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            Box::into_raw(state) as isize,
        );
    }

    TRAY_HWND.store(hwnd as isize, Ordering::SeqCst);
    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
    }
    diag!("[app] tray icon ready (Ctrl+C / tray Exit to quit)");

    // Standard blocking message loop — exits on PostQuitMessage from Exit/Ctrl+C.
    unsafe {
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    TRAY_HWND.store(0, Ordering::SeqCst);
    Ok(())
}

unsafe extern "system" fn console_ctrl_handler(ctrl: u32) -> i32 {
    match ctrl {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            let hwnd = TRAY_HWND.load(Ordering::SeqCst) as HWND;
            if !hwnd.is_null() {
                // Route through the same Exit path as the tray menu.
                PostMessageW(hwnd, WM_COMMAND, ID_EXIT as WPARAM, 0);
            }
            1 // handled — don't let the default handler kill us mid-clear
        }
        _ => 0,
    }
}

fn write_tip(nid: &mut NOTIFYICONDATAW, tip: &str) {
    let encoded: Vec<u16> = tip.encode_utf16().chain(std::iter::once(0)).collect();
    let len = encoded.len().min(nid.szTip.len());
    nid.szTip[..len].copy_from_slice(&encoded[..len]);
}

fn load_tray_icon(hinstance: windows_sys::Win32::Foundation::HINSTANCE) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    // Prefer the bundled .ico written next to the exe at runtime; fall back to IDI_APPLICATION.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("icon.ico");
            if !path.exists() {
                let _ = std::fs::write(&path, include_bytes!("../assets/icon.ico"));
            }
            if path.exists() {
                let wide: Vec<u16> = path
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let icon = unsafe {
                    LoadImageW(
                        ptr::null_mut(),
                        wide.as_ptr(),
                        IMAGE_ICON,
                        0,
                        0,
                        LR_DEFAULTSIZE | windows_sys::Win32::UI::WindowsAndMessaging::LR_LOADFROMFILE,
                    )
                };
                if !icon.is_null() {
                    return icon as _;
                }
            }
        }
    }
    unsafe {
        LoadImageW(
            hinstance,
            IDI_APPLICATION,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        ) as _
    }
}

fn auto_label() -> String {
    let mark = if autostart::check() { "√" } else { "✘" };
    format!("AutoStart    {mark}")
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let mut pt: POINT = zeroed();
        GetCursorPos(&mut pt);
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        let auto: Vec<u16> = auto_label().encode_utf16().chain(std::iter::once(0)).collect();
        let exit: Vec<u16> = "Exit\0".encode_utf16().collect();
        AppendMenuW(menu, MF_STRING, ID_AUTOSTART, auto.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, ID_EXIT, exit.as_ptr());

        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            ptr::null(),
        ) as usize;
        DestroyMenu(menu);

        if cmd != 0 {
            // Re-enter through WM_COMMAND so wnd_proc owns the logic.
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                hwnd,
                WM_COMMAND,
                cmd as WPARAM,
                0,
            );
        }
    }
}

fn take_state(hwnd: HWND) -> Option<*mut TrayState> {
    unsafe {
        let ptr = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
        ) as *mut TrayState;
        if ptr.is_null() {
            None
        } else {
            Some(ptr)
        }
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = lparam as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                show_context_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            let id = wparam;
            if let Some(state_ptr) = take_state(hwnd) {
                let state = &mut *state_ptr;
                match id {
                    ID_AUTOSTART => {
                        let enabled = autostart::check();
                        autostart::set(!enabled);
                        diag!("[app] auto-start toggled → {}", !enabled);
                    }
                    ID_EXIT => {
                        diag!("[app] Exit clicked");
                        let _ = state.cmd_tx.send(Command::Exit);
                        state.running.store(false, Ordering::SeqCst);
                        Shell_NotifyIconW(NIM_DELETE, &state.nid);
                        if !state.nid.hIcon.is_null() {
                            DestroyIcon(state.nid.hIcon);
                            state.nid.hIcon = ptr::null_mut();
                        }
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
            }
            0
        }
        WM_DESTROY => {
            if let Some(state_ptr) = take_state(hwnd) {
                let state = Box::from_raw(state_ptr);
                windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    0,
                );
                Shell_NotifyIconW(NIM_DELETE, &state.nid);
                if !state.nid.hIcon.is_null() {
                    DestroyIcon(state.nid.hIcon);
                }
                drop(state);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
