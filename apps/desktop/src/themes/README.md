# Omarchy palettes

Same selection model as WaveSink: Original, Follow Omarchy when available, static
palettes on every OS. Legacy light-mode preferences map to Flexoki Light.
The picker previews background/accent/foreground swatches; selection persists
immediately on this device. Existing explicit choices remain explicit choices.

With no saved choice, Linux follows the active palette at
`~/.local/state/omarchy/current/theme/colors.toml` and `theme.name`. Native IPC
checks these every two seconds. Theme changes update the running app, including
the HTML reader. No desktop config changes, hooks, shell commands, or restart.
Missing Omarchy data falls back to Original; explicit static choices do not
follow desktop changes. Native palette strings must be valid hex colors.

`omarchy.ts` snapshots **22 stock + 15 custom** installed palettes. All color roles
present in WaveSink's bundled presets match these snapshots (case insensitive).
Extra roles available in the installed themes are retained. Colors only: no
wallpapers, fonts, executables, terminal configuration, or system paths bundled.
Stock sources are Omarchy's installed palettes; custom sources are the user's
installed theme palettes. Palette names and stock/custom provenance are recorded
with every entry. Theme code from WaveSink is not a dependency.

Regenerate intentionally on an Omarchy development machine (Python 3.11+):

```sh
python3 apps/desktop/scripts/import-themes.py
# Optional source roots:
python3 apps/desktop/scripts/import-themes.py --stock /path/to/stock --custom /path/to/custom
```

Normal builds use the committed snapshot; Windows/macOS/non-Omarchy Linux never
need the original directories, Python, or Omarchy. Legacy Alacritty and indexed
terminal palettes use WaveSink's role mapping and luminance threshold. Black or
white label fallbacks keep accent and selection text readable.

Stock: Catppuccin, Catppuccin Latte, Ethereal, Everforest, Flexoki Light, Gruvbox,
Hackerman, Kanagawa, Last Horizon, Lumon, Lupine, Matte Black, Miasma, Nord,
Osaka Jade, Retro 82, Ristretto, Rose Pine, Solitude, Tokyo Night, Vantablack, White.

Custom: Aetheria, Archwave, Ash, Ayaka, Azure Glow, Black Gold, Matrix, Moodpeak,
Neon Dusk, Retro Bloom, RetroPC, Space Monkey, Synthwave '84, Terminus, Waveform Dark.

Linux startup prefers Wayland when `WAYLAND_DISPLAY` is set. Like WaveSink,
`WEBKIT_DISABLE_DMABUF_RENDERER=1` defaults before GTK/WebKit initialization to
avoid the DMA-BUF protocol crash. An explicitly supplied renderer setting is
preserved. This does not select X11 or XWayland.
