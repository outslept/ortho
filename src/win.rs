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
    GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsHungAppWindow,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, SW_RESTORE, SWP_ASYNCWINDOWPOS,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SetForegroundWindow, SetProcessDPIAware, SetWindowPos, ShowWindow, WS_CAPTION, WS_DISABLED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_THICKFRAME,
};
use windows::core::BOOL;

use crate::tile::{self, Action, Rect};

/// Standard 100% scaling baseline DPI on Windows.
const DEFAULT_DPI: u32 = 96;

/// Half of [`DEFAULT_DPI`], added before integer division for nearest-integer DPI rounding.
const HALF_DEFAULT_DPI: i32 = 48;

/// Number of windows tracked in the stack-allocated restore ring buffer.
const HISTORY_CAP: usize = 16;

/// Saved pre-tiled floating bounds and last commanded tile target for a tracked window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryEntry {
    key: isize,
    floating_rect: Rect,
    last_target: Option<Rect>,
}

/// Fixed-capacity ring buffer storing pre-tiled floating window positions.
#[derive(Debug, Clone, Copy)]
pub struct RestoreHistory {
    entries: [Option<HistoryEntry>; HISTORY_CAP],
    cursor: usize,
}

impl Default for RestoreHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl RestoreHistory {
    /// Creates an empty 16-slot restore history buffer on the stack.
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: [None; HISTORY_CAP], cursor: 0 }
    }

    /// Returns `true` if `key` currently has a saved floating rectangle in history.
    #[must_use]
    pub fn has_entry(&self, key: isize) -> bool {
        key != 0 && self.entries.iter().flatten().any(|e| e.key == key)
    }

    /// Returns `true` if `current` matches the last target rectangle assigned to `key` by Ortho.
    #[must_use]
    pub fn is_tracked_at(&self, key: isize, current: Rect) -> bool {
        if key == 0 {
            return false;
        }
        self.entries
            .iter()
            .flatten()
            .find(|e| e.key == key)
            .and_then(|e| e.last_target)
            .is_some_and(|target| current.approx_eq(target))
    }

    /// Records the pre-tiled `floating_rect` for window `key` (`HWND` cast to `isize`).
    ///
    /// Updates the existing entry in-place if `key` is already tracked, reuses an empty slot
    /// if one is available, or overwrites the oldest slot in the ring buffer.
    pub fn record(&mut self, key: isize, floating_rect: Rect) {
        if key == 0 {
            return;
        }
        if let Some(entry) = self.entries.iter_mut().flatten().find(|e| e.key == key) {
            entry.floating_rect = floating_rect;
            return;
        }
        let new_entry = Some(HistoryEntry { key, floating_rect, last_target: None });
        if let Some(empty_slot) = self.entries.iter_mut().find(|slot| slot.is_none()) {
            *empty_slot = new_entry;
            return;
        }
        if let Some(slot) = self.entries.get_mut(self.cursor) {
            *slot = new_entry;
        }
        self.cursor = (self.cursor + 1) % HISTORY_CAP;
    }

    /// Updates the last commanded tile `target` for an existing window `key`.
    pub fn set_last_target(&mut self, key: isize, target: Rect) {
        if key == 0 {
            return;
        }
        if let Some(entry) = self.entries.iter_mut().flatten().find(|e| e.key == key) {
            entry.last_target = Some(target);
        }
    }

    /// Removes and returns the saved floating [`Rect`] for window `key`, if present.
    pub fn take(&mut self, key: isize) -> Option<Rect> {
        if key == 0 {
            return None;
        }
        let slot = self.entries.iter_mut().find(|slot| slot.is_some_and(|e| e.key == key))?;
        slot.take().map(|e| e.floating_rect)
    }
}

/// RAII guard that suppresses DWM window animations for the duration of a restore/reposition sequence.
struct DwmTransitionGuard(HWND);

impl DwmTransitionGuard {
    fn suppress(hwnd: HWND) -> Self {
        let disable = BOOL(1);
        #[expect(clippy::cast_possible_truncation)]
        let size = size_of::<BOOL>() as u32;
        // SAFETY: `disable` is a valid `BOOL` matching `DWMWA_TRANSITIONS_FORCEDISABLED`.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&raw const disable).cast::<c_void>(),
                size,
            )
        };
        Self(hwnd)
    }
}

