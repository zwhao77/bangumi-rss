# bangumi-rss

Anime RSS auto-downloader. Subscribe to RSS feeds, auto-download via aria2/qBittorrent, organize into media library.

Single Rust binary, ~5 MB memory footprint.

## Quick Start

```bash
# 1. Clone
git clone https://github.com/zwhao77/bangumi-rss.git
cd bangumi-rss

# 2. Start aria2 with JSON-RPC enabled
aria2c --enable-rpc --rpc-listen-all --rpc-allow-origin-all &

# 3. Build (res/index.html and res/style.css are embedded at compile time)
cargo build --release

# 4. Run (downloads go to /downloads, media to /anime)
ARIA2_RPC_URL=http://localhost:6800/jsonrpc \
DOWNLOAD_DIR=/downloads \
LIBRARY_DIR=/anime \
./target/release/bangumi-rss
```

Open `http://localhost:7893` in browser to subscribe and manage.

## Resource Usage

| Metric | Typical |
|--------|---------|
| Binary size | ~7.4 MB (release) |
| RSS (physical memory) | ~5 MB idle, ~5 MB under load |
| CPU | ~0% idle, negligible under request |
| State persistence | < 100 µs per write |
| State file size | ~50 KB even after months with dozens of feeds |

## Configuration

| Variable | Default | Description |
|---|---|---|
| `PORT` | `7893` | HTTP API port |
| `NO_SERVER` | `false` | Disable HTTP server |
| `DATA_DIR` | `.` | State file directory (`state.json`) |
| `RSS_INTERVAL` | `900` | RSS poll interval (seconds) |
| `ARIA2_RPC_URL` | `http://localhost:6800/jsonrpc` | Aria2 JSON-RPC endpoint |
| `DOWNLOAD_DIR` | `/downloads` | Torrent staging directory |
| `LIBRARY_DIR` | `/anime` | Media library output directory |
| `DOWNLOADER` | `aria2` | `aria2` or `qbittorrent` |
| `MOCK_DOWNLOADER` | `false` | Enable in-memory mock downloader (testing) |
| `QBITTORRENT_URL` | `http://localhost:8080` | qBittorrent Web UI base URL |
| `QBITTORRENT_USER` | `admin` | qBittorrent username |
| `QBITTORRENT_PASS` | `adminadmin` | qBittorrent password |
| `BANGUMI_API_BASE` | `https://api.bgm.tv` | Bangumi API base URL |
| `MAX_CONCURRENCY` | `8` | Max concurrent HTTP requests |
| `RUST_LOG` | `info` | Log level (`warn` to quieten, `debug` for verbose) |

## Architecture

**TEA** (The Elm Architecture): `Event → State → Effect`

```
Event Sources (timers, HTTP)
       │  Event
       ▼
  logic::reduce(&state, event) → (new_state, Vec<Effect>)
       │                              │
       ▼                              ▼
  AppState.save()              EffectExecutor.run()
  (if dirty)                   (I/O: HTTP, FS, downloader RPC)
                                    │
                                    ▼
                              Feedback Events → event_tx
```

- **`logic::reduce` is pure** — no I/O, no side effects, fully testable.
- **4 threads**: timers, HTTP server, executor, logic.
- **State persisted atomically** via `state.tmp` + `rename`. Synchronous write on each state change (~50 KB → < 100 µs).

## Frontend

Web UI is served at `/`. It consists of two source files:
- `res/index.html` — HTML structure + embedded JavaScript
- `res/style.css` — styling

Both are compiled into the binary via `include_str!` — no external files needed at runtime. To customize the UI, place `res/index.html` and `res/style.css` next to the binary; they take priority over the embedded versions and can be edited without recompilation.

## Deployment

```bash
docker build -t bangumi-rss .
docker run -p 7893:7893 -v /path/to/downloads:/downloads -v /path/to/anime:/anime bangumi-rss
```

Or just copy the single binary:

```bash
cp target/release/bangumi-rss /usr/local/bin/
bangumi-rss
```

## License

MIT
