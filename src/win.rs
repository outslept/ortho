//! Win32 window manipulation, DWM border compensation, multi-monitor geometry, and DPI scaling.

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_TRANSITIONS_FORCEDISABLED, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETAUTOHIDEBAREX, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsIconic,
    IsWindow, IsWindowVisible, IsZoomed, SW_RESTORE, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow,
    WS_CAPTION, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_THICKFRAME,
};
use windows::core::BOOL;

use crate::tile::{self, Action, Rect};

// Standard 100% scaling baseline DPI on Windows.
const DEFAULT_DPI: u32 = 96;

// Number of windows tracked in the stack-allocated restore ring buffer.
const HISTORY_CAP: usize = 16;

// Empty sentinel rect used for unoccupied slots in [`RestoreHistory`].
const EMPTY_RECT: Rect = Rect { x: 0, y: 0, width: 0, height: 0 };

// Fixed-capacity ring buffer storing pre-tiled floating window positions.
#[derive(Debug, Clone, Copy)]
pub struct RestoreHistory {
    entries: [(isize, Rect); HISTORY_CAP],
    cursor: usize,
}

impl Default for RestoreHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl RestoreHistory {
    // Creates an empty 16-slot restore history buffer on the stack.
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: [(0, EMPTY_RECT); HISTORY_CAP], cursor: 0 }
    }

    // Records the floating `rect` for window `key` (`HWND` cast to `isize`).
    //
    // Updates the existing entry in-place if `key` is already tracked; otherwise
    // writes into the oldest slot in the ring buffer.
    pub fn record(&mut self, key: isize, rect: Rect) {
        if key == 0 {
            return;
        }
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = rect;
            return;
        }
        if let Some(slot) = self.entries.get_mut(self.cursor) {
            *slot = (key, rect);
        }
        self.cursor = (self.cursor + 1) % HISTORY_CAP;
    }

    // Removes and returns the saved floating `Rect` for window `key`, if present.
    pub fn take(&mut self, key: isize) -> Option<Rect> {
        if key == 0 {
            return None;
        }
        let slot = self.entries.iter_mut().find(|(k, _)| *k == key)?;
        let rect = slot.1;
        *slot = (0, EMPTY_RECT);
        Some(rect)
    }
}

// Errors returned by Win32 window queries and geometry operations.
#[derive(Debug)]
pub enum WinError {
    // Failed to query monitor bounds via `GetMonitorInfoW`.
    MonitorInfo,
    // Failed to read raw window coordinates via `GetWindowRect`.
    WindowRect(windows::core::Error),
    // Failed to reposition the window via `SetWindowPos`.
    SetPos(windows::core::Error),
}

impl std::fmt::Display for WinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MonitorInfo => write!(f, "failed to get monitor info"),
            Self::WindowRect(e) => write!(f, "GetWindowRect failed: {e}"),
            Self::SetPos(e) => write!(f, "SetWindowPos failed: {e}"),
        }
    }
}

impl std::error::Error for WinError {}

// Enables Per-Monitor V2 DPI awareness for the current process.
//
// Must be called once at startup before any Win32 window or monitor queries so
// all coordinates are reported in physical pixels across mixed-DPI displays.
pub fn init_dpi() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

// Returns `true` if `hwnd` is currently a valid, user-resizable top-level application window.
#[must_use]
pub fn is_manageable_hwnd(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let Ok((_, full_area)) = monitor_rects(monitor) else {
        return false;
    };
    is_manageable(hwnd, full_area)
}

// Resolves the active target window: uses `GetForegroundWindow()` if manageable,
// or falls back to `fallback` (the last focused user window before opening the tray menu).
fn resolve_target_hwnd(fallback: Option<HWND>) -> Option<HWND> {
    let fg = unsafe { GetForegroundWindow() };
    if is_manageable_hwnd(fg) {
        return Some(fg);
    }
    let fb = fallback?;
    if is_manageable_hwnd(fb) {
        let _ = unsafe { SetForegroundWindow(fb) };
        Some(fb)
    } else {
        None
    }
}

