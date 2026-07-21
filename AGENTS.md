# AGENTS.md — bangumi-rss

> Anime RSS auto-downloader. Single Rust binary, ~5 MB memory, 34 tests (logic 11, handler 9, tokenizer 8, bangumi 5, timer 1, rss 1, downloader 1, executor 1).

## Build & Run

```bash
cargo build                  # debug build
cargo build --release        # release build
cargo test                   # run all tests (28)
cargo run                    # run directly (env vars below)
```

Docker:
```bash
docker build -t bangumi-rss .
docker run -p 7893:7893 -v /path/to/downloads:/downloads -v /path/to/anime:/anime bangumi-rss
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `PORT` | `7893` | HTTP API server port |
| `DATA_DIR` | `.` | State persistence directory (`state.json`) |
| `RSS_INTERVAL` | `900` | RSS poll interval in seconds |
| `ARIA2_RPC_URL` | `http://localhost:6800/jsonrpc` | Aria2 JSON-RPC endpoint |
| `DOWNLOAD_DIR` | — | Torrent download staging directory |
| `LIBRARY_DIR` | — | Media library output directory |
| `MOCK_DOWNLOADER` | — | Set to enable in‑memory mock downloader |
| `DOWNLOADER` | `aria2` | `aria2` or `qbittorrent` |
| `QBITTORRENT_URL` | `http://localhost:8080` | qBittorrent Web UI base URL |
| `QBITTORRENT_USER` | `admin` | qBittorrent username |
| `QBITTORRENT_PASS` | `adminadmin` | qBittorrent password |

## Architecture: TEA (The Elm Architecture)

Unidirectional data flow: **Event → State → Effect**

```
Event Sources (timers, server)
       │  Event
       ▼
  logic::reduce(&state, event) → (new_state, Vec<Effect>)
       │                              │
       ▼                              ▼
  AppState.save()              EffectExecutor.run()
  (if dirty)                   (I/O: HTTP, FS, aria2 RPC)
                                    │
                                    ▼
                              Feedback Events → event_tx
```

- **`logic::reduce` is pure** — no I/O, no side effects. Testable.
- State uses **copy-on-write** builder pattern: `state.with_*()`, etc.
- State persists to `{DATA_DIR}/state.json` only when changed.
- 4 threads: timers (combined), HTTP server, executor, logic.

## Key Modules

| File | Purpose |
|---|---|
| `main.rs` | Bootstrap: channels, services, 4 threads, logic loop |
| `timer.rs` | Zero‑dependency periodic timer manager with graceful shutdown |
| `event.rs` | `Event` enum (15 variants) + `run_logic()` loop |
| `effect.rs` | `Effect` enum (9 variants) — pure data |
| `logic.rs` | `reduce()` — pure reducer, one handler per event (11 tests) |
| `state.rs` | `AppState` + `Feed` — serializable, CoW builders, URL dedup |
| `server.rs` | `tiny_http` API + two-step feed confirmation page at `/` |
| `feed.rs` | RSS-related utilities (placeholder) |
| `handler.rs` | Pure post‑download logic: `resolve_files`, toolkit functions (9 tests) |
| `tokenizer.rs` | Regex-based torrent title parser + batch detection (2 tests) |
| `types.rs` | Core types: `EpisodeRecord`, `EpisodeKey`, `AnimeIdentity`, `ResolvedEpisode`, `DownloadSnapshot`, `DownloadInfo` |
| `traits.rs` | Service abstractions: `RssFetcher`, `TorrentDownloader`, `FileOps`, `Notifier`, `BangumiSearcher` |
| `services/mod.rs` | `EffectExecutor<R,D,F,N,B>` — generic effect runner |
| `services/rss.rs` | `RssClient` — ureq-based XML RSS parser + `fetch_preview` (1 test) |
| `services/downloader.rs` | `Aria2Downloader` — stateless JSON-RPC client, paginated gid lookup (1 test) |
| `services/qbittorrent.rs` | `QbittorrentDownloader` — Web API client with SID cookie auth |
| `services/mock.rs` | `MockDownloader`, `MockRssClient`, `MockFileSystem` (all use `Mutex` for thread safety) |
| `services/fs.rs` | `RealFileSystem` — thin `std::fs` wrapper |
| `services/notify.rs` | `NoopNotifier` (Server酱 TODO) |
| `services/bangumi.rs` | `bangumi::search()`, `bangumi::detail()` — old API (no-auth), serde-deserialized (5 tests) |
| `util.rs` | Pure helpers for server: `fetch_feed_preview()` (RSS + tokenizer + Bangumi) |
| `confirm.html` | Web UI template — loaded via `include_str!` |

## Data Model

