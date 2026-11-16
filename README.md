# ortho

fast rectangle-style window manager for windows. zero animations, dwm border compensation, mixed-dpi aware, single binary.

## hotkeys

| key | action |
|---|---|
| `ctrl+alt+left / right` | left / right (`1/2` → `2/3` → `1/3`) |
| `ctrl+alt+up / down` | top / bottom (`1/2` → `2/3` → `1/3`) |
| `ctrl+alt+u / i / j / k` | quarters (tl, tr, bl, br) |
| `ctrl+alt+enter` | almost maximize (`85%`) |
| `ctrl+alt+m` | maximize (toggles outer gap / edge-to-edge) |
| `ctrl+alt+c` | center (toggles current size / middle `1/3`) |
| `ctrl+alt+backspace` | restore floating position |
| `ctrl+alt+win+left / right` | move to prev / next monitor |

## config

stored at `%appdata%\ortho\config.json`. created on first run and hot-reloaded on keypress. set any hotkey to `"none"` to disable it.

```json
{
  "gap": 8,
  "hotkeys": {
    "left": "ctrl+alt+left",
    "right": "ctrl+alt+right"
  }
}
```

## cli

control a running instance from the terminal or scripts:

```sh
orthowm <left|right|top|bottom|tl|tr|bl|br|almost|max|center|restore|next|prev|config|startup|quit>
```

## build

```sh
cargo build --release
```