// Applies a tiling `action` to the focused (or `fallback`) window using `logical_gap`
// (in 96-DPI pixels, scaled automatically to the target monitor's physical DPI).
//
// If the window is currently in a floating (non-maximized, non-snapped) state,
// its bounds are recorded in `history` before tiling.
pub fn apply_foreground(
    action: Action,
    logical_gap: i32,
    history: &mut RestoreHistory,
    fallback: Option<HWND>,
) -> Result<bool, WinError> {
    let Some(hwnd) = resolve_target_hwnd(fallback) else {
        return Ok(false);
    };

    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (work_area, _) = monitor_rects(monitor)?;

    let was_zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
    if was_zoomed {
        restore_without_animation(hwnd);
    }

    let mut current = visible_rect(hwnd)?;
    let physical_gap = scale_gap(monitor, logical_gap);

    if !was_zoomed && !tile::is_snapped(work_area, current, physical_gap) {
        history.record(hwnd.0 as isize, current);
    }

    if was_zoomed && action != Action::Center {
        // Ignore the arbitrary pre-maximized restore rect so Left/Right cycling
        // always starts deterministically at 1/2 width.
        current.width = 0;
    }

    let target = tile::apply(action, work_area, current, physical_gap);
    set_visible_rect(hwnd, target, true)?;
    Ok(true)
}

// Restores the focused (or `fallback`) window to its saved pre-tiled floating bounds in `history`.
//
// If no saved bounds exist for the window but it is currently maximized via Windows,
// unmaximizes it in 0ms.
pub fn restore_foreground(
    history: &mut RestoreHistory,
    fallback: Option<HWND>,
) -> Result<bool, WinError> {
    let Some(hwnd) = resolve_target_hwnd(fallback) else {
        return Ok(false);
    };

    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (work_area, _) = monitor_rects(monitor)?;

    let was_zoomed = unsafe { IsZoomed(hwnd) }.as_bool();

    if let Some(saved) = history.take(hwnd.0 as isize) {
        if was_zoomed {
            restore_without_animation(hwnd);
        }
        let target = clamp_to_work_area(saved, work_area);
        set_visible_rect(hwnd, target, true)?;
        return Ok(true);
    }

    if was_zoomed {
        restore_without_animation(hwnd);
        return Ok(true);
    }

    Ok(false)
}

// Moves the focused (or `fallback`) window to an adjacent monitor in spatial order
// (`step = -1` for previous monitor, `step = 1` for next monitor).
//
// Preserves the window's relative position and size proportionally to the destination
// monitor's work area, and uses a two-pass `SetWindowPos` when crossing monitors with
// different DPIs so Windows recalculates DWM frame borders before final placement.
pub fn move_foreground_monitor(step: isize, fallback: Option<HWND>) -> Result<bool, WinError> {
    let Some(hwnd) = resolve_target_hwnd(fallback) else {
        return Ok(false);
    };

    let monitors = enumerate_monitors();
    if monitors.len() <= 1 {
        return Ok(false);
    }

    let src_mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (src_work, _) = monitor_rects(src_mon)?;

    let Some(src_idx) = monitors.iter().position(|&(m, _)| m == src_mon) else {
        return Ok(false);
    };

    let len = monitors.len().cast_signed();
    let dst_idx = (src_idx.cast_signed() + step).rem_euclid(len).cast_unsigned();
    let Some(&(dst_mon, dst_work)) = monitors.get(dst_idx) else {
        return Ok(false);
    };

    if unsafe { IsZoomed(hwnd) }.as_bool() {
        restore_without_animation(hwnd);
    }

    let current = visible_rect(hwnd)?;
    let target = map_rect_between_screens(current, src_work, dst_work);

    if monitor_dpi(src_mon) != monitor_dpi(dst_mon) {
        // Pass 1 (synchronous): move window onto the destination monitor so Windows sends
        // WM_DPICHANGED and updates the window's DWM invisible border metrics immediately.
        set_visible_rect(hwnd, target, false)?;
    }

    // Pass 2 (asynchronous): apply final placement using the destination monitor's frame delta.
    set_visible_rect(hwnd, target, true)?;
    Ok(true)
}

// Clamps `rect` so that its dimensions and coordinates fit inside `work`.
fn clamp_to_work_area(rect: Rect, work: Rect) -> Rect {
    let width = rect.width.clamp(1, work.width.max(1));
    let height = rect.height.clamp(1, work.height.max(1));
    let x = rect.x.clamp(work.x, work.x + (work.width - width).max(0));
    let y = rect.y.clamp(work.y, work.y + (work.height - height).max(0));
    Rect { x, y, width, height }
}

