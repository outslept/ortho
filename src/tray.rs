//! System tray icon, native Dark Mode popup menu, focus tracking hook, IPC CLI, and startup toggle.
//!
//! Creates a hidden top-level window (required to receive the `"TaskbarCreated"` broadcast
//! if `explorer.exe` restarts), registers a `Shell_NotifyIconW` tray icon, tracks the last
//! focused manageable window via `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`, and posts
//! selected menu or CLI actions back to the main thread message loop via [`WM_APP_TRAY_CMD`].

use std::ffi::{OsStr, c_void};
use std::mem::{size_of, transmute};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, EVENT_SYSTEM_FOREGROUND, FindWindowW, GetCursorPos, GetForegroundWindow, HICON,
    IDI_APPLICATION, LoadIconW, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_SHOWNORMAL,
    SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenuEx, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
    WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCSTR, PCWSTR, w};
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

use crate::hotkey::Command;
use crate::tile::Action;
use crate::win;

/// Window class name used by the hidden tray window and IPC CLI lookup.
const TRAY_CLASS_NAME: PCWSTR = w!("OrthoTrayWindowClass");

/// Custom window message sent by `Shell_NotifyIconW` on tray icon mouse events.
const WM_TRAY_CALLBACK: u32 = WM_APP + 1;

/// Custom window message posted to the main message loop when a tray or CLI command is triggered.
pub const WM_APP_TRAY_CMD: u32 = WM_APP + 2;

/// Tray icon identifier.
const TRAY_ICON_ID: u32 = 1;

/// Embedded application icon resource ID (`MAKEINTRESOURCEW(1)`).
const APP_ICON_RESOURCE_ID: usize = 1;

/// `uxtheme.dll` ordinal 135 (`SetPreferredAppMode` on Windows 10 1903+).
const UXTHEME_ORD_SET_PREFERRED_APP_MODE: usize = 135;

/// `uxtheme.dll` ordinal 136 (`FlushMenuThemes` on Windows 10 1903+).
const UXTHEME_ORD_FLUSH_MENU_THEMES: usize = 136;

/// `PreferredAppMode::AllowDark` value passed to `SetPreferredAppMode`.
const PREFERRED_APP_MODE_ALLOW_DARK: i32 = 1;

/// Registry subkey for current-user login startup items.
const RUN_REG_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// Registry value name used for Ortho startup registration.
const RUN_VALUE_NAME: &str = "Ortho";

// Menu and IPC command IDs.
const ID_LEFT: usize = 101;
const ID_RIGHT: usize = 102;
const ID_TOP: usize = 103;
const ID_BOTTOM: usize = 104;
const ID_TOP_LEFT: usize = 105;
const ID_TOP_RIGHT: usize = 106;
const ID_BOTTOM_LEFT: usize = 107;
const ID_BOTTOM_RIGHT: usize = 108;
const ID_ALMOST: usize = 109;
const ID_MAXIMIZE: usize = 110;
const ID_CENTER: usize = 111;
const ID_RESTORE: usize = 112;
const ID_NEXT_MON: usize = 113;
const ID_PREV_MON: usize = 114;
const ID_OPEN_CONFIG: usize = 201;
const ID_TOGGLE_STARTUP: usize = 202;
const ID_QUIT: usize = 299;

/// Dynamically registered message ID for `"TaskbarCreated"` broadcasts.
/// Accessed only on the main UI thread (`Relaxed` ordering is sufficient).
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);

/// Handle of the most recently focused manageable user window (`HWND` stored as `isize`).
/// Written and read on the main UI thread via `WINEVENT_OUTOFCONTEXT` (`Relaxed` is sufficient).
static LAST_ACTIVE_HWND: AtomicIsize = AtomicIsize::new(0);

/// High-level action selected from the system tray popup menu or IPC CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Execute a window manager command on the active (or last active) window.
    Window(Command),
    /// Open `%APPDATA%\ortho\config.json` in the user's default editor.
    OpenConfig,
    /// Toggle `"Launch at Login"` in `HKCU\...\Run`.
    ToggleStartup,
    /// Exit the application cleanly.
    Quit,
}

/// RAII guard for the system tray icon, hidden host window, and focus-tracking hook.
pub struct Tray {
    hwnd: HWND,
    hook: HWINEVENTHOOK,
}