impl Drop for DwmTransitionGuard {
    fn drop(&mut self) {
        let enable = BOOL(0);
        #[expect(clippy::cast_possible_truncation)]
        let size = size_of::<BOOL>() as u32;
        // SAFETY: `enable` is a valid `BOOL` restoring default DWM transition behavior for `self.0`.
        let _ = unsafe {
            DwmSetWindowAttribute(
                self.0,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&raw const enable).cast::<c_void>(),
                size,
            )
        };
    }
}

/// Errors returned by Win32 window queries and geometry operations.
#[derive(Debug)]
pub enum WinError {
    /// Failed to query monitor bounds via `GetMonitorInfoW`.
    MonitorInfo,
    /// Failed to read raw window coordinates via `GetWindowRect`.
    WindowRect(windows::core::Error),
    /// Failed to reposition the window via `SetWindowPos`.
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

/// Enables Per-Monitor V2 DPI awareness for the current process, falling back to system DPI
/// awareness on older Windows builds if needed.
///
/// Must be called once at startup before any Win32 window or monitor queries so
/// all coordinates are reported in physical pixels across mixed-DPI displays.
pub fn init_dpi() {
    // SAFETY: `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` is a valid system constant
    // and calling `SetProcessDpiAwarenessContext` at startup before window creation is sound.
    if unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_err()
    {
        // SAFETY: Fallback for older builds where Per-Monitor V2 context is unavailable.
        let _ = unsafe { SetProcessDPIAware() };
    }
}

/// Returns `true` if `hwnd` is currently a valid, user-resizable top-level application window.
#[must_use]
pub fn is_manageable_hwnd(hwnd: HWND) -> bool {
    // SAFETY: `IsWindow`, `IsWindowVisible`, and `IsIconic` safely inspect any `HWND`.
    if hwnd.is_invalid()
        || !unsafe { IsWindow(Some(hwnd)) }.as_bool()
        || !unsafe { IsWindowVisible(hwnd) }.as_bool()
        || unsafe { IsIconic(hwnd) }.as_bool()
    {
        return false;
    }

    // SAFETY: `hwnd` was verified to be a valid window via `IsWindow`.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) }.cast_unsigned();
    // SAFETY: `hwnd` was verified to be a valid window via `IsWindow`.
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }.cast_unsigned();

    let disabled = (style & (WS_DISABLED.0 as usize)) != 0;
    let resizable = (style & (WS_THICKFRAME.0 as usize)) != 0;
    let has_caption = (style & (WS_CAPTION.0 as usize)) == (WS_CAPTION.0 as usize);
    let tool_window = (ex_style & (WS_EX_TOOLWINDOW.0 as usize)) != 0;
    let no_activate = (ex_style & (WS_EX_NOACTIVATE.0 as usize)) != 0;

    if disabled || !resizable || tool_window || no_activate {
        return false;
    }

    // Do not disturb borderless fullscreen games or media players covering the display.
    if !has_caption {
        // SAFETY: `MonitorFromWindow` accepts any valid `HWND`.
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if let Ok(full_monitor) = monitor_full_rect(monitor)
            && let Ok(rect) = raw_window_rect(hwnd)
            && rect == full_monitor
        {
            return false;
        }
    }

    true
}

/// Resolves the active target window: uses `GetForegroundWindow()` if manageable,
/// or falls back to `fallback` (the last focused user window before opening the tray menu).
fn resolve_target_hwnd(fallback: Option<HWND>) -> Option<HWND> {
    // SAFETY: `GetForegroundWindow` takes no parameters and returns the current foreground `HWND` or null.
    let fg = unsafe { GetForegroundWindow() };
    if is_manageable_hwnd(fg) {
        return Some(fg);
    }
    let fb = fallback?;
    if is_manageable_hwnd(fb) {
        // SAFETY: `fb` was just verified to be a valid, visible top-level window.
        let _ = unsafe { SetForegroundWindow(fb) };
        Some(fb)
    } else {
        None
    }
}

