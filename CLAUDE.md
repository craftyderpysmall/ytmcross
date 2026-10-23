# ytmcross: notes for Claude

Native Rust YouTube Music client for Windows (no Electron, no browser at runtime). Windows only: no cfgs or code paths for other OSes.

## User priorities (every task)

1. Runtime performance first: low RAM, zero idle CPU, fast startup, nothing in the background.
2. Minimal and clean: no feature creep, remove unused code.
3. Disk size matters, but less than RAM/CPU.
4. After each request, keep looking for startup/storage/CPU/RAM wins and unused code. Measure before and after with `dev/` scripts and report numbers.
5. ONLY when explicitly asked: commit, and push to `origin master` (github.com/craftyderpysmall/ytmcross).

## Build (GNU toolchain, MSYS2 gcc)

Rust via rustup in `~\.rustup` (default toolchain is MSVC, which is not used: no MSVC build tools are installed). gcc, windres and objdump come from MSYS2 (`winget install MSYS2.MSYS2`, then `pacman -S mingw-w64-x86_64-gcc`). Every PowerShell command that runs cargo needs:

```powershell
$env:Path = "C:\msys64\mingw64\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo +stable-x86_64-pc-windows-gnu build --release
```

- Toolchain: always `+stable-x86_64-pc-windows-gnu`. Release build ~2 min (fat LTO).
- Keep `cargo clippy --all-targets` at zero warnings; `cargo test` must pass.
- Two binaries: `ytmcross` and `ytmcross-login` (`src/bin/`, the sign-in webview, needs `WebView2Loader.dll` beside it; copy from webview2-com-sys `x64/` if missing). The main exe must never import that DLL (`objdump -p target\release\ytmcross.exe | Select-String "DLL Name"`).
- `build.rs` deflates `assets/ejs/*.js` into the binary and embeds `assets/ytmcross.manifest` (segment heap) and `assets/ytmcross.ico` (from `ytmcross.svg`, PNG entries; the window loads it with `Icon::from_resource`) via `windres`.
- Per-crate `opt-level = "s"` in Cargo.toml is only for helper-only or cold crates; measure CPU before adding more.

## Releases

- Bump `version` in Cargo.toml, commit, push tag `vX.Y.Z`. `.github/workflows/release.yml` builds the Windows zip (GNU toolchain via MSYS2; exe, login exe, `WebView2Loader.dll`), publishes the GitHub release, then runs `wingetcreate update craftyderpysmall.ytmcross ... --submit` (skipped without the `WINGET_TOKEN` repo secret, a classic PAT with `public_repo`).
- winget package `craftyderpysmall.ytmcross` (microsoft/winget-pkgs): zip with portable `ytmcross.exe`, `ArchiveBinariesDependOnPath: true` so the install folder goes on PATH (the login exe and DLL must stay beside the main exe; a symlink alias would break that). Manually: `wingetcreate update craftyderpysmall.ytmcross -v X.Y.Z -u <zip url> --submit`.

## Pitfalls

- PowerShell 5.1. Source files are CRLF in the working tree (LF in git, `core.autocrlf`): multi-line `.Replace()` silently fails. Prefer the Edit tool, or normalize CRLF first, and verify replacements happened.
- The Bash tool halves backslashes in heredocs: for scripted edits, write the script to a file first.
- Commit messages with quotes: write to a file and `git commit -F <file>`.
- Write files as UTF-8 without BOM: `[IO.File]::WriteAllText(p, t, (New-Object Text.UTF8Encoding $false))`.
- Editing a bundled theme (`assets/themes/*.toml`): add the previous version's hash (`node dev/fnv.js <file>`) to `OLD_BUNDLED_THEMES` in `config.rs`, so unedited user copies upgrade.
- Theme colors: labels on accent buttons and filled icons use `text`, never `background`.

## Architecture (src/)

