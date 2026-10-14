# ytmcross: notes for Claude

Native Rust YouTube Music client (no Electron, no browser at runtime). Read this before working on it.

## The user's priorities (apply to every task)

1. **Runtime performance first**: low RAM, near-zero idle CPU, fast startup, nothing running in the background.
2. **Minimal and clean**: no feature creep; remove unused code; small, readable modules.
3. **Disk size matters, but less** than RAM/CPU (trade disk for speed only when it's clearly worth it).
4. Every request ends with: keep looking for startup/storage/CPU/RAM wins and clean up unused code. Measure before and after (scripts in `dev/`), and report numbers.
5. When done: commit (no attribution or Co-Authored-By lines), and push to `origin` (github.com/craftyderpysmall/ytmcross, branch `master`) when the user asks.
6. The user uses the **scarf-dark** theme (white `text`, blue `accent`). Accent buttons and filled icons use the theme's `text` color, never `background`/`accent` for labels.

## Build (Windows, GNU toolchain via scoop)

Every PowerShell command that runs cargo needs:

```powershell
$env:Path = "$env:USERPROFILE\scoop\persist\rustup\.cargo\bin;$env:USERPROFILE\scoop\apps\mingw\current\bin;$env:Path"
$env:CARGO_HOME = "$env:USERPROFILE\scoop\persist\rustup\.cargo"
$env:RUSTUP_HOME = "$env:USERPROFILE\scoop\persist\rustup\.rustup"
```

- Toolchain override for this folder: `stable-x86_64-pc-windows-gnu`. Release build is ~2 min (fat LTO).
- Checks: `cargo clippy --all-targets` (keep it at zero warnings), `cargo test` (22 tests).
- Two binaries: `ytmcross` (the app) and `ytmcross-login` (`src/bin/`, the sign-in webview). The login exe needs `WebView2Loader.dll` next to it (copy from the webview2-com-sys `x64/` folder into `target/release` if missing). The main exe must never import it: check with `objdump -p target\release\ytmcross.exe | Select-String "DLL Name"`.
- `build.rs` deflates `assets/ejs/*.js` into the binary; `flate2` is already in the tree via png/ureq.

## Environment pitfalls

- Shell is Windows PowerShell 5.1. Many files have **CRLF** line endings: multi-line `.Replace()` in PowerShell silently fails on them. Prefer the Edit tool, or normalize with `.Replace("`r`n","`n")` first, and always verify the replacement happened.
- `git commit -m` with double quotes in the message breaks argument passing: write the message to a file and use `git commit -F <file>`.
- PS 5.1 `Invoke-WebRequest` drops a manual `Cookie` header: use `curl.exe` (see `dev/yt.ps1`).
- `Set-Content` without `-Encoding` writes ANSI; use `[IO.File]::WriteAllText(path, text, (New-Object Text.UTF8Encoding $false))`.

## Architecture (src/)

- `main.rs`: `--build-solver <ver>` helper dispatch, first-run sign-in, then `ui::run`.
- `ui/shell.rs`: winit loop, **CPU rendering** (egui_software_backend + softbuffer, `with_caching(false)`: measured faster and half the RAM). Draws only when something changed; nothing when hidden; frames paced to the refresh rate. Calls `App::on_exit` after the loop.
- `ui/app.rs`: all state. Spotify-style `Queue` (current, `manual` queue plays first, then `context` = album/playlist/radio, radio appended when it runs out). Page history (`Page` enum, Back keeps loaded pages, max 16). `likes` map overrides like status from responses. Resume: `last-played` file (plain text, one field per line) restores the track paused; its stream is prefetched only when the pointer hovers play (`warm_resume`), since doing it at startup cost 1.3 MB idle. Fallback fonts installed on first need (`response_needs_fallback`, search query check).
- `ui/views.rs`: views return an `Action`, applied afterwards (shared borrows while drawing). `track_list` is virtualized (`show_rows`); `rows` draws rows directly (artist page). Queue view maps rows via `QueueRow`. Views slide up while fading in (`VIEW_ENTER`).
- `ui/widgets.rs`: custom-painted widgets. `track_row` has: art, title (not clickable), `byline` (Artist and Album are links, then "N plays"), like button (thumbs-up drawn from convex polygons; visible on hover, and always when liked) in a fixed column left of a fixed-width duration slot.
- `ui/menus.rs`: Account/Equalizer/Pitch/Theme popups (`MENU_COUNT`), song and playlist context menus (Delete for owned playlists, "Remove from library" for saved ones; "Add to playlist" lists only owned ones), delete modal.
- `ui/bar.rs`: now-playing bar; title plain, `Artist • Album` links.
- `worker.rs`: the single API thread (`Request`/`Response`). Large variants are boxed (clippy).
- `api/`: InnerTube WEB_REMIX client (`client.rs`), JSON parsing by searching for leaf renderers (`parse.rs`), models (`Track`, `Playlist` with `kind` Playlist/Album, `owned`, `save_id`; `Header`; `Artist`).
- `stream/`: resolves video ids to stream URLs natively: `player` endpoint + unscrambling in QuickJS (`solver.rs`, bytecode cached in data dir; built by the `--build-solver` helper process with SWC, falling back to the embedded yt-dlp ejs scripts). Hints: Visible (64 KiB preload) / Likely (128 KiB). A 403 on preload invalidates and rebuilds the solver. **yt-dlp is not used anymore** (removed on purpose).
- `player/`: symphonia (AAC/MP4) + cpal. `output.rs` callback applies volume, `pitch.rs` (WSOLA + Catmull-Rom resampler, bypassed at 0, reset via `generation` atomic), `eq.rs` (5-band biquads). `source.rs` HTTP range source + preload cache.
- `theme.rs`: TOML themes merged over the built-in default; Inter fonts (subset, see the comment there); `system_fallbacks()` memory-maps OS fonts for CJK/Hangul/Indic/Thai/Arabic.
- `config.rs`: `config.toml` with line-based edits (`set_section`, `set_top_level_key`). Bundled themes are upgraded in place only if their FNV-1a hash (CR stripped) is in `OLD_BUNDLED_THEMES`. **When editing a bundled theme file, add the previous version's hash** (`node dev/fnv.js <file>`).

## YouTube Music API facts (verified with dev/yt.ps1 probes)

- Search filters: songs `EgWKAQIIAWoMEA4QChADEAQQCRAF`, albums `EgWKAQIYAWoMEA4QChADEAQQCRAF`.
- Rows: artist/album links are runs with `browseEndpoint` and pageType `MUSIC_PAGE_TYPE_ARTIST`/`_ALBUM`, in any flex column after the title; "N plays"/"N views" text is in a later column.
- Like status: playlist rows have `likeButtonRenderer.likeStatus`; search/up-next rows only offer a like action whose status is `INDIFFERENT` when already liked, `LIKE` when not. Like: `like/like` / `like/removelike` with `{target:{videoId}}`.
- Library: `FEmusic_liked_playlists`, `FEmusic_liked_albums`. Own playlists have `playlistEditorEndpoint` in their menu. Save/remove a playlist or album: `like/like` / `like/removelike` with `{target:{playlistId}}` (albums use their `OLAK5uy_…` playlist id).
- Album page: browse `MPREb_…`; header `musicResponsiveHeaderRenderer` (strapline = artist, save toggle `toggleButtonRenderer`); rows lack artist/art (filled from the header). Own playlists use `musicEditablePlaylistDetailHeaderRenderer`.
- Artist page: `musicImmersiveHeaderRenderer` (`monthlyListenerCount`), first `musicShelfRenderer` = top songs, carousels "Albums" and "Singles & EPs".
- The library listing lags behind edits: update lists locally (optimistic) instead of refetching.
- Signed out, every response says `logged_in=0`: only treat that as an expired session when a session exists.

## Measuring and testing (dev/)

- `dev/startup.ps1`: exe size, launch-to-first-frame, idle private memory (3 runs; move the cursor off the play button first).
- `dev/memtest.ps1`: memory over searches and playback (should plateau).
- `dev/launch.ps1` + `dev/win.ps1` (`-Action shot|click|rclick|drag|move|type|wheel`): drive the window for screenshot checks. Coordinates are client pixels; window 1100x720; sidebar menus at x~50.
- `dev/yt.ps1 <endpoint> '<json>' <out>`: authenticated InnerTube probe using cookies.txt; `rows.js`, `menus.js`, `jq.js` summarize responses.
- `dev/pxdiff.py`: pixel diff of screenshots (used to prove the font subset renders identically).
- `YTMCROSS_TRACE=1` logs timings to `trace.log` in the data dir; `YTMCROSS_STATS=1` shows ms/frame in the title.

## Baselines (latest)

Main exe 10.71 MB, login exe 0.68 MB. First frame ~40 ms after main (wall ~80 ms). Idle 7.8–8.0 MB private, ~17.4 MB while playing (plateaus). Idle CPU 0. Playing ~1.7% of one core; pitch adds ~0.6%.

## Data locations (Windows)

Config + themes: `%APPDATA%\ytmcross`. Session (`cookies.txt`), `last-played`, `signed-out` marker, solver cache, trace.log: `%LOCALAPPDATA%\ytmcross`. Never print or publish cookies.txt.

## Ideas not done yet

- Unhinted Inter subset would save another ~170 KB, but changes ~5k pixels of rendering (rejected for now).
- Dirty-rect presenting (only the bar changes during playback) could cut the 1 Hz clock repaint cost.
- Artist pages show YouTube's first page only (no "see all").
