#!/bin/sh
set -e

# Start aria2 in background
aria2c \
    --enable-rpc \
    --rpc-listen-all=true \
    --rpc-listen-port=6800 \
    --dir=/downloads \
    --seed-time=0 \
    --max-connection-per-server=4 \
    --min-split-size=10M \
    --console-log-level=warn \
    &

echo "[entrypoint] aria2 started"

# Wait for aria2 to be ready
for i in $(seq 1 10); do
    if wget -qO- http://localhost:6800/jsonrpc 2>/dev/null | grep -q aria2; then
        echo "[entrypoint] aria2 ready"
        break
    fi
    sleep 1
done

# Start bangumi-rss (foreground)
exec bangumi-rss