- `ui/shell.rs`: winit loop, CPU rendering (egui_software_backend, caching off). Draws only on change, nothing when hidden; animation frames wait on `DwmFlush`. `YTMCROSS_STATS=1` shows per-phase frame times in the title.
- `ui/damage.rs`: hashes every shape per frame; only changed regions are tessellated, rasterized and blitted. Partial frames must stay pixel-identical to full ones (check with screenshots + `pxdiff.py` after a forced full redraw: minimize/restore).
- `ui/present.rs`: presents with GDI from a `VirtualAlloc` frame freed whenever no animation frame is due; `GetUpdateRect` tells when the OS lost the content (`Presenter::invalidated`). `ui/clipboard.rs`: text clipboard straight through Win32 on copy/paste; egui-winit's clipboard feature is off.
- `ui/app.rs`: all state. Spotify-style `Queue` (current, user `manual` queue first, then `context`; radio appended at the end). Back history of pages. `likes` overrides like status. `last-played` restores the track paused; its stream is prefetched only when the pointer hovers play. System fallback fonts are installed on first need.
- `ui/views.rs`: views return an `Action` applied afterwards; virtualized `track_list`; queue rows via `QueueRow`.
- `ui/widgets.rs`: custom widgets. `track_row`: title (plain), `byline` (a link per artist via `Track::artist_links` byte ranges into `artists`, Album link, plays), like button (hover only, filled if liked) left of a fixed-width duration slot.
- `ui/menus.rs`: Account/Equalizer/Pitch/Preferences (theme with "Open themes folder", download folder, shortcuts) popups, Downloads button (opens the folder in explorer via `proc::open_folder`, nothing loaded in-process), context menus (Download, Delete owned playlists, Remove saved ones), delete modal. `ui/bar.rs`: now-playing bar. `ui/win32.rs`, via a wrapped window procedure: taskbar thumbnail buttons (prev/play-pause/next, added on "TaskbarButtonCreated") and the `[keys]` shortcuts as system-wide hotkeys (`RegisterHotKey`, WM_HOTKEY). Both come back as `Wake::Control` → `App::control`; button state and hotkeys are pushed from `about_to_wait` (hotkeys unregistered while Preferences records one). A hotkey another app holds works in-window only (`take_key` in app.rs, ahead of Alt+Left = back). `config::Bind` parses/prints "Ctrl+Alt+Left" with egui key names.
- Naming: the package, binaries, paths and app id stay `ytmcross`; everything users see in the app (window and sign-in titles, sidebar title, exe FileDescription from build.rs) says "YouTube Music". The winget package name is `ytmcross`.
- `pool.rs`: worker threads that exit after 5 idle s; used by `worker.rs` (API, one thread), album art (4), and the resolver/engine threads follow the same rule. Idle app: 6 threads.
- `worker.rs`: API requests (`Request`/`Response`). `Notifier` wakes the event loop directly (shell's `Waker`), never via `ctx.request_repaint()`: egui drops requests while one is pending and only a frame clears that, so a hidden window would stop advancing the queue.
- `proc.rs`: helper processes (no console window, killed with us via a job object).
- `api/`: InnerTube WEB_REMIX client, parser (searches leaf renderers), models (`Track`, `Playlist` with `kind`/`owned`/`save_id`, `Header`, `Artist`).
- `stream/`: native stream resolving: `player` endpoint + unscrambling in QuickJS (`solver.rs`, bytecode cached, built by the `--build-solver` helper process; each runtime on a private heap destroyed with it). A 403 rebuilds the solver. yt-dlp is intentionally not used. Only likely plays (hovered/pressed row, next in queue) get their first 128 KiB preloaded.
- `player/`: symphonia + cpal. Callback applies volume, `pitch.rs` (WSOLA + resampler, bypassed at 0, buffers allocated on first use) and `eq.rs`. `index.rs` reads the sidx: seeks/resumes splice the stream (head + target fragment) for one request. After 30 s paused the engine releases track and device (`PlayerEvent::Released`) and its thread exits. Downloaded songs play from the file (`load_file`, no index, no network); the app skips resolver hints for them.
- `download.rs`: one `download` pool thread. Songs: the AAC stream as-is, tags appended to moov/udta/meta/ilst (YouTube's files already have one with `©too`), written as `.part` then renamed, `Artist - Title [videoId].m4a`. Playlists/albums: `Title [id].m3u8` of those files; the page button and menu toggle (removing deletes the .m3u8 and its songs no other .m3u8 lists). The folder is the record (scanned at startup only if it exists; a list counts only if all its songs exist). Default folder `downloads` in the config dir, `[downloads] folder` in config.toml.
- `theme.rs`: themes, subset Inter fonts, memory-mapped system fallback fonts (`%SystemRoot%\Fonts`). `config.rs`: line-based `config.toml` edits.

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

Main exe 9.99 MB, login exe 0.72 MB. Process creation to main ~30 ms (OS; a trivial exe is the same), main to first frame ~34 ms (~20 ms of it is winit window creation). Idle 3.5 MB private, 6 threads. memtest settles ~8.5 MB. Playing ~13 Mcycles/s visible, ~9.7 minimized; true idle ~0. Seek ~30 ms.

## Data

Config, themes and downloads: `%APPDATA%\ytmcross`. `cookies.txt` (never print or publish), `last-played`, solver cache, `trace.log`: `%LOCALAPPDATA%\ytmcross`.