/// Applies a tiling `action` to the focused (or `fallback`) window using `logical_gap`
/// (in 96-DPI pixels, scaled automatically to the target monitor's physical DPI).
///
/// If the window is currently in a floating (non-snapped, non-tracked) state,
/// its bounds are recorded in `history` before tiling.
///
/// # Errors
///
/// Returns [`WinError`] if querying monitor info, reading window bounds, or repositioning fails.
pub fn apply_foreground(
    action: Action,
    logical_gap: i32,
    history: &mut RestoreHistory,
    fallback: Option<HWND>,
) -> Result<bool, WinError> {
    let Some(hwnd) = resolve_target_hwnd(fallback) else {
        return Ok(false);
    };
    let key = hwnd.0 as isize;
    // SAFETY: `hwnd` is a valid window handle resolved by `resolve_target_hwnd`.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (work_area, _) = monitor_rects(monitor)?;
    let physical_gap = scale_gap(monitor, logical_gap);

    // SAFETY: `hwnd` is a valid window handle.
    let was_zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
    let _no_anim = was_zoomed.then(|| DwmTransitionGuard::suppress(hwnd));
    if was_zoomed {
        unmaximize_and_sync_frame(hwnd);
    }

    let mut current = visible_rect(hwnd)?;
    if was_zoomed {
        if !history.has_entry(key) {
            history.record(key, current);
        }
        if action != Action::Center {
            // Start Left/Right/Top/Bottom fraction cycling deterministically at 1/2 size.
            current.width = 0;
        }
    } else if !history.is_tracked_at(key, current)
        && !tile::is_snapped(work_area, current, physical_gap)
    {
        history.record(key, current);
    }

    let target = tile::apply(action, work_area, current, physical_gap);
    set_visible_rect(hwnd, target)?;
    history.set_last_target(key, target);
    Ok(true)
}

/// Restores the focused (or `fallback`) window to its saved pre-tiled floating bounds in `history`.
///
/// If no saved bounds exist for the window but it is currently maximized via Windows,
/// unmaximizes it in 0ms.
///
/// # Errors
///
/// Returns [`WinError`] if querying monitor info or repositioning the window fails.
pub fn restore_foreground(
    history: &mut RestoreHistory,
    fallback: Option<HWND>,
) -> Result<bool, WinError> {
    let Some(hwnd) = resolve_target_hwnd(fallback) else {
        return Ok(false);
    };
    // SAFETY: `hwnd` is a valid window handle resolved by `resolve_target_hwnd`.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (work_area, _) = monitor_rects(monitor)?;
    // SAFETY: `hwnd` is a valid window handle.
    let was_zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
    let _no_anim = was_zoomed.then(|| DwmTransitionGuard::suppress(hwnd));

    if let Some(saved) = history.take(hwnd.0 as isize) {
        if was_zoomed {
            unmaximize_and_sync_frame(hwnd);
        }
        let target = tile::clamp_to_work_area(saved, work_area);
        set_visible_rect(hwnd, target)?;
        return Ok(true);
    }

    if was_zoomed {
        unmaximize_and_sync_frame(hwnd);
        return Ok(true);
    }

    Ok(false)
}

/// Moves the focused (or `fallback`) window to an adjacent monitor in spatial order
/// (`step = -1` for previous monitor, `step = 1` for next monitor).
///
/// Preserves the window's canonical snap tile or relative floating bounds on the destination
/// monitor's work area, and uses a two-pass `SetWindowPos` when crossing monitors with
/// different DPIs so Windows recalculates DWM frame borders before final placement.
///
/// # Errors
///
/// Returns [`WinError`] if querying monitor info, reading window bounds, or repositioning fails.
pub fn move_foreground_monitor(
    step: isize,
    logical_gap: i32,
    history: &mut RestoreHistory,
    fallback: Option<HWND>,
) -> Result<bool, WinError> {
    let Some(hwnd) = resolve_target_hwnd(fallback) else {
        return Ok(false);
    };
    let monitors = enumerate_monitors();
    if monitors.len() <= 1 {
        return Ok(false);
    }
    // SAFETY: `hwnd` is a valid window handle resolved by `resolve_target_hwnd`.
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

    let key = hwnd.0 as isize;
    let src_gap = scale_gap(src_mon, logical_gap);
    let dst_gap = scale_gap(dst_mon, logical_gap);

    // SAFETY: `hwnd` is a valid window handle.
    let was_zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
    let _no_anim = was_zoomed.then(|| DwmTransitionGuard::suppress(hwnd));

    let target = if was_zoomed {
        unmaximize_and_sync_frame(hwnd);
        let restored = visible_rect(hwnd)?;
        if !history.has_entry(key) {
            history.record(key, restored);
        }
        tile::apply(Action::Maximize, dst_work, restored, dst_gap)
    } else {
        let current = visible_rect(hwnd)?;
        if !history.is_tracked_at(key, current) && !tile::is_snapped(src_work, current, src_gap) {
            history.record(key, current);
        }
        tile::remap_to_screen(current, src_work, src_gap, dst_work, dst_gap)
    };

    if monitor_dpi(src_mon) != monitor_dpi(dst_mon) {
        // Pass 1: move window onto the destination monitor so Windows sends WM_DPICHANGED
        // and updates the window's DWM invisible border metrics immediately.
        set_visible_rect(hwnd, target)?;
    }
    // Pass 2: apply final placement using the destination monitor's frame delta.
    set_visible_rect(hwnd, target)?;
    history.set_last_target(key, target);
    Ok(true)
}

