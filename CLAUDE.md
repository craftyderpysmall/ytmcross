# ytmcross: notes for Claude

Native Rust YouTube Music client for Linux on Wayland (no Electron, no browser at runtime). Linux and Wayland only: no cfgs or code paths for other OSes or X11.

## Current goal

Convert this project from Windows to Linux. The app builds and plays on Hyprland; Windows code and tooling are gone and must not come back. Still unverified on Linux: audio quality, clipboard paste, Library rendering, downloads, window resizing, non-Latin titles, `makepkg`, the release workflow, and long-session memory (no `memtest` port yet).

## User priorities (every task)

1. Runtime performance first: low RAM, zero idle CPU, fast startup, nothing in the background.
2. Minimal and clean: no feature creep, remove unused code.
3. Disk size matters, but less than RAM/CPU.
4. After each request, keep looking for startup/storage/CPU/RAM wins and unused code. Measure before and after with `dev/` scripts and report numbers.
5. ONLY when explicitly asked: commit, and push to `origin master` (github.com/craftyderpysmall/ytmcross).

## Build

Rust via rustup, plus gcc, `alsa-lib` and `dbus` (runtime libraries and headers). On Arch these are all in the main packages.

```sh
cargo build --release
```

- Release build ~3.5 min from clean, ~1.5 min incremental (fat LTO).
- Keep `cargo clippy --all-targets` at zero warnings; `cargo test` must pass.
- One binary, with two helper modes: `--build-solver <version>` and `--login`. It must never link GTK or WebKit (`ldd target/release/ytmcross`).
- `build.rs` only deflates `assets/ejs/*.js` into the binary. Window icon and name come from `assets/ytmcross.desktop` through app id `ytmcross`.
- Per-crate `opt-level = "s"` in Cargo.toml is only for helper-only or cold crates; measure CPU before adding more.

## Releases

- Bump `version` in Cargo.toml and `pkgver` in `packaging/PKGBUILD`, commit, push tag `vX.Y.Z`. `.github/workflows/release.yml` builds on the oldest Ubuntu runner (for glibc compatibility) and publishes `ytmcross-vX.Y.Z-linux-x64.tar.gz` (binary, `.desktop`, icon, license).
- Nothing is published to the AUR automatically.

## Pitfalls

- Editing a bundled theme (`assets/themes/*.toml`): add the previous version's hash (`node dev/fnv.js <file>`) to `OLD_BUNDLED_THEMES` in `config.rs`, so unedited user copies upgrade.
- Theme colors: labels on accent buttons and filled icons use `text`, never `background`.
- Never send keys while ytmcross lacks focus: `dev/win.sh` refuses, because `wtype` types into whatever window is focused.
- `proc::helper` uses `PR_SET_PDEATHSIG`, which fires when the spawning *thread* exits: that thread must wait for the child.
- `window.pre_present_notify()` only on frames that commit (present.rs does this): a callback requested without a commit never fires and redraws stall.
- A Wayland buffer must be complete. After `Presenter::release` the next frame is a full redraw.
- Google refuses sign-in in webviews and in browsers with a debugger attached (`--remote-debugging-*`, tested). Don't "simplify" `--login` into either.

## Architecture (src/)

