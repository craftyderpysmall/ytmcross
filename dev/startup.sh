#!/usr/bin/env bash
# Linux counterpart of startup.ps1: exe size, launch -> first frame (from
# the trace log), and memory/threads/CPU once idle. Three runs.
#
# Memory columns:
#   private: Private_Clean + Private_Dirty from smaps_rollup, the closest
#            match to Task Manager's private working set used on Windows;
#   anon:    RssAnon (heap and stacks only).
# CPU: milliseconds of CPU per second over 10 s of idling (0 is the goal).
#
# X11=1 ./dev/startup.sh runs the app on X11 (XWayland) even in a Wayland
# session, to compare the two backends.
set -u
here=$(cd "$(dirname "$0")" && pwd)
. "$here/proc.sh"
exe="$here/../target/release/ytmcross"
log="${XDG_DATA_HOME:-$HOME/.local/share}/ytmcross/trace.log"
[ -x "$exe" ] || { echo "build first: cargo build --release" >&2; exit 1; }
[ "${X11:-0}" = 1 ] && unset WAYLAND_DISPLAY
export YTMCROSS_TRACE=1

echo "exe size: $(awk -v b="$(stat -c %s "$exe")" 'BEGIN { printf "%.2f", b / 1048576 }') MB"
if [ -n "${WAYLAND_DISPLAY:-}" ]; then echo "backend: wayland"; else echo "backend: x11"; fi
for i in 1 2 3; do
    pkill -x ytmcross; sleep 0.3
    rm -f "$log"
    start=$(date +%s%N)
    "$exe" >/dev/null 2>&1 &
    pid=$!
    until grep -q 'first frame' "$log" 2>/dev/null || [ $(( ($(date +%s%N) - start) / 1000000 )) -gt 10000 ]; do
        sleep 0.005
    done
    wall=$(( ($(date +%s%N) - start) / 1000000 ))
    sleep 4
    trace=$(grep -E '\[main\] (main: start|shell:)' "$log" | sed -E 's/^ *//; s/ +\[main\] +/ /' | paste -sd ';' | sed 's/;/ ; /g')
    echo "run $i: launch->first frame (wall) ${wall} ms | idle $(private_mb $pid) MB private, $(anon_mb $pid) MB anon, $(threads $pid) threads | trace: $trace"
done
# The last run, once its startup workers have exited (they idle out after 5 s).
sleep 8
echo "settled: $(private_mb $pid) MB private, $(anon_mb $pid) MB anon, $(threads $pid) threads, CPU $(cpu_ms_per_s $pid 10) ms/s"
kill "$pid"
