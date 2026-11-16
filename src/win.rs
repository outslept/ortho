use std::ffi::c_void;
use std::mem::size_of;
use thiserror::Error;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_TRANSITIONS_FORCEDISABLED, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongW, GetWindowRect, IsIconic,
    IsWindowVisible, IsZoomed, SW_RESTORE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CAPTION, WS_DISABLED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_THICKFRAME,
};
use windows::core::BOOL;

use crate::tile::{self, Action, Rect};

#[derive(Debug, Error)]
pub enum WinError {
    #[error("OS call failed: {0}")]
    Os(#[from] windows::core::Error),
    #[error("Failed to query monitor bounds")]
    MonitorInfo,
}

/// Enables `Per-Monitor V2` DPI awareness so all `Win32` APIs operate in physical
/// screen pixels rather than DPI-virtualized coordinates.
pub fn init_dpi() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Tiles the currently focused window according to `action` and physical pixel `gap`.
///
/// Returns `Ok(false)` if no window is focused or if the focused window is filtered
/// out by [`is_manageable`].
///
/// # Errors
/// Returns [`WinError`] if querying monitor or window coordinates fails.
pub fn apply_foreground(action: Action, gap: i32) -> Result<bool, WinError> {
    let hwnd = unsafe { GetForegroundWindow() };
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (work_area, full_area) = monitor_rects(monitor)?;

    if !is_manageable(hwnd, full_area) {
        return Ok(false);
    }

    let was_maximized = unsafe { IsZoomed(hwnd) }.as_bool();
    if was_maximized {
        restore_without_animation(hwnd);
    }

    let mut current = visible_rect(hwnd)?;
    if was_maximized && action != Action::Center {
        // Prevent a window that was in a 1/2 slot prior to maximization from
        // immediately cycling to 2/3 when unmaximized via Left/Right.
        current.width = 0;
    }

    let target = tile::apply(action, work_area, current, gap);
    set_visible_rect(hwnd, target)?;

    Ok(true)
}

// Rejects desktop/taskbar windows, non-resizable dialogs, disabled parent windows
// behind modal dialogs, floating palettes, and borderless fullscreen games.
#[must_use]
pub fn is_manageable(hwnd: HWND, full_monitor: Rect) -> bool {
    if hwnd.is_invalid() {
        return false;
    }

    let active = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() };
    if !active {
        return false;
    }

    let style = WINDOW_STYLE(unsafe { GetWindowLongW(hwnd, GWL_STYLE) }.cast_unsigned());
    let ex_style = WINDOW_EX_STYLE(unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }.cast_unsigned());

    // `WS_THICKFRAME` is required for resizing and implicitly filters out `Progman` (desktop)
    // and `Shell_TrayWnd` (taskbar) without string allocations for class names.
    if !style.contains(WS_THICKFRAME)
        || style.contains(WS_DISABLED)
        || ex_style.contains(WS_EX_TOOLWINDOW)
        || ex_style.contains(WS_EX_NOACTIVATE)
    {
        return false;
    }

    // Borderless fullscreen games/videos strip `WS_CAPTION` and cover the entire monitor.
    if !style.contains(WS_CAPTION)
        && let Ok(outer) = outer_rect(hwnd)
        && outer == full_monitor
    {
        return false;
    }

    true
}

// Unmaximizes `hwnd` while temporarily disabling DWM transitions so the state change
// takes 0ms instead of playing the 200ms Windows restore animation.
fn restore_without_animation(hwnd: HWND) {
    set_dwm_transitions(hwnd, true);
    let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
    set_dwm_transitions(hwnd, false);
}

fn set_dwm_transitions(hwnd: HWND, force_disabled: bool) {
    let val = BOOL::from(force_disabled);
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const val).cast::<c_void>(),
            u32::try_from(size_of::<BOOL>()).unwrap_or(4),
        )
    };
}

// Queries the visible window bounds (excluding the invisible ~7px Win10/11 resize shadow).
fn visible_rect(hwnd: HWND) -> Result<Rect, WinError> {
    let mut rc = RECT::default();
    let res = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rc).cast::<c_void>(),
            u32::try_from(size_of::<RECT>()).unwrap_or(16),
        )
    };
    if res.is_ok() {
        return Ok(from_win_rect(rc));
    }
    outer_rect(hwnd)
}

// Queries the raw `GetWindowRect` bounds (which include invisible DWM resize borders).
fn outer_rect(hwnd: HWND) -> Result<Rect, WinError> {
    let mut rc = RECT::default();
    unsafe { GetWindowRect(hwnd, &raw mut rc)? };
    Ok(from_win_rect(rc))
}

// Moves and resizes `hwnd` so its *visible* DWM frame aligns with `target` to the pixel.
fn set_visible_rect(hwnd: HWND, target: Rect) -> Result<(), WinError> {
    let outer = outer_rect(hwnd)?;
    let visible = visible_rect(hwnd)?;

    let pad_left = visible.x - outer.x;
    let pad_top = visible.y - outer.y;
    let pad_right = (outer.x + outer.width) - (visible.x + visible.width);
    let pad_bottom = (outer.y + outer.height) - (visible.y + visible.height);

    unsafe {
        SetWindowPos(
            hwnd,
            None,
            target.x - pad_left,
            target.y - pad_top,
            target.width + pad_left + pad_right,
            target.height + pad_top + pad_bottom,
            SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOZORDER,
        )?;
    }
    Ok(())
}

// Returns `(work_area, full_monitor_area)` in physical pixels for `monitor`.
fn monitor_rects(monitor: HMONITOR) -> Result<(Rect, Rect), WinError> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(40),
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(WinError::MonitorInfo);
    }
    Ok((from_win_rect(info.rcWork), from_win_rect(info.rcMonitor)))
}

const fn from_win_rect(rc: RECT) -> Rect {
    Rect { x: rc.left, y: rc.top, width: rc.right - rc.left, height: rc.bottom - rc.top }
}
