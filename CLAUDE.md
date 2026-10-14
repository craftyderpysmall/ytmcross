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
- `build.rs` deflates `assets/ejs/*.js` into the binary and embeds `assets/ytmcross.manifest` (segment heap) and `assets/ytmcross.ico` (from `ytmcross.svg`, PNG entries; the window loads it with `Icon::from_resource`) via `windres`.
- Per-crate `opt-level = "s"` in Cargo.toml is only for helper-only or cold crates; measure CPU before adding more.

## Releases

- Bump `version` in Cargo.toml, commit, push tag `vX.Y.Z`. `.github/workflows/release.yml` builds the Windows zip (GNU toolchain via MSYS2; exe, login exe, `WebView2Loader.dll`), publishes the GitHub release, and commits the new url/hash to `bucket/ytmcross.json` (this repo is the Scoop bucket).
- COPR (`craftyderpysmall/ytmcross`) builds from SCM with `make_srpm`: `.copr/Makefile` vendors all crates into the SRPM and stamps Version/Release into `.copr/ytmcross.spec` (Release 1 on the version tag, snapshots otherwise). New system libraries need a `BuildRequires` there.

## Pitfalls

- PowerShell 5.1. Many files are CRLF: multi-line `.Replace()` silently fails. Prefer the Edit tool, or normalize CRLF first, and verify replacements happened.
- Commit messages with quotes: write to a file and `git commit -F <file>`.
- Write files as UTF-8 without BOM: `[IO.File]::WriteAllText(p, t, (New-Object Text.UTF8Encoding $false))`.
- Editing a bundled theme (`assets/themes/*.toml`): add the previous version's hash (`node dev/fnv.js <file>`) to `OLD_BUNDLED_THEMES` in `config.rs`, so unedited user copies upgrade.
- Theme colors: labels on accent buttons and filled icons use `text`, never `background`.

## Architecture (src/)

