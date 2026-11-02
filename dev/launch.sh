#!/bin/sh
# Restart ytmcross, tiled like any other window, and focus it. Prints its pid. Extra
# environment passes through (XDG_* for an isolated run, YTMCROSS_TRACE, ...).
exe="$(dirname "$0")/../target/release/ytmcross"
pkill -x ytmcross
sleep 0.3
# stderr is kept: with panic = abort, it is all a crash leaves behind.
"$exe" >/dev/null 2>>"${TMPDIR:-/tmp}/ytmcross.err" &
pid=$!
sleep 1
hyprctl -q dispatch "hl.dsp.focus({ window = \"pid:$pid\" })"
sleep 0.3
echo "$pid"
