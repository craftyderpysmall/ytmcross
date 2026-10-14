# Process measurements from /proc, shared by the Linux dev scripts (sourced).

# Private resident memory in MB (Private_Clean + Private_Dirty).
private_mb() {
    awk '/^Private_(Clean|Dirty):/ { kb += $2 } END { printf "%.1f", kb / 1024 }' "/proc/$1/smaps_rollup"
}

# Anonymous resident memory in MB.
anon_mb() {
    awk '/^RssAnon:/ { printf "%.1f", $2 / 1024 }' "/proc/$1/status"
}

threads() {
    awk '/^Threads:/ { print $2 }' "/proc/$1/status"
}

# CPU time (user + system) in clock ticks. The command name has no spaces.
cpu_ticks() {
    awk '{ print $14 + $15 }' "/proc/$1/stat"
}

# Milliseconds of CPU used per second, over $2 seconds.
cpu_ms_per_s() {
    local before after
    before=$(cpu_ticks "$1"); sleep "$2"; after=$(cpu_ticks "$1")
    awk -v a="$before" -v b="$after" -v hz="$(getconf CLK_TCK)" -v s="$2" 'BEGIN { printf "%.2f", (b - a) * 1000 / hz / s }'
}
