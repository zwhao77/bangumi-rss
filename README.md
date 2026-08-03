# bangumi-rss

Anime RSS auto-downloader. Subscribe to RSS feeds, auto-download via aria2/qBittorrent, organize into media library.

Single Rust binary, ~5 MB memory footprint.

## Quick Start

```bash
# 1. Clone
git clone https://github.com/zwhao77/bangumi-rss.git
cd bangumi-rss

# 2. Build (res/index.html and res/style.css are embedded at compile time)
cargo build --release
```

### 3. Start one download backend (see [Downloader Selection](#downloader-selection))

```bash
# Transmission — recommended for BitTorrent
transmission-daemon --rpc-port 9091 &

# qBittorrent — headless, Web UI on :8080. NOTE: on first start, 5.2+
# generates a TEMPORARY password instead of a default one; read it from the
# startup log, log in via the Web UI to set your own, then use it below.
qbittorrent-nox --webui-port=8080 --confirm-legal-notice &

# aria2 — lightweight, no daemon
aria2c --enable-rpc --rpc-listen-all --rpc-allow-origin-all &
```

### 4. Run, matching your backend

```bash
# Transmission
DOWNLOADER=transmission \
TRANSMISSION_RPC_URL=http://localhost:9091/transmission/rpc \
DOWNLOAD_DIR=/downloads LIBRARY_DIR=/anime ./target/release/bangumi-rss

# qBittorrent (use the password you set, or the temporary one from the log)
DOWNLOADER=qbittorrent \
QBITTORRENT_URL=http://localhost:8080 \
QBITTORRENT_USER=admin QBITTORRENT_PASS=YOUR_PASSWORD \
DOWNLOAD_DIR=/downloads LIBRARY_DIR=/anime ./target/release/bangumi-rss

# aria2
DOWNLOADER=aria2 \
ARIA2_RPC_URL=http://localhost:6800/jsonrpc \
DOWNLOAD_DIR=/downloads LIBRARY_DIR=/anime ./target/release/bangumi-rss
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
| `POLL_INTERVAL` | `30` | Download status poll interval (seconds) |
| `ARIA2_RPC_URL` | `http://localhost:6800/jsonrpc` | Aria2 JSON-RPC endpoint |
| `ARIA2_RPC_TOKEN` | — | Aria2 RPC secret (`--rpc-secret`) |
| `DOWNLOAD_DIR` | `/downloads` | Torrent staging directory |
| `LIBRARY_DIR` | `/anime` | Media library output directory |
| `DOWNLOADER` | `aria2` | `aria2`, `qbittorrent`, or `transmission` |
| `MOCK_DOWNLOADER` | `false` | Enable in-memory mock downloader (testing) |
| `QBITTORRENT_URL` | `http://localhost:8080` | qBittorrent Web UI base URL |
| `QBITTORRENT_USER` | `admin` | qBittorrent username |
| `QBITTORRENT_PASS` | `adminadmin` | qBittorrent password (5.2+ first start uses a temporary password — set your own) |
| `TRANSMISSION_RPC_URL` | `http://localhost:9091/transmission/rpc` | Transmission RPC endpoint |
| `TRANSMISSION_USER` | — | Transmission HTTP Basic Auth username |
| `TRANSMISSION_PASS` | — | Transmission HTTP Basic Auth password |
| `BANGUMI_API_BASE` | `https://api.bgm.tv` | Bangumi API base URL |
| `TORRENT_CONCURRENCY` | `4` | Worker pool threads (RSS + torrent downloads) |
| `QUEUE_CAPACITY` | `512` | Worker pool job queue capacity |
| `BIND_ADDR` | `127.0.0.1` | HTTP server bind address (`0.0.0.0` for all interfaces) |
| `MAX_CONNECTIONS` | `16` | Rouille thread-pool connection count |
| `AUTH_USERNAME` | — | Basic Auth username (empty = no auth) |
| `AUTH_PASSWORD` | — | Basic Auth password |
| `RUST_LOG` | `info` | Log level (`warn` to quieten, `debug` for verbose) |
| `WEBHOOK_URL` | — | Webhook URL (e.g. `http://gotify:8080/message?token=xxx`) |
| `WEBHOOK_FORMAT` | — | Preset name: `bark`, `gotify`, or `serverchan` |
| `WEBHOOK_TEMPLATE` | — | Custom JSON/Form template (overrides preset) |
| `WEBHOOK_ERROR_TEMPLATE` | — | Custom error template (overrides default error format) |

## Downloader Selection

| Downloader | rename_file | move_files | Seeding Retention | Notes |
|---|---|---|---|---|
| **Transmission** (4.1.0+) | ✅ `torrent_rename_path` | ✅ `torrent_set_location` | ✅ | Lightweight, simple, well-documented JSON-RPC 2.0, official OpenWrt package |
| **qBittorrent** (5.0+) | ✅ `torrents/renameFile` | ✅ `torrents/setLocation` | ✅ | Feature-rich; peer/connection optimizations (incl. end-game) usually faster on modern networks |
| **aria2** | ❌ no BT multi-file rename | ❌ no built-in move API | ❌ stops immediately after completion | Lightweight direct downloads, or fallback |

