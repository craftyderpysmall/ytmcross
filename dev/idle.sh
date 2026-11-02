#!/bin/sh
# Is the app truly idle? CPU ticks and context switches of a pid over N seconds.
# idle.sh <pid> [seconds]
pid=$1; secs=${2:-10}
ticks() { cut -d' ' -f14,15 "/proc/$pid/stat" | awk '{print $1 + $2}'; }
switches() { cat /proc/"$pid"/task/*/status | awk '/ctxt_switches/ {n += $2} END {print n}'; }
t0=$(ticks); s0=$(switches)
sleep "$secs"
t1=$(ticks); s1=$(switches)
echo "over ${secs}s: $((t1 - t0)) CPU ticks (10 ms each), $((s1 - s0)) context switches"
