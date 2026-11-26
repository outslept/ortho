<div align="center">

  <img src="icon.svg" width="96" alt="ortho">

  <h1>ortho</h1>

  <p>
    fast, rectangle-style window tiler for windows 10 &amp; 11.<br>
    zero animations · dwm border compensation · mixed-dpi aware · single binary
  </p>
  
</div>

## highlights

- **0ms response** - suppresses dwm animations on unmaximize; never blocks on hung windows.
- **pixel-perfect** - compensates for invisible win10/11 resize borders and eliminates 1px rounding seams between adjacent tiles (`2/3` + `1/3`).
- **multi-monitor & mixed-dpi** - scales gaps per display dpi and preserves exact grid positions across monitors.
- **smart exclusions** - leaves 1px hover margins for auto-hide taskbars and ignores borderless fullscreen games.
- **zero overhead** - single ~400 kb native binary, stack-allocated state, lazy metadata-based config reload on keypress.

## hotkeys

| shortcut | action | behavior |
| :--- | :--- | :--- |
| `ctrl+alt+left` / `right` | **left / right** | cycles `1/2` → `2/3` → `1/3` |
| `ctrl+alt+up` / `down` | **top / bottom** | cycles `1/2` → `2/3` → `1/3` |
| `ctrl+alt+u` / `i` / `j` / `k` | **quarters** | top-left, top-right, bottom-left, bottom-right |
| `ctrl+alt+enter` | **almost** | centered `85%` width × `85%` height |
| `ctrl+alt+m` | **maximize** | toggles padded work area (`gap`) ↔ edge-to-edge (`0px`) |
| `ctrl+alt+c` | **center** | centers current size; press again for middle `1/3` column |
| `ctrl+alt+backspace` | **restore** | restores pre-tiled floating bounds |
| `ctrl+alt+win+left` / `right` | **monitor hop** | moves window to previous / next display |

```text
+-----------+-----------+   +---------------+-------+   +-------+-------+-------+
|           |           |   |               |       |   |       |       |       |
|   left    |   right   |   |   left 2/3    | right |   | left  |center | right |
|   (1/2)   |   (1/2)   |   |  (2x press)   |  1/3  |   |  1/3  |  1/3  |  1/3  |
|           |           |   |               |       |   |       | (2x c)|       |
+-----------+-----------+   +---------------+-------+   +-------+-------+-------+
```

## usage & tray

run `orthowm.exe`. it runs silently in the background and adds an icon to the system tray:
- **tray menu** - trigger any tile action with the mouse, open `config.json`, or toggle **launch at login**.
- **elevated windows** - to tile administrator windows (e.g. task manager, elevated terminal), run `orthowm.exe` as administrator.

## config

stored at `%appdata%\ortho\config.json`. created automatically on first run and hot-reloaded on keypress when modified.

```json
{
  "gap": 8,
  "hotkeys": {
    "left": "ctrl+alt+left",
    "right": "ctrl+alt+right",
    "top": "ctrl+alt+up",
    "bottom": "ctrl+alt+down",
    "top_left": "ctrl+alt+u",
    "top_right": "ctrl+alt+i",
    "bottom_left": "ctrl+alt+j",
    "bottom_right": "ctrl+alt+k",
    "almost": "ctrl+alt+enter",
    "maximize": "ctrl+alt+m",
    "center": "ctrl+alt+c",
    "restore": "ctrl+alt+backspace",
    "prev_monitor": "ctrl+alt+win+left",
    "next_monitor": "ctrl+alt+win+right"
  }
}
```

- **`gap`** - logical margin in pixels at 96 dpi (`0..=128`, scaled automatically per monitor dpi). set to `0` for seamless edge-to-edge tiling.
- **`hotkeys`** - modifiers (`ctrl`, `alt`, `shift`, `win`) + key (`a`–`z`, `0`–`9`, `f1`–`f24`, `left`, `right`, `up`, `down`, `enter`, `backspace`, `space`, `tab`, `esc`, `home`, `end`, `pgup`, `pgdn`, `del`, `plus`, `minus`). set to `"none"` to disable.

## cli

control a running instance from scripts, terminals, or launchers (flow launcher, raycast, autohotkey, stream deck):

```sh
orthowm <command>
```

| command | aliases | action |
| :--- | :--- | :--- |
| `left`, `right`, `top`, `bottom` | `l`, `r`, `u`, `d`, `up`, `down` | snap half / cycle thirds |
| `top-left`, `top-right` | `tl`, `tr`, `topleft`, `topright` | snap top quarters |
| `bottom-left`, `bottom-right` | `bl`, `br`, `bottomleft`, `bottomright` | snap bottom quarters |
| `almost` | `a` | center at `85%` |
| `maximize` | `max`, `m` | toggle padded / fullscreen |
| `center` | `c` | center window / middle `1/3` |
| `restore` | `back` | restore floating position |
| `next-monitor`, `prev-monitor` | `next`, `nm`, `prev`, `pm` | move across monitors |
| `config` | `cfg` | open `config.json` in default editor |
| `startup` | `autostart` | toggle launch at login |
| `quit` | `exit`, `stop`, `q` | stop running instance |

## build

requires **rust 1.88+** (2024 edition).

```sh
cargo test
cargo build --release
```