> **API-based support**: bangumi-rss drives each downloader through its HTTP API behind a single trait — any client with a compatible API works (e.g. qBittorrent Enhanced Edition uses the same `/api/v2/*` as upstream). There is no standard cross-client downloader protocol; every client exposes its own API.
> **Auth note**: qBittorrent requires ≥ 5.0 — 5.0+ authenticates with username/password (HTTP Basic); 4.x uses cookie (SID) login, which is not supported. ≥ 5.2 also supports an API key (Bearer).

### aria2 BitTorrent Limitations

aria2 has the following known limitations with BitTorrent torrents:

1. **No torrent file rename**: `changeOption("out")` only works for single-file HTTP downloads, not BT multi-file torrents. bangumi-rss falls back to filesystem operations (`fs.move_file`) when aria2 is detected.
2. **No downloader-aware move**: aria2 has no equivalent of `torrent_set_location` or `setLocation`. After moving files, the downloader is unaware of the new location.
3. **Seeding interrupted**: Due to the above limitations, aria2 torrents are stopped and removed from the list after completion, then moved via the filesystem. This **violates BT sharing etiquette** — if you want to keep seeding, use Transmission or qBittorrent.

> **Which to choose?** bangumi-rss doesn't push a single downloader — pick what fits your environment:
> - **qBittorrent** is often the better default on modern networks: feature-rich, and its peer/connection handling (including end-game last-piece optimization) tends to be faster. The cost is higher memory usage.
> - **Transmission** suits low-memory devices (routers / NAS): simple and well-documented. It's less feature-rich and may be slower in some peer/end-game scenarios.
> - **aria2** is best for direct (HTTP) downloads; for BitTorrent it lacks rename/move APIs and stops seeding after completion.

## Torrent Filtering

Each feed can carry a title filter, applied to RSS items before download. Useful
when the same episode is released by multiple groups and you only want some of
them — there is no episode-level dedup by design.

```json
{
  "url": "https://mikanani.me/RSS/Classic/...",
  "name": "示例番剧",
  "season": 1,
  "filter": {
    "include": ["SubA", "SubB"],
    "exclude": ["720p", "sample"],
    "regex": "(?i)^\\[ANi\\].*1080P"
  }
}
```

- `include` (optional): whitelist words — non-empty → the title must contain
  **all** words (ANDed, like qBittorrent's Must Contain; case-insensitive
  substrings, no regex escaping needed). Use `regex` with `|` for OR.
- `exclude` (optional): blacklist words — the title containing any word is skipped.
- `regex` (optional): advanced escape hatch — when set, the title must match this
  pure Rust regex (no lookaround/backreferences; `(?i)` works).
- All fields optional; an empty filter accepts everything. Send the same object
  via `PUT /api/feeds/{id}` to update; omitting `filter` keeps the current one.

## Notifications

bangumi-rss sends webhook notifications on download completions and failures.

### Quick Setup (Gotify)

```bash
# Deploy Gotify
docker run -d -p 7894:80 gotify/server

# Create an application in Gotify Web UI, copy its token
# Start bangumi-rss with webhook
WEBHOOK_URL=http://localhost:7894/message?token=YOUR_TOKEN \
WEBHOOK_FORMAT=gotify \
./bangumi-rss
```

### Presets

| Preset | WEBHOOK_FORMAT | Expected URL |
|--------|---------------|--------------|
| [Gotify](https://gotify.net) | `gotify` | `http://host:port/message?token=xxx` |
| [Bark](https://github.com/Finb/Bark) | `bark` | `http://host:port/your-key` |
| [Server酱](https://sct.ftqq.com) | `serverchan` | `https://sctapi.ftqq.com/SCTxxx.send` |

### Custom Templates

Use `WEBHOOK_TEMPLATE` for a custom JSON/Form template and `WEBHOOK_ERROR_TEMPLATE` for a dedicated error template. Available placeholders:

- `{{title}}`, `{{message}}` — common
- `{{anime_name}}`, `{{episode}}`, `{{season}}`, `{{library_path}}` — download events
- `{{name_cn}}`, `{{name_original}}`, `{{summary}}`, `{{rating}}`, `{{image_url}}`, `{{eps_count}}` — download events (Bangumi metadata)

### Test Endpoint

```bash
curl -X POST http://localhost:7893/api/notify/test
```

Sends one normal notification and one failure notification to verify the webhook config. Also accessible via the **📢 测试通知** button in the Web UI.

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
- **Threaded design**: dedicated threads for timers, logic, executor, and the downloader (DlThread); HTTP requests run on a rouille thread pool (default 16).
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
