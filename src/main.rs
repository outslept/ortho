pub mod config;
pub mod hotkey;
pub mod tile;
pub mod win;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage, WM_HOTKEY,
};
use windows::core::w;

use crate::config::ConfigManager;
use crate::hotkey::{Command, HotkeyRegistry};
use crate::win::RestoreHistory;

// Guard that holds the single-instance OS mutex for the lifetime of the process.
struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

// Acquires a session-local named mutex so launching `orthowm` twice exits cleanly
// instead of competing for global hotkeys.
fn acquire_single_instance() -> Option<SingleInstance> {
    let handle = unsafe { CreateMutexW(None, false, w!("Local\\OrthoWindowManagerMutex")) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(handle) };
        return None;
    }
    Some(SingleInstance(handle))
}

fn main() {
    let Some(_instance) = acquire_single_instance() else {
        return;
    };

    win::init_dpi();

    let mut config_mgr = ConfigManager::load();
    let mut hotkeys = HotkeyRegistry::register(&config_mgr.config().hotkeys);
    let mut history = RestoreHistory::new();

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == WM_HOTKEY {
            if config_mgr.reload_if_changed() {
                hotkeys.rebind(&config_mgr.config().hotkeys);
            }

            #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let hotkey_id = msg.wParam.0 as i32;

            if let Some(cmd) = hotkeys.command_for_id(hotkey_id) {
                dispatch_command(cmd, config_mgr.config().gap, &mut history);
            }
        }

        let _ = unsafe { TranslateMessage(&raw const msg) };
        let _ = unsafe { DispatchMessageW(&raw const msg) };
    }
}

// Executes a tiling, restore, or monitor-hop command on the focused window.
fn dispatch_command(cmd: Command, gap: i32, history: &mut RestoreHistory) {
    let _ = match cmd {
        Command::Tile(action) => win::apply_foreground(action, gap, history),
        Command::Restore => win::restore_foreground(history),
        Command::PrevMonitor => win::move_foreground_monitor(-1),
        Command::NextMonitor => win::move_foreground_monitor(1),
    };
}
