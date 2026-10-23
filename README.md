# ytmcross

A minimal, fast, native YouTube Music client for Windows, written in Rust. No Electron and no bundled browser.

- **Sign-in:** a webview window opens once for Google sign-in, saves the session to `cookies.txt`, and exits, so no browser engine keeps running.
- **Interface:** native egui, drawn on the CPU, so no GPU driver is loaded. It only draws when something changes: nothing at all when idle or minimized, and one frame per second while music plays.
- **Playback:** done inside the app, with no external player. Audio streams over HTTP and is decoded in pure Rust (symphonia, AAC 256 kbps with Premium).
- **Instant start:** stream addresses are resolved natively (YouTube's scrambled URLs are unlocked in an embedded QuickJS), and the start of every song on screen is preloaded, so a click plays in a few milliseconds.
- **Footprint:** about 4 MB of RAM idle and about 8 MB while playing.
- **Taskbar and keys:** previous / play-pause / next buttons on the taskbar thumbnail, and system-wide shortcuts.
- **Ricing:** readable TOML theme files, reloaded live with **F5**.

## Install

```powershell
winget install craftyderpysmall.ytmcross
```

Then run `ytmcross` (from Win+R or a terminal). Or download the zip from [Releases](https://github.com/craftyderpysmall/ytmcross/releases) and keep its three files together.

## Requirements

Windows 10 or 11, x64. The sign-in window uses the WebView2 runtime, which ships with Windows. No other programs are needed to play music.

## Build

Rust's `stable-x86_64-pc-windows-gnu` toolchain, and MinGW-w64 gcc on `PATH` (for example MSYS2's `mingw-w64-x86_64-gcc`, in `C:\msys64\mingw64\bin`):

```powershell
cargo +stable-x86_64-pc-windows-gnu build --release
.\target\release\ytmcross.exe
```

This builds two programs that belong in the same folder: `ytmcross.exe` and the small `ytmcross-login.exe`, which shows the sign-in window and needs `WebView2Loader.dll` beside it (from the `webview2-com-sys` crate's `x64` folder). Keeping the webview in its own program means the player itself never loads any browser code.

## Files

| | |
|---|---|
| config, themes, downloads | `%APPDATA%\ytmcross` |
| session (`cookies.txt`) | `%LOCALAPPDATA%\ytmcross` |

`cookies.txt` is your Google session. Keep it private.

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
| F5 | Reload config and theme |

The player shortcuts (previous, play / pause, next) work from any app; set them in Preferences.

## Troubleshooting

Run with `YTMCROSS_TRACE=1` to log millisecond timings (click, stream lookup, preload, first audio) to `trace.log` in the session folder above. `YTMCROSS_STATS=1` shows the time per drawn frame in the window title.

## License

MIT. See [LICENSE](LICENSE).

## Disclaimer

ytmcross uses YouTube Music's private web API, which is unofficial and may change without notice. It isn't affiliated with Google or YouTube.