/// Enumerates all connected display monitors sorted spatially from left to right, top to bottom.
fn enumerate_monitors() -> Vec<(HMONITOR, Rect)> {
    let mut list: Vec<(HMONITOR, Rect)> = Vec::with_capacity(4);
    let lparam = LPARAM((&raw mut list) as isize);
    // SAFETY: `EnumDisplayMonitors` invokes `enum_monitors_proc` synchronously on the
    // current thread before returning, so `list` outlives all callback invocations and is
    // never accessed concurrently.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(enum_monitors_proc), lparam) };
    list.sort_by_key(|&(_, work)| (work.x, work.y));
    list
}

/// Win32 callback invoked synchronously by `EnumDisplayMonitors` for each active display.
unsafe extern "system" fn enum_monitors_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprc: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let list_ptr = lparam.0 as *mut Vec<(HMONITOR, Rect)>;
    // SAFETY: `lparam` was constructed from `&raw mut list` in `enumerate_monitors`, and
    // `EnumDisplayMonitors` invokes this callback sequentially on the same thread.
    if let Some(list) = unsafe { list_ptr.as_mut() }
        && let Ok((work, _)) = monitor_rects(hmonitor)
    {
        list.push((hmonitor, work));
    }
    BOOL(1)
}

/// Returns the effective DPI of `monitor`, falling back to 96 DPI on failure.
fn monitor_dpi(monitor: HMONITOR) -> u32 {
    let mut dpi_x: u32 = DEFAULT_DPI;
    let mut dpi_y: u32 = DEFAULT_DPI;
    // SAFETY: `dpi_x` and `dpi_y` are valid writable `u32` stack pointers.
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y) }
        .is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        DEFAULT_DPI
    }
}

/// Scales `logical_gap` from 96-DPI logical pixels to the physical pixels of `monitor`.
fn scale_gap(monitor: HMONITOR, logical_gap: i32) -> i32 {
    let dpi = monitor_dpi(monitor).cast_signed();
    (logical_gap.max(0) * dpi + HALF_DEFAULT_DPI) / DEFAULT_DPI.cast_signed()
}

/// Unmaximizes `hwnd` and forces a synchronous non-client frame update so subsequent
/// `DWMWA_EXTENDED_FRAME_BOUNDS` queries read the restored frame padding rather than stale
/// maximized metrics.
fn unmaximize_and_sync_frame(hwnd: HWND) {
    // SAFETY: `hwnd` is a valid window handle.
    let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
    // SAFETY: Triggers `WM_NCCALCSIZE` / frame recalculation without moving or activating `hwnd`.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOOWNERZORDER
                | SWP_NOACTIVATE
                | SWP_FRAMECHANGED,
        )
    };
}