- `ui/shell.rs`: winit loop, CPU rendering (egui_software_backend, caching off). Draws only on change; frames are paced by frame callbacks, so a window out of view gets none and draws nothing (winit holds the redraw back; `Occluded` doesn't exist on Wayland). `YTMCROSS_STATS=1` shows per-phase frame times in the title.
- `ui/damage.rs`: hashes every shape per frame; only changed regions are tessellated, rasterized and blitted. Partial frames must stay pixel-identical to full ones (check with screenshots + `pxdiff.py` after a forced full redraw: minimize/restore).
- `ui/present.rs`: presents through wl_shm (memfd), on a client of winit's own display, with one buffer reused once the compositor releases it (a spare only if it hasn't). Freed 3 s after the last draw (`KEEP_BUFFER`), so the 1 Hz progress update stays partial. `ui/clipboard.rs`: smithay-clipboard, connected on the first copy or paste; egui-winit's clipboard feature is off.
- `ui/app.rs`: all state. Spotify-style `Queue` (current, user `manual` queue first, then `context`; radio appended at the end). Back history of pages. `likes` overrides like status. `last-played` restores the track paused; its stream is prefetched only when the pointer hovers play. System fallback fonts are installed on first need.
- `ui/views.rs`: views return an `Action` applied afterwards; virtualized `track_list`; queue rows via `QueueRow`.
- `ui/widgets.rs`: custom widgets. `track_row`: title (plain), `byline` (a link per artist via `Track::artist_links` byte ranges into `artists`, Album link, plays), like button (hover only, filled if liked) left of a fixed-width duration slot.
- `ui/menus.rs`: Account/Equalizer/Pitch/Preferences (theme with "Open themes folder", download folder, shortcuts) popups, Downloads button (opens the folder with `xdg-open` via `proc::open_folder`), context menus (Download, Delete owned playlists, Remove saved ones), delete modal. `ui/bar.rs`: now-playing bar. `[keys]` shortcuts are in-window (`take_key` in app.rs, ahead of Alt+Left = back). `config::Bind` parses/prints "Ctrl+Shift+Left" with egui key names.
- `mpris.rs`: `org.mpris.MediaPlayer2.ytmcross` over libdbus. One thread asleep in poll, started by the first playback (`Shell::publish`), so a restored paused track isn't controllable from outside until played. Commands arrive as `Wake::Control` → `App::control`. Read-only: `CanSeek` and `CanRaise` are false, Volume can't be set.
- Naming: the package, binary, paths and app id stay `ytmcross`; everything users see (window title, sidebar title, `.desktop` name, MPRIS Identity) says "YouTube Music".
- `pool.rs`: worker threads that exit after 5 idle s; used by `worker.rs` (API, one thread), album art (4), and the resolver/engine threads follow the same rule. Idle app: 1 thread.
- `worker.rs`: API requests (`Request`/`Response`). `Notifier` wakes the event loop directly (shell's `Waker`), never via `ctx.request_repaint()`: egui drops requests while one is pending and only a frame clears that, so a hidden window would stop advancing the queue.
- `auth/login.rs` + `auth/cookiedb.rs`: `--login` runs the user's browser untouched on a throwaway profile with `--password-store=basic`, watches its cookie database (our small SQLite reader; Chrome relaunches itself, so aliveness is `SingletonLock`, not the child), closes it, decrypts only the YouTube cookies (`v10`, fixed key, 32-byte host hash from schema 24) into `cookies.txt` (0600), deletes the profile.
- `proc.rs`: helper processes (killed with us via `PR_SET_PDEATHSIG`), and glibc heap tuning: one arena (`tune_heap`), `malloc_trim` after each solver (`trim_heap`).
- `api/`: InnerTube WEB_REMIX client, parser (searches leaf renderers), models (`Track`, `Playlist` with `kind`/`owned`/`save_id`, `Header`, `Artist`).
- `stream/`: native stream resolving: `player` endpoint + unscrambling in QuickJS (`solver.rs`, bytecode cached, built by the `--build-solver` helper process; the heap trimmed after each runtime). A 403 rebuilds the solver. yt-dlp is intentionally not used. Only likely plays (hovered/pressed row, next in queue) get their first 128 KiB preloaded.
- `player/`: symphonia + cpal. Callback applies volume, `pitch.rs` (WSOLA + resampler, bypassed at 0, buffers allocated on first use) and `eq.rs`. `index.rs` reads the sidx: seeks/resumes splice the stream (head + target fragment) for one request. After 30 s paused the engine releases track and device (`PlayerEvent::Released`) and its thread exits. Downloaded songs play from the file (`load_file`, no index, no network); the app skips resolver hints for them.
- `download.rs`: one `download` pool thread. Songs: the AAC stream as-is, tags appended to moov/udta/meta/ilst (YouTube's files already have one with `©too`), written as `.part` then renamed, `Artist - Title [videoId].m4a`. Playlists/albums: `Title [id].m3u8` of those files; the page button and menu toggle (removing deletes the .m3u8 and its songs no other .m3u8 lists). The folder is the record (scanned at startup only if it exists; a list counts only if all its songs exist). Default folder `downloads` in the data dir, `[downloads] folder` in config.toml.
- `theme.rs`: themes, subset Inter fonts, memory-mapped system fallback fonts (picked with `fc-match` on first need). `config.rs`: line-based `config.toml` edits.

## YouTube Music API facts

- Search filters: songs `EgWKAQIIAWoMEA4QChADEAQQCRAF`, albums `EgWKAQIYAWoMEA4QChADEAQQCRAF`.
- Artist/album links are runs with `browseEndpoint` pageType `MUSIC_PAGE_TYPE_ARTIST`/`_ALBUM` in any column after the title.
- Like status: `likeButtonRenderer.likeStatus`, or a like action with status `INDIFFERENT` (already liked) / `LIKE`. Like: `like/like` / `like/removelike` with `{target:{videoId}}`; save/remove playlist or album with `{target:{playlistId}}` (albums use their `OLAK5uy_` id).
- Library: `FEmusic_liked_playlists`, `FEmusic_liked_albums`. Own playlists have `playlistEditorEndpoint` in their menu. The listing lags behind edits: update locally.
- Album page `MPREb_…`: header `musicResponsiveHeaderRenderer`; rows lack artist/art. Artist page: `musicImmersiveHeaderRenderer`, first `musicShelfRenderer` = top songs, carousels "Albums" / "Singles & EPs".
- Signed out, responses say `logged_in=0`; only treat it as expiry when a session exists.

## dev/ scripts

- `startup.sh` (size, first frame, idle memory; isolated XDG folders, signed out), `idle.sh <pid> [s]` (CPU ticks, context switches).
- `launch.sh` + `win.sh shot|click|rclick|move|type|key` to drive the window on Hyprland (grim, wtype; clicks need ydotool). The window stays tiled, so its size varies: never force it to float.
- `yt.sh <endpoint> '<json>' <out>` authenticated InnerTube probe; `rows.js`, `menus.js`, `jq.js` summarize responses (need node); `pxdiff.py` pixel diffs; `cookiedb-fixture.py` rebuilds the test database.
- `YTMCROSS_TRACE=1` writes timings to `trace.log` in the data dir.

## Baselines

Binary 12.0 MB. Main to first frame 10-39 ms (on Hyprland, depends on window size and first run). Idle signed out: 2.3 MB private (RssAnon), no frame buffer, 1 thread, 0 CPU ticks. Steady playing, 30 s in: 7.4 MB private + 7.5 MB frame buffer (RssShmem, 1920x1080), 7 threads, ~2% of a core. Paused 30 s+: 4.6 MB, 3 threads, 0 CPU ticks. `RssShmem` counts until 3 s after the last draw. No Linux memtest yet, and Windows numbers aren't comparable.

## Data

XDG: `~/.config/ytmcross` config, themes; `~/.local/share/ytmcross` (0700) `cookies.txt` (never print or publish), `last-played`, downloads, `trace.log`; `~/.cache/ytmcross` solver cache, the login profile while signing in.