impl Tray {
    /// Initializes dark mode menu themes, creates the hidden tray window, registers the
    /// system tray icon, and starts the foreground window focus hook.
    #[must_use]
    pub fn new() -> Option<Self> {
        enable_dark_mode_menus();
        // SAFETY: `GetModuleHandleW(None)` retrieves the base address of the current executable.
        let hmodule = unsafe { GetModuleHandleW(None) }.ok()?;
        let hinstance = HINSTANCE::from(hmodule);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_wnd_proc),
            hInstance: hinstance,
            lpszClassName: TRAY_CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: `wc` is a valid `WNDCLASSW` referencing static class name and `tray_wnd_proc`.
        let _ = unsafe { RegisterClassW(&raw const wc) };
        // SAFETY: `"TaskbarCreated"` is a valid null-terminated UTF-16 string literal.
        let taskbar_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        TASKBAR_CREATED_MSG.store(taskbar_msg, Ordering::Relaxed);
        // SAFETY: `TRAY_CLASS_NAME` is registered above and `hinstance` is the current module.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                TRAY_CLASS_NAME,
                w!("Ortho"),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
        }
        .ok()?;
        // SAFETY: `GetForegroundWindow` takes no arguments and returns the current foreground `HWND`.
        let initial_fg = unsafe { GetForegroundWindow() };
        if win::is_manageable_hwnd(initial_fg) {
            LAST_ACTIVE_HWND.store(initial_fg.0 as isize, Ordering::Relaxed);
        }
        // SAFETY: `WINEVENT_OUTOFCONTEXT` delivers events on the calling thread's message loop
        // to `win_event_proc` without injecting a DLL into other processes.
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        add_tray_icon(hwnd);
        Some(Self { hwnd, hook })
    }

    /// Returns the most recently focused manageable user window captured before the tray was clicked.
    #[must_use]
    pub fn last_active_window() -> Option<HWND> {
        let raw = LAST_ACTIVE_HWND.load(Ordering::Relaxed);
        if raw == 0 { None } else { Some(HWND(raw as *mut c_void)) }
    }

    /// Decodes a [`WM_APP_TRAY_CMD`] `wParam` value into a [`TrayEvent`].
    #[must_use]
    pub const fn decode_event(cmd_id: usize) -> Option<TrayEvent> {
        match cmd_id {
            ID_LEFT => Some(TrayEvent::Window(Command::Tile(Action::Left))),
            ID_RIGHT => Some(TrayEvent::Window(Command::Tile(Action::Right))),
            ID_TOP => Some(TrayEvent::Window(Command::Tile(Action::Top))),
            ID_BOTTOM => Some(TrayEvent::Window(Command::Tile(Action::Bottom))),
            ID_TOP_LEFT => Some(TrayEvent::Window(Command::Tile(Action::TopLeft))),
            ID_TOP_RIGHT => Some(TrayEvent::Window(Command::Tile(Action::TopRight))),
            ID_BOTTOM_LEFT => Some(TrayEvent::Window(Command::Tile(Action::BottomLeft))),
            ID_BOTTOM_RIGHT => Some(TrayEvent::Window(Command::Tile(Action::BottomRight))),
            ID_ALMOST => Some(TrayEvent::Window(Command::Tile(Action::Almost))),
            ID_MAXIMIZE => Some(TrayEvent::Window(Command::Tile(Action::Maximize))),
            ID_CENTER => Some(TrayEvent::Window(Command::Tile(Action::Center))),
            ID_RESTORE => Some(TrayEvent::Window(Command::Restore)),
            ID_NEXT_MON => Some(TrayEvent::Window(Command::NextMonitor)),
            ID_PREV_MON => Some(TrayEvent::Window(Command::PrevMonitor)),
            ID_OPEN_CONFIG => Some(TrayEvent::OpenConfig),
            ID_TOGGLE_STARTUP => Some(TrayEvent::ToggleStartup),
            ID_QUIT => Some(TrayEvent::Quit),
            _ => None,
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if !self.hook.is_invalid() {
            // SAFETY: `self.hook` was returned by `SetWinEventHook` on this thread.
            let _ = unsafe { UnhookWinEvent(self.hook) };
        }
        remove_tray_icon(self.hwnd);
        if !self.hwnd.is_invalid() {
            // SAFETY: `self.hwnd` was created by `CreateWindowExW` on this thread.
            let _ = unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

/// Parses a CLI subcommand (e.g. `"quit"`, `"config"`, `"left"`, `"top-right"`, `"next"`)
/// into its corresponding [`WM_APP_TRAY_CMD`] identifier.
#[must_use]
pub fn parse_cli_arg(arg: &str) -> Option<usize> {
    let clean = arg.trim().trim_start_matches('-').to_ascii_lowercase();
    match clean.as_str() {
        "left" | "l" => Some(ID_LEFT),
        "right" | "r" => Some(ID_RIGHT),
        "top" | "up" | "u" => Some(ID_TOP),
        "bottom" | "down" | "d" => Some(ID_BOTTOM),
        "top-left" | "topleft" | "tl" => Some(ID_TOP_LEFT),
        "top-right" | "topright" | "tr" => Some(ID_TOP_RIGHT),
        "bottom-left" | "bottomleft" | "bl" => Some(ID_BOTTOM_LEFT),
        "bottom-right" | "bottomright" | "br" => Some(ID_BOTTOM_RIGHT),
        "almost" | "a" => Some(ID_ALMOST),
        "maximize" | "max" | "m" => Some(ID_MAXIMIZE),
        "center" | "c" => Some(ID_CENTER),
        "restore" | "back" => Some(ID_RESTORE),
        "next-monitor" | "next" | "nm" => Some(ID_NEXT_MON),
        "prev-monitor" | "prev" | "pm" => Some(ID_PREV_MON),
        "config" | "cfg" => Some(ID_OPEN_CONFIG),
        "startup" | "autostart" => Some(ID_TOGGLE_STARTUP),
        "quit" | "exit" | "stop" | "q" => Some(ID_QUIT),
        _ => None,
    }
}

/// Sends a command ID to the already-running `orthowm` background instance via `PostMessageW`.
///
/// Returns `true` if the running tray window was found and the message was posted.
#[must_use]
pub fn send_ipc_command(cmd_id: usize) -> bool {
    // SAFETY: `TRAY_CLASS_NAME` is a valid null-terminated UTF-16 string literal.
    let Ok(hwnd) = (unsafe { FindWindowW(TRAY_CLASS_NAME, None) }) else {
        return false;
    };
    if hwnd.is_invalid() {
        return false;
    }
    // SAFETY: `hwnd` is a valid top-level window handle belonging to the running Ortho instance.
    unsafe { PostMessageW(Some(hwnd), WM_APP_TRAY_CMD, WPARAM(cmd_id), LPARAM(0)) }.is_ok()
}

/// Opens `config_path` in the user's default associated editor via `ShellExecuteW`.
pub fn open_config_in_editor(config_path: &Path) {
    let wide_path: Vec<u16> =
        OsStr::new(config_path).encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: `wide_path` is a null-terminated UTF-16 string and lives across the `ShellExecuteW` call.
    let _ = unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(wide_path.as_ptr()), None, None, SW_SHOWNORMAL)
    };
}

/// Returns `true` if Ortho is currently registered under `HKCU\...\CurrentVersion\Run`.
#[must_use]
pub fn is_startup_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run_key) = hkcu.open_subkey(RUN_REG_PATH) else {
        return false;
    };
    run_key.get_value::<String, _>(RUN_VALUE_NAME).is_ok_and(|val| !val.trim().is_empty())
}