// Maps a window rectangle `rect` proportionally from `src` work area to `dst` work area.
fn map_rect_between_screens(rect: Rect, src: Rect, dst: Rect) -> Rect {
    let src_w = i64::from(src.width.max(1));
    let src_h = i64::from(src.height.max(1));
    let dst_w = i64::from(dst.width.max(1));
    let dst_h = i64::from(dst.height.max(1));

    let rel_x = i64::from(rect.x - src.x);
    let rel_y = i64::from(rect.y - src.y);

    let new_x = i64::from(dst.x) + (rel_x * dst_w) / src_w;
    let new_y = i64::from(dst.y) + (rel_y * dst_h) / src_h;
    let new_w = ((i64::from(rect.width) * dst_w) / src_w).clamp(1, dst_w);
    let new_h = ((i64::from(rect.height) * dst_h) / src_h).clamp(1, dst_h);

    #[expect(clippy::cast_possible_truncation)]
    Rect {
        x: (new_x as i32).clamp(dst.x, dst.x + dst.width - (new_w as i32)),
        y: (new_y as i32).clamp(dst.y, dst.y + dst.height - (new_h as i32)),
        width: new_w as i32,
        height: new_h as i32,
    }
}

// Enumerates all connected display monitors sorted spatially from left to right, top to bottom.
fn enumerate_monitors() -> Vec<(HMONITOR, Rect)> {
    let mut list: Vec<(HMONITOR, Rect)> = Vec::with_capacity(4);
    let lparam = LPARAM((&raw mut list) as isize);
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(enum_monitors_proc), lparam) };
    list.sort_by_key(|&(_, work)| (work.x, work.y));
    list
}

// Win32 callback invoked by `EnumDisplayMonitors` for each active display.
unsafe extern "system" fn enum_monitors_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprc: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let list_ptr = lparam.0 as *mut Vec<(HMONITOR, Rect)>;
    if let Some(list) = unsafe { list_ptr.as_mut() }
        && let Ok((work, _)) = monitor_rects(hmonitor)
    {
        list.push((hmonitor, work));
    }
    BOOL(1)
}

// Returns the effective DPI of `monitor`, falling back to 96 DPI on failure.
fn monitor_dpi(monitor: HMONITOR) -> u32 {
    let mut dpi_x: u32 = DEFAULT_DPI;
    let mut dpi_y: u32 = DEFAULT_DPI;
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y) }
        .is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        DEFAULT_DPI
    }
}

// Scales `logical_gap` from 96-DPI logical pixels to the physical pixels of `monitor`.
fn scale_gap(monitor: HMONITOR, logical_gap: i32) -> i32 {
    let dpi = monitor_dpi(monitor).cast_signed();
    (logical_gap.max(0) * dpi + 48) / 96
}

// Returns `true` if `hwnd` is a standard user-resizable top-level application window.
fn is_manageable(hwnd: HWND, full_monitor: Rect) -> bool {
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool()
        || !unsafe { IsWindowVisible(hwnd) }.as_bool()
        || unsafe { IsIconic(hwnd) }.as_bool()
    {
        return false;
    }

    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) }.cast_unsigned();
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }.cast_unsigned();

    let disabled = (style & (WS_DISABLED.0 as usize)) != 0;
    let resizable = (style & (WS_THICKFRAME.0 as usize)) != 0;
    let has_caption = (style & (WS_CAPTION.0 as usize)) != 0;
    let tool_window = (ex_style & (WS_EX_TOOLWINDOW.0 as usize)) != 0;
    let no_activate = (ex_style & (WS_EX_NOACTIVATE.0 as usize)) != 0;

    if disabled || !resizable || tool_window || no_activate {
        return false;
    }

    // Do not disturb borderless fullscreen games or media players covering the display.
    if !has_caption
        && let Ok(rect) = raw_window_rect(hwnd)
        && rect == full_monitor
    {
        return false;
    }

    true
}

// Unmaximizes `hwnd` while suppressing the 200ms DWM restore animation for 0ms response.
fn restore_without_animation(hwnd: HWND) {
    let disable = BOOL(1);
    let enable = BOOL(0);
    #[expect(clippy::cast_possible_truncation)]
    let size = size_of::<BOOL>() as u32;

    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const disable).cast::<c_void>(),
            size,
        )
    };
    let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const enable).cast::<c_void>(),
            size,
        )
    };
}

// Queries the monitor's `(work_area, full_area)` in physical pixels, reserving a 1px margin
// on any edge that hosts an auto-hide taskbar when `work_area` equals `full_area`.
fn monitor_rects(monitor: HMONITOR) -> Result<(Rect, Rect), WinError> {
    #[expect(clippy::cast_possible_truncation)]
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };

    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(WinError::MonitorInfo);
    }

    let full = from_win_rect(info.rcMonitor);
    let mut work = from_win_rect(info.rcWork);

    if work == full {
        work = adjust_for_autohide(work, info.rcMonitor);
    }

    Ok((work, full))
}

