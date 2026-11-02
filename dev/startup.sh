#!/bin/sh
# Size, first frame (trace) and idle memory, over 3 runs, in throwaway XDG
# folders, signed out: this measures the app, not the account.
exe="$(dirname "$0")/../target/release/ytmcross"
tmp=$(mktemp -d)
trap 'pkill -x ytmcross; rm -rf "$tmp"' EXIT
mkdir -p "$tmp/data/ytmcross"
touch "$tmp/data/ytmcross/signed-out"
export XDG_DATA_HOME="$tmp/data" XDG_CONFIG_HOME="$tmp/config" XDG_CACHE_HOME="$tmp/cache" YTMCROSS_TRACE=1
printf 'binary: %s bytes\n' "$(stat -c %s "$exe")"
for run in 1 2 3; do
    pkill -x ytmcross; sleep 0.3
    rm -f "$tmp/data/ytmcross/trace.log"
    "$exe" >/dev/null 2>&1 &
    pid=$!
    sleep 9 # past the frame buffer release and the pools' idle exit
    first=$(grep -o 'first frame.*' "$tmp/data/ytmcross/trace.log")
    start=$(awk '/first frame/ {print $1; exit}' "$tmp/data/ytmcross/trace.log")
    mem=$(awk '/RssAnon|RssShmem|Threads/ {printf "%s %s  ", $1, $2}' "/proc/$pid/status")
    echo "run $run: main->frame ${start} ms; idle $mem"
    echo "        $first"
done