/// Queries only the full physical bounds (`rcMonitor`) of `monitor` without appbar IPC calls.
fn monitor_full_rect(monitor: HMONITOR) -> Result<Rect, WinError> {
    #[expect(clippy::cast_possible_truncation)]
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info.cbSize` is initialized to the exact size of `MONITORINFO`.
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(WinError::MonitorInfo);
    }
    Ok(from_win_rect(info.rcMonitor))
}

/// Queries the monitor's `(work_area, full_area)` in physical pixels, reserving a 1px margin
/// on any edge that hosts an auto-hide taskbar when `work_area` equals `full_area`.
fn monitor_rects(monitor: HMONITOR) -> Result<(Rect, Rect), WinError> {
    #[expect(clippy::cast_possible_truncation)]
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info.cbSize` is initialized to the exact size of `MONITORINFO`.
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

/// Shrinks `work` by 1px on edges where an auto-hide taskbar is registered on `monitor_rect`.
///
/// Windows requires at least 1px of uncovered screen edge for an auto-hide taskbar to receive
/// mouse hover events when a window is tiled flush to the work area.
fn adjust_for_autohide(mut work: Rect, monitor_rect: RECT) -> Rect {
    let has_autohide = |edge: u32| -> bool {
        #[expect(clippy::cast_possible_truncation)]
        let mut data = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            uEdge: edge,
            rc: monitor_rect,
            ..Default::default()
        };
        // SAFETY: `data.cbSize` is initialized to `size_of::<APPBARDATA>()`.
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

/// Returns the visible DWM frame bounds of `hwnd`, falling back to `GetWindowRect`.
fn visible_rect(hwnd: HWND) -> Result<Rect, WinError> {
    if let Some(dwm) = dwm_frame_rect(hwnd) { Ok(dwm) } else { raw_window_rect(hwnd) }
}

/// Repositions `hwnd` so that its *visible* DWM frame matches `target` on screen.
///
/// Uses synchronous placement for responsive windows so rapid hotkey cycling immediately sees
/// the updated geometry, and automatically switches to `SWP_ASYNCWINDOWPOS` if `hwnd` is hung.
fn set_visible_rect(hwnd: HWND, target: Rect) -> Result<(), WinError> {
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
    // SAFETY: `IsHungAppWindow` safely checks whether `hwnd`'s thread has stopped processing messages.
    if unsafe { IsHungAppWindow(hwnd) }.as_bool() {
        flags |= SWP_ASYNCWINDOWPOS;
    }

    // SAFETY: `hwnd` is a valid window handle and `None` is passed for `hWndInsertAfter`
    // because `SWP_NOZORDER` is set.
    unsafe { SetWindowPos(hwnd, None, x, y, width, height, flags) }.map_err(WinError::SetPos)
}

/// Queries `GetWindowRect` (which includes invisible DWM resize borders).
fn raw_window_rect(hwnd: HWND) -> Result<Rect, WinError> {
    let mut r = RECT::default();
    // SAFETY: `r` is a valid writable `RECT` on the stack.
    unsafe { GetWindowRect(hwnd, &raw mut r) }.map_err(WinError::WindowRect)?;
    Ok(from_win_rect(r))
}

/// Queries `DWMWA_EXTENDED_FRAME_BOUNDS` (the visible window bounds excluding invisible borders).
fn dwm_frame_rect(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    #[expect(clippy::cast_possible_truncation)]
    let size = size_of::<RECT>() as u32;
    // SAFETY: `r` is a valid writable `RECT` matching `size` for `DWMWA_EXTENDED_FRAME_BOUNDS`.
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

/// Converts a Win32 `RECT` (`left, top, right, bottom`) to [`tile::Rect`] (`x, y, width, height`).
const fn from_win_rect(r: RECT) -> Rect {
    Rect { x: r.left, y: r.top, width: r.right - r.left, height: r.bottom - r.top }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_history_records_tracks_targets_and_takes() {
        let mut hist = RestoreHistory::new();
        let r1 = Rect { x: 100, y: 120, width: 800, height: 600 };
        let r2 = Rect { x: 200, y: 220, width: 900, height: 700 };
        let target = Rect { x: 510, y: 215, width: 900, height: 700 };

        hist.record(42, r1);
        hist.record(42, r2); // In-place update for same HWND key
        hist.set_last_target(42, target);

        assert!(hist.has_entry(42));
        assert!(hist.is_tracked_at(42, target));
        assert_eq!(hist.take(42), Some(r2));
        assert!(!hist.has_entry(42));
        assert_eq!(hist.take(42), None);
    }

    #[test]
    fn restore_history_reuses_taken_slots_before_evicting() {
        let mut hist = RestoreHistory::new();
        for i in 1..=16_isize {
            #[expect(clippy::cast_possible_truncation)]
            let r = Rect { x: i as i32, y: 0, width: 500, height: 400 };
            hist.record(i, r);
        }

        // Free slot 5 and insert key 99: key 1 must NOT be evicted because slot 5 is empty.
        assert!(hist.take(5).is_some());
        hist.record(99, Rect { x: 99, y: 0, width: 500, height: 400 });
        assert!(hist.has_entry(1));
        assert!(hist.has_entry(99));

        // Now all 16 slots are full; inserting key 100 evicts slot 1.
        hist.record(100, Rect { x: 100, y: 0, width: 500, height: 400 });
        assert_eq!(hist.take(1), None);
        assert_eq!(hist.take(100), Some(Rect { x: 100, y: 0, width: 500, height: 400 }));
    }
}