// Shrinks `work` by 1px on edges where an auto-hide taskbar is registered on `monitor_rect`.
fn adjust_for_autohide(mut work: Rect, monitor_rect: RECT) -> Rect {
    let has_autohide = |edge: u32| -> bool {
        #[expect(clippy::cast_possible_truncation)]
        let mut data = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            uEdge: edge,
            rc: monitor_rect,
            ..Default::default()
        };
        unsafe { SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &raw mut data) != 0 }
    };

    if has_autohide(ABE_LEFT) {
        work.x += 1;
        work.width = (work.width - 1).max(1);
    }
    if has_autohide(ABE_TOP) {
        work.y += 1;
        work.height = (work.height - 1).max(1);
    }
    if has_autohide(ABE_RIGHT) {
        work.width = (work.width - 1).max(1);
    }
    if has_autohide(ABE_BOTTOM) {
        work.height = (work.height - 1).max(1);
    }
    work
}

// Returns the visible DWM frame bounds of `hwnd`, falling back to `GetWindowRect`.
fn visible_rect(hwnd: HWND) -> Result<Rect, WinError> {
    if let Some(dwm) = dwm_frame_rect(hwnd) { Ok(dwm) } else { raw_window_rect(hwnd) }
}

// Repositions `hwnd` so that its *visible* DWM frame matches `target` on screen.
fn set_visible_rect(hwnd: HWND, target: Rect, async_pos: bool) -> Result<(), WinError> {
    let raw = raw_window_rect(hwnd)?;
    let vis = dwm_frame_rect(hwnd).unwrap_or(raw);

    // Compensate for the invisible Win10/Win11 resize border overhang.
    let pad_left = vis.x - raw.x;
    let pad_top = vis.y - raw.y;
    let pad_width = raw.width - vis.width;
    let pad_height = raw.height - vis.height;

    let x = target.x - pad_left;
    let y = target.y - pad_top;
    let width = target.width + pad_width;
    let height = target.height + pad_height;

    let mut flags = SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED;
    if async_pos {
        flags |= SWP_ASYNCWINDOWPOS;
    }

    unsafe { SetWindowPos(hwnd, None, x, y, width, height, flags) }.map_err(WinError::SetPos)
}

// Queries `GetWindowRect` (which includes invisible DWM resize borders).
fn raw_window_rect(hwnd: HWND) -> Result<Rect, WinError> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &raw mut r) }.map_err(WinError::WindowRect)?;
    Ok(from_win_rect(r))
}

// Queries `DWMWA_EXTENDED_FRAME_BOUNDS` (the visible window bounds excluding invisible borders).
fn dwm_frame_rect(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    #[expect(clippy::cast_possible_truncation)]
    let size = size_of::<RECT>() as u32;

    let res = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast::<c_void>(),
            size,
        )
    };
    res.is_ok().then(|| from_win_rect(r))
}

// Converts a Win32 `RECT` (`left, top, right, bottom`) to `tile::Rect` (`x, y, width, height`).
const fn from_win_rect(r: RECT) -> Rect {
    Rect { x: r.left, y: r.top, width: r.right - r.left, height: r.bottom - r.top }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_history_records_updates_and_takes() {
        let mut hist = RestoreHistory::new();
        let r1 = Rect { x: 100, y: 120, width: 800, height: 600 };
        let r2 = Rect { x: 200, y: 220, width: 900, height: 700 };

        hist.record(42, r1);
        hist.record(42, r2); // In-place update for same HWND key

        assert_eq!(hist.take(42), Some(r2));
        assert_eq!(hist.take(42), None);
    }

    #[test]
    fn restore_history_evicts_oldest_after_16_entries() {
        let mut hist = RestoreHistory::new();
        for i in 1..=17_isize {
            #[expect(clippy::cast_possible_truncation)]
            let r = Rect { x: i as i32, y: 0, width: 500, height: 400 };
            hist.record(i, r);
        }

        // Key 1 was overwritten by key 17 in the 16-slot ring buffer
        assert_eq!(hist.take(1), None);
        assert_eq!(hist.take(2), Some(Rect { x: 2, y: 0, width: 500, height: 400 }));
        assert_eq!(hist.take(17), Some(Rect { x: 17, y: 0, width: 500, height: 400 }));
    }

    #[test]
    fn clamp_to_work_area_keeps_restored_window_on_screen() {
        let work = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        let offscreen = Rect { x: 1600, y: 900, width: 800, height: 600 };

        let clamped = clamp_to_work_area(offscreen, work);
        assert_eq!(clamped, Rect { x: 1120, y: 480, width: 800, height: 600 });
    }
}
