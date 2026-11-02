# ytmcross

A minimal, fast, native YouTube Music client for Linux on Wayland, written in Rust. No Electron and no bundled browser.

- **Sign-in:** your own Chromium-family browser opens once on a throwaway profile. The YouTube session is saved to `cookies.txt`, the profile is deleted, and no browser keeps running.
- **Interface:** native egui, drawn on the CPU, so no GPU driver is loaded. It only draws when something changes: nothing at all when idle or out of view, and one frame per second while music plays.
- **Playback:** done inside the app, with no external player. Audio streams over HTTP and is decoded in pure Rust (symphonia, AAC 256 kbps with Premium).
- **Instant start:** stream addresses are resolved natively (YouTube's scrambled URLs are unlocked in an embedded QuickJS), and the start of every song on screen is preloaded, so a click plays in a few milliseconds.
- **Footprint:** about 2.3 MB of private memory and a single thread when idle.
- **Desktop integration:** MPRIS, so media keys, `playerctl` and status bars just work.
- **Ricing:** readable TOML theme files, reloaded live with **F5**.

## Requirements

- Linux on a Wayland compositor (Hyprland, Sway, GNOME, KDE). X11 isn't supported.
- `alsa-lib` (with PipeWire's or PulseAudio's ALSA plugin), `dbus`, `fontconfig`, `libxkbcommon`, `xdg-utils`.
- For sign-in: Chrome, Chromium, Brave, Vivaldi or Edge. Set `YTMCROSS_BROWSER` to use another build of them. Firefox doesn't work.
- Optional: `noto-fonts-cjk`, for Chinese, Japanese and Korean titles.

## Install

On Arch, [`packaging/PKGBUILD`](packaging/PKGBUILD) builds a package from a release:

```sh
cd packaging && makepkg -si
```

Elsewhere, download the tarball from [Releases](https://github.com/craftyderpysmall/ytmcross/releases), or build it yourself.

## Build

Rust stable, a C compiler, and the development files of ALSA and D-Bus:

```sh
cargo build --release
./target/release/ytmcross
```

To add it to your application launcher:

```sh
install -Dm755 target/release/ytmcross ~/.local/bin/ytmcross
install -Dm644 assets/ytmcross.desktop -t ~/.local/share/applications
install -Dm644 assets/ytmcross.svg -t ~/.local/share/icons/hicolor/scalable/apps
```

## Files

| | |
|---|---|
| config, themes | `~/.config/ytmcross` |
| session (`cookies.txt`), downloads, logs | `~/.local/share/ytmcross` |
| solver cache | `~/.cache/ytmcross` |

These follow `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME`. The session file is readable only by you.

`cookies.txt` is your YouTube session. Keep it private.

## Theming

`config.toml` picks a theme with `theme = "name"`, which loads `themes/<name>.toml`. Any token a theme leaves out falls back to the default theme, so this is a complete theme:

```toml
[colors]
accent = "#ff5555"
```

The full set of tokens (`colors`, `font`, `shape`, `art`) is documented in `themes/default.toml`, and `themes/scarf-dark.toml` is a worked example. Layout options (bar position, sidebar and its entries) live in `config.toml`.

## Keys

| Key | Action |
|---|---|
| Space | Play / pause |
| Ctrl+F | Search |
| Ctrl+1 / 2 / 3 | Switch view (sidebar order) |
| Alt+Left / mouse back | Back (album and artist pages) |
| Ctrl+Left / Ctrl+Right | Previous / next track |
| F5 | Reload config and theme |

These work in the app's window; set the player shortcuts in Preferences. For keys that work everywhere, bind them in your compositor. On Hyprland, in `hyprland.lua`:

```lua
hl.bind("XF86AudioPlay", hl.dsp.exec_cmd("playerctl -p ytmcross play-pause"), { locked = true })
hl.bind("XF86AudioNext", hl.dsp.exec_cmd("playerctl -p ytmcross next"), { locked = true })
hl.bind("XF86AudioPrev", hl.dsp.exec_cmd("playerctl -p ytmcross previous"), { locked = true })
```

## Troubleshooting

Run with `YTMCROSS_TRACE=1` to log millisecond timings (click, stream lookup, preload, first audio) to `trace.log` in the session folder above. `YTMCROSS_STATS=1` shows the time per drawn frame in the window title.

## License

MIT. See [LICENSE](LICENSE).

## Disclaimer

ytmcross uses YouTube Music's private web API, which is unofficial and may change without notice. It isn't affiliated with Google or YouTube.
