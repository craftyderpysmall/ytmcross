#!/usr/bin/env bash
# Linux counterpart of memtest.ps1: does memory plateau over several
# searches and plays, then idle? Same steps and window coordinates (client
# pixels of the default 1100x720 window, at scale 1), plus CPU while
# playing, visible and minimized.
#
# Needs xdotool, which drives X11 windows only: the app is run on X11
# (XWayland in a Wayland session). Columns as in startup.sh.
set -u
here=$(cd "$(dirname "$0")" && pwd)
. "$here/proc.sh"
exe="$here/../target/release/ytmcross"
command -v xdotool >/dev/null || { echo "needs xdotool" >&2; exit 1; }
[ -x "$exe" ] || { echo "build first: cargo build --release" >&2; exit 1; }
unset WAYLAND_DISPLAY YTMCROSS_TRACE

pkill -x ytmcross; sleep 0.3
"$exe" >/dev/null 2>&1 &
pid=$!
win=$(xdotool search --sync --onlyvisible --pid "$pid" | head -n 1)

mem() { printf '%-28s %5s MB private, %5s MB anon, %2s threads\n' "$1" "$(private_mb $pid)" "$(anon_mb $pid)" "$(threads $pid)"; }
# Client-area coordinates; a short press, like win.ps1.
click() {
    xdotool windowactivate --sync "$win" mousemove --window "$win" "$1" "$2" mousedown 1
    sleep "${3:-0.05}"
    xdotool mouseup 1
}

sleep 5; mem "startup"
for q in 'daft punk' 'radiohead' 'kendrick lamar' 'taylor swift'; do
    click 600 82
    xdotool key --window "$win" ctrl+a
    xdotool type --window "$win" --delay 20 "$q"
    xdotool key --window "$win" Return
    sleep 5; mem "after search '$q'"
done
click 600 150 0.2; sleep 6; mem "playing track 1"
click 600 278 0.2; sleep 6; mem "playing track 2"
for t in 1 2 3 4; do sleep 15; mem "idle +$((t * 15))s (playing)"; done
echo "CPU playing, visible:   $(cpu_ms_per_s $pid 10) ms/s"
xdotool windowminimize --sync "$win"
echo "CPU playing, minimized: $(cpu_ms_per_s $pid 10) ms/s"
xdotool windowactivate --sync "$win"; sleep 1
click 625 668; sleep 3; mem "paused"
kill "$pid"
