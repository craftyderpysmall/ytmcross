# ytmcross: notes for Claude

Native Rust YouTube Music client (no Electron, no browser at runtime).

## User priorities (every task)

1. Runtime performance first: low RAM, zero idle CPU, fast startup, nothing in the background.
2. Minimal and clean: no feature creep, remove unused code.
3. Disk size matters, but less than RAM/CPU.
4. After each request, keep looking for startup/storage/CPU/RAM wins and unused code. Measure before and after with `dev/` scripts and report numbers.
5. When done: commit, and push to `origin master` (github.com/craftyderpysmall/ytmcross) when asked.

## Build (Windows, GNU toolchain via scoop)

Every PowerShell command that runs cargo needs:

```powershell
$env:Path = "$env:USERPROFILE\scoop\persist\rustup\.cargo\bin;$env:USERPROFILE\scoop\apps\mingw\current\bin;$env:Path"
$env:CARGO_HOME = "$env:USERPROFILE\scoop\persist\rustup\.cargo"
$env:RUSTUP_HOME = "$env:USERPROFILE\scoop\persist\rustup\.rustup"
```

- Toolchain: `stable-x86_64-pc-windows-gnu` (folder override). Release build ~2 min (fat LTO).
- Keep `cargo clippy --all-targets` at zero warnings; `cargo test` must pass.
- Two binaries: `ytmcross` and `ytmcross-login` (`src/bin/`, the sign-in webview, needs `WebView2Loader.dll` beside it; copy from webview2-com-sys `x64/` if missing). The main exe must never import that DLL (`objdump -p target\release\ytmcross.exe | Select-String "DLL Name"`).
- `build.rs` deflates `assets/ejs/*.js` into the binary and embeds `assets/ytmcross.manifest` (segment heap) via `windres`.
- Per-crate `opt-level = "s"` in Cargo.toml is only for helper-only or cold crates; measure CPU before adding more.

## Pitfalls

- PowerShell 5.1. Many files are CRLF: multi-line `.Replace()` silently fails. Prefer the Edit tool, or normalize CRLF first, and verify replacements happened.
- Commit messages with quotes: write to a file and `git commit -F <file>`.
- Write files as UTF-8 without BOM: `[IO.File]::WriteAllText(p, t, (New-Object Text.UTF8Encoding $false))`.
- Editing a bundled theme (`assets/themes/*.toml`): add the previous version's hash (`node dev/fnv.js <file>`) to `OLD_BUNDLED_THEMES` in `config.rs`, so unedited user copies upgrade.
- Theme colors: labels on accent buttons and filled icons use `text`, never `background`.

## Architecture (src/)