- `ui/shell.rs`: winit loop, CPU rendering (egui_software_backend, caching off). Draws only on change, nothing when hidden; animation frames wait on `DwmFlush`. `YTMCROSS_STATS=1` shows per-phase frame times in the title.
- `ui/damage.rs`: hashes every shape per frame; only changed regions are tessellated, rasterized and blitted. Partial frames must stay pixel-identical to full ones (check with screenshots + `pxdiff.py` after a forced full redraw: minimize/restore).
- `ui/present.rs`: Windows presents with GDI from a `VirtualAlloc` frame freed whenever no animation frame is due. Elsewhere softbuffer: buffer `age()` 1 (X11) draws the damage, 2 (Wayland) the damage of both frames, then `present_with_damage`; backends that always return blank buffers (macOS CG, age 0) draw into a kept copy freed when idle. A redraw we did not request means the OS lost the content (`Presenter::invalidated`). Test the softbuffer path on Windows by temporarily swapping the cfgs and adding softbuffer/bytemuck deps (its Win32 backend reports age 1). `ui/clipboard.rs`: text clipboard connected on first copy/paste (Win32; smithay on Wayland; arboard without image-data on X11/macOS); egui-winit's clipboard feature is off everywhere. Linux glibc: `M_ARENA_MAX` 2 in `main`.
- `ui/app.rs`: all state. Spotify-style `Queue` (current, user `manual` queue first, then `context`; radio appended at the end). Back history of pages. `likes` overrides like status. `last-played` restores the track paused; its stream is prefetched only when the pointer hovers play. System fallback fonts are installed on first need.
- `ui/views.rs`: views return an `Action` applied afterwards; virtualized `track_list`; queue rows via `QueueRow`.
- `ui/widgets.rs`: custom widgets. `track_row`: title (plain), `byline` (Artist and Album links, plays), like button (hover only, filled if liked) left of a fixed-width duration slot.
- `ui/menus.rs`: Account/Equalizer/Pitch/Theme (with "Open themes folder")/Preferences (download folder) popups, Downloads button (opens the folder via `proc::open_folder`: explorer/open/xdg-open, nothing loaded in-process), context menus (Download, Delete owned playlists, Remove saved ones), delete modal. `ui/bar.rs`: now-playing bar.
- `pool.rs`: worker threads that exit after 5 idle s; used by `worker.rs` (API, one thread), album art (4), and the resolver/engine threads follow the same rule. Idle app: 6 threads.
- `worker.rs`: API requests (`Request`/`Response`).
- `api/`: InnerTube WEB_REMIX client, parser (searches leaf renderers), models (`Track`, `Playlist` with `kind`/`owned`/`save_id`, `Header`, `Artist`).
- `stream/`: native stream resolving: `player` endpoint + unscrambling in QuickJS (`solver.rs`, bytecode cached, built by the `--build-solver` helper process). A 403 rebuilds the solver. yt-dlp is intentionally not used. Only likely plays (hovered/pressed row, next in queue) get their first 128 KiB preloaded.
- `player/`: symphonia + cpal. Callback applies volume, `pitch.rs` (WSOLA + resampler, bypassed at 0, buffers allocated on first use) and `eq.rs`. `index.rs` reads the sidx: seeks/resumes splice the stream (head + target fragment) for one request. After 30 s paused the engine releases track and device (`PlayerEvent::Released`) and its thread exits. Downloaded songs play from the file (`load_file`, no index, no network); the app skips resolver hints for them.
- `download.rs`: one `download` pool thread. Songs: the AAC stream as-is, tags appended to moov/udta/meta/ilst (YouTube's files already have one with `©too`), written as `.part` then renamed, `Artist - Title [videoId].m4a`. Playlists/albums: `Title [id].m3u8` of those files; the page button and menu toggle (removing deletes the .m3u8 and its songs no other .m3u8 lists). The folder is the record (scanned at startup only if it exists; a list counts only if all its songs exist). Default folder `downloads` in the config dir, `[downloads] folder` in config.toml.
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
- Linux: `startup.sh` (same, plus threads and idle CPU once settled; `X11=1` forces X11/XWayland), `memtest.sh` (same steps and coordinates as memtest.ps1, plus CPU while playing visible/minimized; needs `xdotool`, so it runs the app on X11). Memory from `/proc`: "private" = smaps_rollup Private_Clean + Private_Dirty (closest to Task Manager's private working set), "anon" = RssAnon. CPU in ms per second, not cycles. Helpers in `proc.sh`. Built with `cargo build --release` on Linux (needs the GTK/WebKitGTK and ALSA dev packages for wry/tao and cpal).
- `launch.ps1` + `win.ps1 -Action shot|click|rclick|drag|move|type` to drive the window (client pixels, 1100x720).
- `yt.ps1 <endpoint> '<json>' <out>` authenticated InnerTube probe; `rows.js`, `menus.js`, `jq.js` summarize responses; `pxdiff.py` pixel diffs.
- `YTMCROSS_TRACE=1` writes timings to `trace.log` in the data dir.

## Baselines

Main exe 10.42 MB, login exe 0.72 MB. Process creation to main ~30 ms (OS; a trivial exe is the same), main to first frame ~34 ms (~20 ms of it is winit window creation). Idle 3.5 MB private, 6 threads. memtest settles ~8.5 MB. Playing ~13 Mcycles/s visible, ~9.7 minimized; true idle ~0. Seek ~30 ms. (Windows.)

Linux (Fedora 44, Hyprland 0.56, Ryzen AI 7 350, 2026-10-14): main exe 12.43 MB, login exe 0.79 MB. Wayland: launch to first frame ~21 ms wall (main to first frame ~12 ms, event loop ready ~4 ms); idle 11.0 MB private, 3.1 MB anon, 1 thread, 0 ms/s CPU once settled. X11 (XWayland): ~22-29 ms wall (event loop ready ~10-15 ms); settled 11.6 MB private, 3.6 MB anon, 1 thread, ~3 ms/s CPU. The first launch blocks on sign-in and then builds the solver, so measure from the second run on. memtest (X11, `WINIT_X11_SCALE_FACTOR=1`, window floated at 1100x720): 11.4 MB at start, 21.8 after four searches, settles ~25.8 MB private / 16.1 anon while playing; playing ~28 ms/s CPU. Hyprland has no minimize, so the "minimized" line equals the visible one. Search now shows an Albums shelf above Songs, so memtest's "track 1" click (y 150) opens an album and "track 2" plays from it.

## Data

Config, themes and downloads: `%APPDATA%\ytmcross`. `cookies.txt` (never print or publish), `last-played`, solver cache, `trace.log`: `%LOCALAPPDATA%\ytmcross`.
