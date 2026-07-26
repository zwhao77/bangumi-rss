#!/bin/bash
# Thread monitor — stress-test tiny_http with slow endpoint,
# record thread count timeline in real-time.

set -e

BINARY="./target/release/bangumi-rss"
PORT="${PORT:-59973}"
SLOW_EP="http://127.0.0.1:${PORT}/api/slow"
CONCURRENT="${CONCURRENT:-50}"
OUTFILE="/tmp/thread_monitor_$$.csv"
TMPDIR="/tmp/bgtest$$"

echo "=== bangumi-rss thread monitor ==="
echo "port=$PORT  concurrent=$CONCURRENT"
echo "output: $OUTFILE"

cleanup() {
    echo "--- cleanup ---"
    pkill -9 -f "$BINARY" 2>/dev/null || true
    pkill -9 -f "curl.*api/slow" 2>/dev/null || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

mkdir -p "$TMPDIR"/{dl,lib,data}

# ── Start server ──
PORT="$PORT" DOWNLOAD_DIR="$TMPDIR/dl" \
    LIBRARY_DIR="$TMPDIR/lib" DATA_DIR="$TMPDIR/data" \
    "$BINARY" > /dev/null 2>&1 &
sleep 3

PID=$(pgrep -f "$BINARY" | head -1)
if [ -z "$PID" ]; then
    echo "ERROR: server failed to start"
    exit 1
fi
echo "PID=$PID"

# ── CSV header ──
echo "timestamp_ms,threads" > "$OUTFILE"

# ── Phase 1: idle baseline (2s) ──
echo -n "idle:  "
for i in $(seq 1 10); do
    ts=$(date +%s%3N)
    t=$(awk '/^Threads:/ {print $2}' /proc/$PID/status)
    echo "$ts,$t" >> "$OUTFILE"
    echo -n "."
    sleep 0.2
done
T_IDLE=$(tail -1 "$OUTFILE" | cut -d, -f2)
echo " idle=$T_IDLE"

# ── Phase 2: burst concurrent slow requests ──
echo "firing $CONCURRENT concurrent slow (2s each)"
for i in $(seq 1 $CONCURRENT); do
    curl -s --max-time 10 "$SLOW_EP" > /dev/null 2>&1 &
done

# ── Phase 3: monitor during load (10s, 200ms interval) ──
echo -n "load:  "
for i in $(seq 1 50); do
    ts=$(date +%s%3N)
    t=$(awk '/^Threads:/ {print $2}' /proc/$PID/status)
    echo "$ts,$t" >> "$OUTFILE"
    echo -n "."
    sleep 0.2
done
echo ""

# ── Analysis ──
echo ""
echo "=== Results ==="
BASE=$(head -2 "$OUTFILE" | tail -1 | cut -d, -f2)
MAX=$(cut -d, -f2 "$OUTFILE" | sort -n | tail -1)
echo "  baseline threads: $BASE"
echo "  peak threads:     $MAX"
echo "  delta:            $((MAX - BASE))"
echo "  full log:         $OUTFILE"
