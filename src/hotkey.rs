//! Win32 global hotkey parsing, registration, and `WM_HOTKEY` command dispatch.
//!
//! All shortcuts are registered with `MOD_NOREPEAT` so holding down a key combination
//! fires a single 0ms action rather than flooding the thread message queue.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_LEFT,
    VK_NEXT, VK_OEM_MINUS, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};

use crate::config::HotkeysConfig;
use crate::tile::Action;

/// Maximum length in bytes of a single hotkey token (e.g. `"backspace"`).
const MAX_TOKEN_LEN: usize = 16;

/// Number of configurable hotkey commands.
const COMMAND_COUNT: usize = 14;

/// High-level window manager command triggered by a registered global hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Snap or cycle the focused window using a tiling [`Action`].
    Tile(Action),
    /// Restore the focused window to its pre-tiled floating bounds.
    Restore,
    /// Move the focused window to the previous monitor.
    PrevMonitor,
    /// Move the focused window to the next monitor.
    NextMonitor,
}

/// Active global hotkeys registered on the current Win32 thread.
///
/// Automatically unregisters all bound hotkeys when dropped or rebound.
#[derive(Debug, Default)]
pub struct HotkeyRegistry {
    bindings: Vec<(i32, Command)>,
    active_config: Option<HotkeysConfig>,
}

impl HotkeyRegistry {
    /// Registers all valid hotkey specifications in `cfg` on the calling thread.
    #[must_use]
    pub fn register(cfg: &HotkeysConfig) -> Self {
        let mut registry =
            Self { bindings: Vec::with_capacity(COMMAND_COUNT), active_config: None };
        registry.rebind(cfg);
        registry
    }

    /// Unregisters all currently active hotkeys and registers the bindings from `cfg`.
    ///
    /// No-op if `cfg` is identical to the currently registered [`HotkeysConfig`].
    pub fn rebind(&mut self, cfg: &HotkeysConfig) {
        if self.active_config.as_ref() == Some(cfg) {
            return;
        }
        self.unregister_all();
        self.active_config = Some(cfg.clone());

        let table: [(&str, Command); COMMAND_COUNT] = [
            (&cfg.left, Command::Tile(Action::Left)),
            (&cfg.right, Command::Tile(Action::Right)),
            (&cfg.top, Command::Tile(Action::Top)),
            (&cfg.bottom, Command::Tile(Action::Bottom)),
            (&cfg.top_left, Command::Tile(Action::TopLeft)),
            (&cfg.top_right, Command::Tile(Action::TopRight)),
            (&cfg.bottom_left, Command::Tile(Action::BottomLeft)),
            (&cfg.bottom_right, Command::Tile(Action::BottomRight)),
            (&cfg.almost, Command::Tile(Action::Almost)),
            (&cfg.maximize, Command::Tile(Action::Maximize)),
            (&cfg.center, Command::Tile(Action::Center)),
            (&cfg.restore, Command::Restore),
            (&cfg.prev_monitor, Command::PrevMonitor),
            (&cfg.next_monitor, Command::NextMonitor),
        ];

        let mut seen_combos: [(HOT_KEY_MODIFIERS, u32); COMMAND_COUNT] =
            [(HOT_KEY_MODIFIERS(0), 0); COMMAND_COUNT];
        let mut seen_len = 0;

        for (idx, (spec, cmd)) in table.into_iter().enumerate() {
            let Some((mods, vk)) = parse_hotkey(spec) else {
                continue;
            };
            if seen_combos[..seen_len].contains(&(mods, vk)) {
                continue;
            }
            seen_combos[seen_len] = (mods, vk);
            seen_len += 1;

            let Ok(id) = i32::try_from(idx + 1) else {
                continue;
            };
            // SAFETY: `None` associates the hotkey with the calling thread's message queue,
            // and `id` is a positive thread-unique hotkey identifier.
            if unsafe { RegisterHotKey(None, id, mods | MOD_NOREPEAT, vk) }.is_ok() {
                self.bindings.push((id, cmd));
            }
        }
    }

    /// Resolves a `WM_HOTKEY` identifier (`wParam`) to its corresponding [`Command`].
    #[must_use]
    pub fn command_for_id(&self, id: i32) -> Option<Command> {
        self.bindings.iter().find(|&&(bound_id, _)| bound_id == id).map(|&(_, cmd)| cmd)
    }

    /// Unregisters all active hotkeys from the OS.
    fn unregister_all(&mut self) {
        for &(id, _) in &self.bindings {
            // SAFETY: `id` was previously registered on the calling thread via `RegisterHotKey`.
            let _ = unsafe { UnregisterHotKey(None, id) };
        }
        self.bindings.clear();
    }
}