```
AnimeIdentity { name, season }
    │
    ├─→ Feed.anime                    (subscription)
    ├─→ EpisodeKey.anime + episode    (dedup key)
    │       │
    │       └─→ EpisodeRecord {
    │             infohash, torrent_url, feed_id,
    │             key: EpisodeKey, status, library_path
    │           }
    │               │
    │               └─→ tracker: HashMap<infohash, EpisodeRecord>
    │
    └─→ FeedPreview                   (web UI before confirm)
```

- `AppState.seen_urls: HashSet<String>` — persisted URL dedup, survives restarts.
- `EpisodeRecord.library_path` — populated after file move to media library.- `Feed.bangumi_info: Option<BangumiInfo>` — attached on confirm if preview fetched it; persisted in `state.json`.- Batch torrents (`01-12`, `01~12`) are rejected at RSS fetch time.

## Feed Confirmation Pipeline (preview + subscribe)

```
POST /api/feeds/preview
  → server: util::fetch_feed_preview(url)  ← RSS + tokenizer + Bangumi (direct, no TEA)
    → FeedPreview { suggested_name, bangumi_info: Option<BangumiInfo> }
  → JSON response (with cover image URL, rating, tags for web UI)

POST /api/feeds/confirm { name, season, bangumi_info? }
  → Event::ConfirmFeed → logic: Feed { bangumi_info: Some(...) }
    → RSS tick → download pipeline starts immediately
```

## Full Download Pipeline

```
RssTickAll → Effect::FetchRss
  → executor: rss.fetch() → Event::RssItemsFetched
    → logic: URL dedup (seen_urls) + batch rejection → Effect::AddTorrent
      → executor: downloader.add_uri() → Event::DownloadStarted
        → logic: tracker ← EpisodeRecord { infohash, torrent_url, ... }

PollDownloader (every 30s) → Effect::PollCompleted
  → executor: downloader.poll_completed() → detect 100% seeding
    → Event::DownloaderNotification { Completed }
      → logic: Effect::HandleCompleted
        → executor: list_files → handler::resolve_files → move → rename
          → Event::EpisodeCompleted → logic: status = InLibrary + Notify
```

## API Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/` | Two-step feed confirmation UI |
| `POST` | `/api/feeds/preview` | Submit RSS URL → get structured preview |
| `POST` | `/api/feeds/confirm` | Confirm anime name + season → create Feed |
| `POST` | `/api/feeds/update` | Trigger immediate RSS refresh |
| `GET` | `/api/feeds` | List all feeds |
| `DELETE` | `/api/feeds?id=<uuid>` | Remove feed |
| `GET` | `/api/downloads` | List cached downloads (seeding state supported) |
| `POST` | `/api/downloads/refresh` | Trigger download list refresh |

## Download States

| State | Description |
|---|---|
| `Downloading` | Active download |
| `Seeding` | 100% complete, still in aria2 (auto‑detected) |
| `Waiting` | Queued in downloader |
| `Paused` | User paused |
| `Checking` | Verifying integrity |
| `Completed` | Stopped after completion |
| `Failed` | Download error |

## Bangumi Integration

Uses the **legacy (no-auth) API** via `services/bangumi.rs`:

| Endpoint | Method | Purpose |
|---|---|---|
| `/search/subject/{keyword}` | GET | Search by name → subject ID |
| `/subject/{id}?responseGroup=large` | GET | Full metadata (rating, eps, cover, rank, air_date, air_weekday) |

- **`util::fetch_feed_preview(url)`** — combines RSS parsing + tokenizer + Bangumi search into one call, used directly by `server.rs` (not through TEA).
- Image caching is **not** handled by core logic — the server/web UI uses `image_url` directly.
- `ureq` needs `proxy-from-env` feature to respect `HTTP_PROXY`/`HTTPS_PROXY`.
- UA must follow [Bangumi guidelines](https://bangumi.github.io/dev-docs/#user-agent): `ezio/bangumi-rss`.
- **TODO**: New v0 API support (Bearer token, POST search, returns `tags` + `platform`).

## Conventions

- **Rust edition 2024**
- **Error handling**: `anyhow::Result` throughout, `?` operator
- **Channels**: `crossbeam_channel::bounded(256)` for all thread communication
- **Services behind traits**: All I/O is behind `Arc<dyn Trait>` for testability
- **Testing**: 38 tests total — logic (11), handler (9), tokenizer (8), bangumi (5), downloader (3), timer (1), rss (1), executor integration (1)
- **No async runtime** — everything is sync with OS threads and channels
- **Single‑threaded services**: `Aria2Downloader` is only accessed from executor thread
- **Timer**: `TimerManager` — `add(interval, callback → bool)`, returns `false` to self‑remove, graceful shutdown via `AtomicBool`

## Adding a New Event/Effect

1. Add variant to `Event`/`Effect` enum
2. Add handler in `logic::reduce()` match arm
3. Add executor handler in `services/executor.rs` `execute()` match arm (Effects only)
4. Add builder method in `state.rs` if state changes
5. Write a test in `logic.rs` or `handler.rs`