- `ui/shell.rs`: winit loop, CPU rendering (egui_software_backend, caching off). Draws only on change, nothing when hidden; animation frames wait on `DwmFlush`. `YTMCROSS_STATS=1` shows per-phase frame times in the title.
- `ui/damage.rs`: hashes every shape per frame; only changed regions are tessellated, rasterized and blitted. Partial frames must stay pixel-identical to full ones (check with screenshots + `pxdiff.py` after a forced full redraw: minimize/restore).
- `ui/present.rs`: Windows presents with GDI from a `VirtualAlloc` frame freed whenever no animation frame is due (softbuffer elsewhere). `ui/clipboard.rs`: Win32 text clipboard (egui-winit's clipboard feature is off on Windows).
- `ui/app.rs`: all state. Spotify-style `Queue` (current, user `manual` queue first, then `context`; radio appended at the end). Back history of pages. `likes` overrides like status. `last-played` restores the track paused; its stream is prefetched only when the pointer hovers play. System fallback fonts are installed on first need.
- `ui/views.rs`: views return an `Action` applied afterwards; virtualized `track_list`; queue rows via `QueueRow`.
- `ui/widgets.rs`: custom widgets. `track_row`: title (plain), `byline` (Artist and Album links, plays), like button (hover only, filled if liked) left of a fixed-width duration slot.
- `ui/menus.rs`: Account/Equalizer/Pitch/Theme (with "Open themes folder")/Preferences (download folder) popups, Downloads button (opens the folder via `proc::open_folder`: explorer/open/xdg-open, nothing loaded in-process), context menus (Download, Delete owned playlists, Remove saved ones), delete modal. `ui/bar.rs`: now-playing bar.
- `pool.rs`: worker threads that exit after 5 idle s; used by `worker.rs` (API, one thread), album art (4), and the resolver/engine threads follow the same rule. Idle app: 6 threads.
- `worker.rs`: API requests (`Request`/`Response`).
- `api/`: InnerTube WEB_REMIX client, parser (searches leaf renderers), models (`Track`, `Playlist` with `kind`/`owned`/`save_id`, `Header`, `Artist`).
- `stream/`: native stream resolving: `player` endpoint + unscrambling in QuickJS (`solver.rs`, bytecode cached, built by the `--build-solver` helper process). A 403 rebuilds the solver. yt-dlp is intentionally not used. Only likely plays (hovered/pressed row, next in queue) get their first 128 KiB preloaded.
- `player/`: symphonia + cpal. Callback applies volume, `pitch.rs` (WSOLA + resampler, bypassed at 0, buffers allocated on first use) and `eq.rs`. `index.rs` reads the sidx: seeks/resumes splice the stream (head + target fragment) for one request. After 30 s paused the engine releases track and device (`PlayerEvent::Released`) and its thread exits. Downloaded songs play from the file (`load_file`, no index, no network); the app skips resolver hints for them.
- `download.rs`: one `download` pool thread. Songs: the AAC stream as-is, tags appended to moov/udta/meta/ilst (YouTube's files already have one with `©too`), written as `.part` then renamed, `Artist - Title [videoId].m4a`. Playlists/albums: `Title [id].m3u8` of those files. The folder is the record (scanned at startup only if it exists). Default folder `downloads` in the config dir, `[downloads] folder` in config.toml.
- `theme.rs`: themes, subset Inter fonts, memory-mapped system fallback fonts. `config.rs`: line-based `config.toml` edits.

## YouTube Music API facts

- Search filters: songs `EgWKAQIIAWoMEA4QChADEAQQCRAF`, albums `EgWKAQIYAWoMEA4QChADEAQQCRAF`.
- Artist/album links are runs with `browseEndpoint` pageType `MUSIC_PAGE_TYPE_ARTIST`/`_ALBUM` in any column after the title.
- Like status: `likeButtonRenderer.likeStatus`, or a like action with status `INDIFFERENT` (already liked) / `LIKE`. Like: `like/like` / `like/removelike` with `{target:{videoId}}`; save/remove playlist or album with `{target:{playlistId}}` (albums use their `OLAK5uy_` id).
- Library: `FEmusic_liked_playlists`, `FEmusic_liked_albums`. Own playlists have `playlistEditorEndpoint` in their menu. The listing lags behind edits: update locally.
- Album page `MPREb_…`: header `musicResponsiveHeaderRenderer`; rows lack artist/art. Artist page: `musicImmersiveHeaderRenderer`, first `musicShelfRenderer` = top songs, carousels "Albums" / "Singles & EPs".
- Signed out, responses say `logged_in=0`; only treat it as expiry when a session exists.

## dev/ scripts

- `startup.ps1` (size, first frame, idle memory), `memtest.ps1` (memory over use).
- `launch.ps1` + `win.ps1 -Action shot|click|rclick|drag|move|type` to drive the window (client pixels, 1100x720).
- `yt.ps1 <endpoint> '<json>' <out>` authenticated InnerTube probe; `rows.js`, `menus.js`, `jq.js` summarize responses; `pxdiff.py` pixel diffs.
- `YTMCROSS_TRACE=1` writes timings to `trace.log` in the data dir.

## Baselines

Main exe 10.41 MB, login exe 0.72 MB. Process creation to main ~30 ms (OS; a trivial exe is the same), main to first frame ~34 ms (~20 ms of it is winit window creation). Idle 3.5 MB private, 6 threads. memtest settles ~8.5 MB. Playing ~13 Mcycles/s visible, ~9.7 minimized; true idle ~0. Seek ~30 ms.

## Data

Config, themes and downloads: `%APPDATA%\ytmcross`. `cookies.txt` (never print or publish), `last-played`, solver cache, `trace.log`: `%LOCALAPPDATA%\ytmcross`.