impl Drop for HotkeyRegistry {
    fn drop(&mut self) {
        self.unregister_all();
    }
}

/// Parses a human-readable hotkey string (e.g. `"ctrl+alt+win+left"`) into Win32
/// [`HOT_KEY_MODIFIERS`] and a virtual-key code (`vk`).
///
/// Returns `None` if the specification is empty, lacks at least one modifier key,
/// contains unknown tokens, or specifies multiple non-modifier keys.
#[must_use]
pub fn parse_hotkey(spec: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return None;
    }

    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut vk: Option<u32> = None;
    let mut buf = [0_u8; MAX_TOKEN_LEN];

    for raw_part in trimmed.split('+') {
        let raw_token = raw_part.trim();
        if raw_token.is_empty() || raw_token.len() > MAX_TOKEN_LEN || !raw_token.is_ascii() {
            return None;
        }
        for (dst, &src) in buf.iter_mut().zip(raw_token.as_bytes()) {
            *dst = src.to_ascii_lowercase();
        }
        let token = std::str::from_utf8(&buf[..raw_token.len()]).ok()?;
        match token {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" | "menu" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            key => {
                if vk.is_some() {
                    return None;
                }
                vk = Some(parse_vk(key)?);
            }
        }
    }

    if mods == HOT_KEY_MODIFIERS(0) {
        return None;
    }
    vk.map(|code| (mods, code))
}

/// Maps a lowercase key token to its Win32 virtual-key code (`VK_*`).
fn parse_vk(key: &str) -> Option<u32> {
    if let [byte] = key.as_bytes()
        && byte.is_ascii_alphanumeric()
    {
        return Some(u32::from(byte.to_ascii_uppercase()));
    }
    let code = match key {
        "left" => u32::from(VK_LEFT.0),
        "right" => u32::from(VK_RIGHT.0),
        "up" => u32::from(VK_UP.0),
        "down" => u32::from(VK_DOWN.0),
        "enter" | "return" => u32::from(VK_RETURN.0),
        "backspace" | "back" | "bs" => u32::from(VK_BACK.0),
        "space" => u32::from(VK_SPACE.0),
        "tab" => u32::from(VK_TAB.0),
        "esc" | "escape" => u32::from(VK_ESCAPE.0),
        "home" => u32::from(VK_HOME.0),
        "end" => u32::from(VK_END.0),
        "pageup" | "pgup" => u32::from(VK_PRIOR.0),
        "pagedown" | "pgdn" => u32::from(VK_NEXT.0),
        "delete" | "del" => u32::from(VK_DELETE.0),
        "plus" | "=" => u32::from(VK_OEM_PLUS.0),
        "minus" | "-" => u32::from(VK_OEM_MINUS.0),
        _ => return parse_function_key(key),
    };
    Some(code)
}

/// Parses function keys `"f1"` through `"f24"`.
fn parse_function_key(key: &str) -> Option<u32> {
    let digits = key.strip_prefix('f')?;
    let num: u32 = digits.parse().ok()?;
    if (1..=24).contains(&num) { Some(u32::from(VK_F1.0) + (num - 1)) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_and_multi_modifier_hotkeys() {
        let (mods, vk) = parse_hotkey("Ctrl + Alt + Left").unwrap_or_default();
        assert_eq!(mods, MOD_CONTROL | MOD_ALT);
        assert_eq!(vk, u32::from(VK_LEFT.0));

        let (mods_mon, vk_mon) = parse_hotkey("ctrl+alt+win+right").unwrap_or_default();
        assert_eq!(mods_mon, MOD_CONTROL | MOD_ALT | MOD_WIN);
        assert_eq!(vk_mon, u32::from(VK_RIGHT.0));

        let (mods_letter, vk_letter) = parse_hotkey("alt+shift+u").unwrap_or_default();
        assert_eq!(mods_letter, MOD_ALT | MOD_SHIFT);
        assert_eq!(vk_letter, u32::from(b'U'));

        let (_, vk_f12) = parse_hotkey("ctrl+f12").unwrap_or_default();
        assert_eq!(vk_f12, u32::from(VK_F1.0) + 11);
    }

    #[test]
    fn rejects_invalid_or_unmodified_hotkeys() {
        assert!(parse_hotkey("").is_none());
        assert!(parse_hotkey("none").is_none());
        assert!(parse_hotkey("left").is_none()); // Missing modifier
        assert!(parse_hotkey("ctrl+alt").is_none()); // Missing key
        assert!(parse_hotkey("ctrl+a+b").is_none()); // Multiple keys
        assert!(parse_hotkey("ctrl+f25").is_none()); // Out-of-range function key
    }
}
