# ytmcross

A minimal, fast, native YouTube Music client written in Rust. No Electron and no bundled browser.

- **Sign-in:** a webview window opens once for Google sign-in, saves the session to `cookies.txt`, and exits, so no browser engine keeps running.
- **Interface:** native egui, drawn on the CPU, so no GPU driver is loaded. It only draws when something changes: nothing at all when idle or minimized, and one frame per second while music plays.
- **Playback:** done inside the app, with no external player. Audio streams over HTTP and is decoded in pure Rust (symphonia, AAC 256 kbps with Premium).
- **Instant start:** stream addresses are resolved natively (YouTube's scrambled URLs are unlocked in an embedded QuickJS), and the start of every song on screen is preloaded, so a click plays in a few milliseconds.
- **Footprint:** about 8 MB of RAM idle and about 18 MB while playing, in a single process.
- **Ricing:** readable TOML theme files, reloaded live with **F5**.

## Requirements

None: no external programs are needed to play music.

To build on Linux: `libwebkit2gtk-4.1-dev libgtk-3-dev libasound2-dev` (Debian/Ubuntu) or `webkit2gtk4.1-devel gtk3-devel alsa-lib-devel` (Fedora).

## Build

```sh
cargo build --release
./target/release/ytmcross
```

This builds two programs that belong in the same folder: `ytmcross` and the small `ytmcross-login`, which shows the sign-in window (on Windows, with `WebView2Loader.dll` beside it). Keeping the webview in its own program means the player itself never loads any browser code.

The first launch opens the sign-in window. Sign out or in again from the **Account** menu. The **Equalizer** and **Pitch** menus apply instantly and are saved to `config.toml`.

## Files

| | Windows | Linux | macOS |
|---|---|---|---|
| config + themes | `%APPDATA%\ytmcross` | `~/.config/ytmcross` | `~/Library/Application Support/ytmcross` |
| session (`cookies.txt`) | `%LOCALAPPDATA%\ytmcross` | `~/.local/share/ytmcross` | `~/Library/Application Support/ytmcross` |

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

## Troubleshooting

Run with `YTMCROSS_TRACE=1` to log millisecond timings (click, stream lookup, preload, first audio) to `trace.log` in the session folder above. `YTMCROSS_STATS=1` shows the time per drawn frame in the window title.

## Credits

Stream unlocking uses [yt-dlp/ejs](https://github.com/yt-dlp/ejs) (public domain; bundles meriyah, ISC, and astring, MIT) and [anenasa/ytdlp-ejs](https://github.com/anenasa/ytdlp-ejs) (MIT).

## Disclaimer

ytmcross uses YouTube Music's private web API, which is unofficial and may change without notice. It isn't affiliated with Google or YouTube.