/// Toggles the `"Ortho"` startup entry in `HKCU\...\CurrentVersion\Run` using native registry APIs.
pub fn toggle_startup() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok((run_key, _)) = hkcu.create_subkey(RUN_REG_PATH) else {
        return;
    };
    if is_startup_enabled() {
        let _ = run_key.delete_value(RUN_VALUE_NAME);
    } else if let Ok(exe_path) = std::env::current_exe() {
        let quoted = format!("\"{}\"", exe_path.display());
        let _ = run_key.set_value(RUN_VALUE_NAME, &quoted);
    }
}

/// Callback invoked by Windows whenever the foreground window changes across the desktop.
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if id_object == 0 && id_child == 0 && win::is_manageable_hwnd(hwnd) {
        LAST_ACTIVE_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    }
}

/// Window procedure for the hidden tray host window.
unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TRAY_CALLBACK {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let event = (lparam.0 as u32) & 0xFFFF;
        if event == WM_RBUTTONUP || event == WM_LBUTTONUP || event == WM_CONTEXTMENU {
            show_context_menu(hwnd);
        }
        return LRESULT(0);
    }
    let taskbar_created = TASKBAR_CREATED_MSG.load(Ordering::Relaxed);
    if taskbar_created != 0 && msg == taskbar_created {
        add_tray_icon(hwnd);
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        // SAFETY: Posts `WM_QUIT` to the current thread's message queue.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: Forwards unhandled window messages to the default Win32 window procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Builds and displays the native Win32 popup menu at the cursor position, posting any
/// selected command ID to the main message loop via [`WM_APP_TRAY_CMD`].
fn show_context_menu(hwnd: HWND) {
    // SAFETY: `CreatePopupMenu` allocates a new empty Win32 menu handle.
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    // SAFETY: `menu` is a valid `HMENU` and all labels are static null-terminated UTF-16 literals.
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, ID_LEFT, w!("Left Half\tCtrl+Alt+Left"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RIGHT, w!("Right Half\tCtrl+Alt+Right"));
        let _ = AppendMenuW(menu, MF_STRING, ID_TOP, w!("Top Half\tCtrl+Alt+Up"));
        let _ = AppendMenuW(menu, MF_STRING, ID_BOTTOM, w!("Bottom Half\tCtrl+Alt+Down"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    }
    // SAFETY: `CreatePopupMenu` creates a submenu whose ownership is transferred to `menu` via `MF_POPUP`.
    if let Ok(quarters) = unsafe { CreatePopupMenu() } {
        unsafe {
            let _ = AppendMenuW(quarters, MF_STRING, ID_TOP_LEFT, w!("Top Left\tCtrl+Alt+U"));
            let _ = AppendMenuW(quarters, MF_STRING, ID_TOP_RIGHT, w!("Top Right\tCtrl+Alt+I"));
            let _ = AppendMenuW(quarters, MF_STRING, ID_BOTTOM_LEFT, w!("Bottom Left\tCtrl+Alt+J"));
            let _ =
                AppendMenuW(quarters, MF_STRING, ID_BOTTOM_RIGHT, w!("Bottom Right\tCtrl+Alt+K"));
            let _ = AppendMenuW(menu, MF_POPUP, quarters.0 as usize, w!("Quarters"));
        }
    }
    let startup_flags =
        if is_startup_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING | MF_UNCHECKED };
    // SAFETY: `menu` is a valid `HMENU` and all labels are static null-terminated UTF-16 literals.
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ =
            AppendMenuW(menu, MF_STRING, ID_ALMOST, w!("Almost Maximize (85%)\tCtrl+Alt+Enter"));
        let _ = AppendMenuW(menu, MF_STRING, ID_MAXIMIZE, w!("Maximize\tCtrl+Alt+M"));
        let _ = AppendMenuW(menu, MF_STRING, ID_CENTER, w!("Center\tCtrl+Alt+C"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RESTORE, w!("Restore\tCtrl+Alt+Backspace"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_NEXT_MON, w!("Next Monitor\tCtrl+Alt+Win+Right"));
        let _ =
            AppendMenuW(menu, MF_STRING, ID_PREV_MON, w!("Previous Monitor\tCtrl+Alt+Win+Left"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_CONFIG, w!("Open Config..."));
        let _ = AppendMenuW(menu, startup_flags, ID_TOGGLE_STARTUP, w!("Launch at Login"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit Ortho"));
    }

    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid writable `POINT` on the stack.
    let _ = unsafe { GetCursorPos(&raw mut pt) };
    // SAFETY: `hwnd` is our valid hidden tray window; required by Win32 so the popup menu
    // dismisses when the user clicks outside it.
    let _ = unsafe { SetForegroundWindow(hwnd) };
    let flags = (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN).0;
    // SAFETY: `menu` and `hwnd` are valid handles.
    let selected = unsafe { TrackPopupMenuEx(menu, flags, pt.x, pt.y, hwnd, None) };
    // SAFETY: Standard Win32 `TrackPopupMenuEx` post-dismissal no-op message.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
    // SAFETY: `DestroyMenu` frees `menu` and its attached `quarters` submenu.
    let _ = unsafe { DestroyMenu(menu) };

    if selected.0 > 0
        && let Ok(cmd_id) = usize::try_from(selected.0)
    {
        // SAFETY: `hwnd` is a valid window handle on this thread.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_TRAY_CMD, WPARAM(cmd_id), LPARAM(0)) };
    }
}

/// Registers the tray icon with `Shell_NotifyIconW(NIM_ADD)`.
fn add_tray_icon(hwnd: HWND) {
    let icon = load_app_icon();
    #[expect(clippy::cast_possible_truncation)]
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY_CALLBACK,
        hIcon: icon,
        ..Default::default()
    };
    for (dst, src) in nid.szTip.iter_mut().zip("Ortho Window Manager".encode_utf16()) {
        *dst = src;
    }
    // SAFETY: `nid` is properly sized and initialized with a null-terminated `szTip` buffer.
    let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &raw const nid) };
}

/// Removes the tray icon with `Shell_NotifyIconW(NIM_DELETE)`.
fn remove_tray_icon(hwnd: HWND) {
    #[expect(clippy::cast_possible_truncation)]
    let nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        ..Default::default()
    };
    // SAFETY: `nid` identifies the registered tray icon via `(hWnd, uID)`.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const nid) };
}

/// Loads embedded icon resource `#1` (`MAKEINTRESOURCEW(1)`) from the executable,
/// falling back to `IDI_APPLICATION`.
fn load_app_icon() -> HICON {
    // SAFETY: `GetModuleHandleW(None)` retrieves the module handle of the current process.
    if let Ok(hmodule) = unsafe { GetModuleHandleW(None) } {
        let hinstance = HINSTANCE::from(hmodule);
        let resource_id = PCWSTR(APP_ICON_RESOURCE_ID as *const u16);
        // SAFETY: `MAKEINTRESOURCEW(1)` encodes an integer resource ID in the low 16 bits
        // as required by `LoadIconW`.
        if let Ok(icon) = unsafe { LoadIconW(Some(hinstance), resource_id) } {
            return icon;
        }
    }
    // SAFETY: `IDI_APPLICATION` is a standard system-shared icon resource identifier.
    unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default()
}

/// Opts native Win32 popup menus into Windows 10/11 Dark Mode via `uxtheme.dll` ordinals 135 & 136.
fn enable_dark_mode_menus() {
    type SetPreferredAppModeFn = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemesFn = unsafe extern "system" fn();

    // SAFETY: Loads `uxtheme.dll` strictly from `%SystemRoot%\System32`.
    let Ok(uxtheme) =
        (unsafe { LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) })
    else {
        return;
    };

    let ord_135 = PCSTR(UXTHEME_ORD_SET_PREFERRED_APP_MODE as *const u8);
    // SAFETY: Ordinal 135 in `uxtheme.dll` exports `SetPreferredAppMode(i32) -> i32` using the
    // `extern "system"` ABI on Windows 10 1903+ and Windows 11.
    if let Some(proc_135) = unsafe { GetProcAddress(uxtheme, ord_135) } {
        let set_mode: SetPreferredAppModeFn = unsafe {
            transmute::<unsafe extern "system" fn() -> isize, SetPreferredAppModeFn>(proc_135)
        };
        let _ = unsafe { set_mode(PREFERRED_APP_MODE_ALLOW_DARK) };
    }

    let ord_136 = PCSTR(UXTHEME_ORD_FLUSH_MENU_THEMES as *const u8);
    // SAFETY: Ordinal 136 in `uxtheme.dll` exports `FlushMenuThemes()` using the
    // `extern "system"` ABI on Windows 10 1903+ and Windows 11.
    if let Some(proc_136) = unsafe { GetProcAddress(uxtheme, ord_136) } {
        let flush: FlushMenuThemesFn = unsafe {
            transmute::<unsafe extern "system" fn() -> isize, FlushMenuThemesFn>(proc_136)
        };
        unsafe { flush() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_subcommands_map_to_valid_tray_events() {
        let cases = [
            ("left", TrayEvent::Window(Command::Tile(Action::Left))),
            ("--right", TrayEvent::Window(Command::Tile(Action::Right))),
            ("top-left", TrayEvent::Window(Command::Tile(Action::TopLeft))),
            ("br", TrayEvent::Window(Command::Tile(Action::BottomRight))),
            ("almost", TrayEvent::Window(Command::Tile(Action::Almost))),
            ("center", TrayEvent::Window(Command::Tile(Action::Center))),
            ("restore", TrayEvent::Window(Command::Restore)),
            ("next-monitor", TrayEvent::Window(Command::NextMonitor)),
            ("prev", TrayEvent::Window(Command::PrevMonitor)),
            ("config", TrayEvent::OpenConfig),
            ("startup", TrayEvent::ToggleStartup),
            ("quit", TrayEvent::Quit),
            ("--exit", TrayEvent::Quit),
        ];
        for (arg, expected) in cases {
            let id = parse_cli_arg(arg).unwrap_or_default();
            assert_eq!(Tray::decode_event(id), Some(expected));
        }
        assert!(parse_cli_arg("unknown").is_none());
    }
}
