//! Ortho (`orthowm`) binary entrypoint, CLI IPC client, single-instance guard, and Win32 message loop.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod config;
pub mod hotkey;
pub mod tile;
pub mod tray;
pub mod win;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage, WM_HOTKEY,
};
use windows::core::w;

use crate::config::ConfigManager;
use crate::hotkey::{Command, HotkeyRegistry};
use crate::tray::{Tray, TrayEvent, WM_APP_TRAY_CMD};
use crate::win::RestoreHistory;

/// Guard that holds the single-instance OS mutex for the lifetime of the process.
struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: `self.0` is a valid mutex handle created by `CreateMutexW`.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Acquires a session-local named mutex so launching `orthowm` twice exits cleanly
/// instead of competing for global hotkeys.
fn acquire_single_instance() -> Option<SingleInstance> {
    // SAFETY: Creates or opens a session-local named Win32 mutex with default security attributes.
    let handle = unsafe { CreateMutexW(None, false, w!("Local\\OrthoWindowManagerMutex")) }.ok()?;
    // SAFETY: `GetLastError` reads the calling thread's last-error code set by `CreateMutexW`.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: `handle` is valid and must be closed before exiting the duplicate instance.
        let _ = unsafe { CloseHandle(handle) };
        return None;
    }
    Some(SingleInstance(handle))
}

fn main() {
    if let Some(arg) = std::env::args().nth(1) {
        if let Some(cmd_id) = tray::parse_cli_arg(&arg) {
            let _ = tray::send_ipc_command(cmd_id);
        }
        return;
    }
    let Some(_instance) = acquire_single_instance() else {
        return;
    };
    win::init_dpi();
    let mut config_mgr = ConfigManager::load();
    let mut hotkeys = HotkeyRegistry::register(&config_mgr.config().hotkeys);
    let mut history = RestoreHistory::new();
    let _tray = Tray::new();
    let mut msg = MSG::default();

    // SAFETY: `msg` is a valid writable `MSG` on the stack, and `None` retrieves messages
    // for any window or thread message belonging to the calling thread.
    while unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == WM_HOTKEY {
            if config_mgr.reload_if_changed() {
                hotkeys.rebind(&config_mgr.config().hotkeys);
            }
            if let Ok(hotkey_id) = i32::try_from(msg.wParam.0)
                && let Some(cmd) = hotkeys.command_for_id(hotkey_id)
            {
                dispatch_command(cmd, config_mgr.config().gap, &mut history, None);
            }
        } else if msg.message == WM_APP_TRAY_CMD {
            if config_mgr.reload_if_changed() {
                hotkeys.rebind(&config_mgr.config().hotkeys);
            }
            if let Some(event) = Tray::decode_event(msg.wParam.0) {
                match event {
                    TrayEvent::Window(cmd) => {
                        let target = Tray::last_active_window();
                        dispatch_command(cmd, config_mgr.config().gap, &mut history, target);
                    }
                    TrayEvent::OpenConfig => {
                        tray::open_config_in_editor(config_mgr.path());
                    }
                    TrayEvent::ToggleStartup => {
                        tray::toggle_startup();
                    }
                    TrayEvent::Quit => {
                        // SAFETY: Posts `WM_QUIT` to terminate the calling thread's message loop.
                        unsafe { PostQuitMessage(0) };
                    }
                }
            }
        }
        // SAFETY: `msg` was populated by `GetMessageW` on the current thread.
        let _ = unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: Dispatches `msg` to the target window procedure on the current thread.
        let _ = unsafe { DispatchMessageW(&raw const msg) };
    }
}

/// Executes a tiling, restore, or monitor-hop command on the focused (or `fallback`) window.
fn dispatch_command(cmd: Command, gap: i32, history: &mut RestoreHistory, fallback: Option<HWND>) {
    let _ = match cmd {
        Command::Tile(action) => win::apply_foreground(action, gap, history, fallback),
        Command::Restore => win::restore_foreground(history, fallback),
        Command::PrevMonitor => win::move_foreground_monitor(-1, gap, fallback),
        Command::NextMonitor => win::move_foreground_monitor(1, gap, fallback),
    };
}
