# README screenshot

The README image is a Ghostty window with the macmon TUI, stored in the `assets` branch.

## Window

Ghostty with JetBrains Mono at the default 13 pt and the Catppuccin Macchiato (dark) theme. Resize the frontmost window to 120×32 cells:

```sh
resize-term 966 585
stty size  # 32 120
```

`resize-term` is a fish function from the dotfiles; it sets the window size in points:

```sh
osascript -e 'tell application "System Events" to tell process "ghostty" to set size of front window to {966, 585}'
```

The size assumes one tab: a tab bar takes rows. At 120 columns every box title fits in full; the window gives about the same text size on GitHub as the previous screenshot. The v0.9 image is 1936×1174 px.

## Content

- Mixed load, so the graphs show green, yellow and red. Avoid `macmon stress`: it puts a `macmon` process with high CPU and power at the top of the list.
- Wait a minute or two for the graphs to fill their width.
- Sort the process list by POWER (`s`) and clear the selection (`Esc`).
- Close apps you don't want in the list.

## Upload

Add the image to the `assets` branch as `macmon-v<major>.<minor>.png`, keeping the older images for older README versions, and point the README `<img>` at it. Push `assets` before `main`.
