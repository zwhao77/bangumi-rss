# bangumi-rss

番剧 RSS 自动下载器。订阅 RSS 源、自动下载、整理入库。

单个 Rust 二进制文件，约 5 MB 内存占用。

## 快速开始

```bash
# 1. 克隆
git clone https://github.com/zwhao77/bangumi-rss.git
cd bangumi-rss

# 2. 启动 aria2 并开启 JSON-RPC
aria2c --enable-rpc --rpc-listen-all --rpc-allow-origin-all &

# 3. 编译（res/index.html 和 res/style.css 会在编译时嵌入二进制）
cargo build --release

# 4. 运行
ARIA2_RPC_URL=http://localhost:6800/jsonrpc \
DOWNLOAD_DIR=/downloads \
LIBRARY_DIR=/anime \
./target/release/bangumi-rss
```

打开 `http://localhost:7893` 即可订阅和管理。

## 资源占用

| 指标 | 典型值 |
|--------|---------|
| 二进制体积 | ~7.4 MB（release） |
| RSS（物理内存） | ~5 MB（空闲与负载下均稳定） |
| CPU | 空闲 0%，请求时几乎无波动 |
| 状态写入耗时 | < 100 µs 每次 |
| 状态文件大小 | 数十个订阅数月后仍约 50 KB |

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `PORT` | `7893` | HTTP API 端口 |
| `NO_SERVER` | `false` | 禁用 HTTP 服务器 |
| `DATA_DIR` | `.` | 状态文件目录 (`state.json`) |
| `RSS_INTERVAL` | `900` | RSS 轮询间隔（秒） |
| `ARIA2_RPC_URL` | `http://localhost:6800/jsonrpc` | Aria2 JSON-RPC 端点 |
| `DOWNLOAD_DIR` | `/downloads` | 种子暂存目录 |
| `LIBRARY_DIR` | `/anime` | 媒体库输出目录 |
| `DOWNLOADER` | `aria2` | `aria2` 或 `qbittorrent` |
| `MOCK_DOWNLOADER` | `false` | 启用内存模拟下载器（测试用） |
| `QBITTORRENT_URL` | `http://localhost:8080` | qBittorrent Web UI 地址 |
| `QBITTORRENT_USER` | `admin` | qBittorrent 用户名 |
| `QBITTORRENT_PASS` | `adminadmin` | qBittorrent 密码 |
| `BANGUMI_API_BASE` | `https://api.bgm.tv` | Bangumi API 基础 URL |
| `MAX_CONCURRENCY` | `8` | 最大并发 HTTP 请求数 |
| `TORRENT_CONCURRENCY` | `4` | Worker pool 线程数（RSS + 种子下载） |
| `QUEUE_CAPACITY` | `512` | Worker pool 队列容量 |
| `BIND_ADDR` | `127.0.0.1` | HTTP 监听地址（`0.0.0.0` 监听所有接口） |
| `AUTH_USERNAME` | — | Basic Auth 用户名（留空不启用） |
| `AUTH_PASSWORD` | — | Basic Auth 密码 |
| `RUST_LOG` | `info` | 日志级别（设为 `warn` 可减少输出） |

## 架构

**TEA 模式**（The Elm Architecture）：`Event → State → Effect`

```
定时器/HTTP 请求
       │  Event
       ▼
  logic::reduce(&state, event) → (new_state, Vec<Effect>)
       │                              │
       ▼                              ▼
  AppState.save()              EffectExecutor.run()
  (状态变化时)                  (I/O: HTTP, 文件系统, 下载器 RPC)
                                    │
                                    ▼
                              反馈 Event → event_tx
```

- **`logic::reduce` 纯函数** — 无 I/O、无副作用，可完整测试
- **4 个线程**：定时器、HTTP 服务、执行器、逻辑处理
- **原子写入** — `state.tmp` + `rename` 方式，防止损坏

## 前端

Web 界面由 `/` 路由提供，包含两个源文件：
- `res/index.html` — HTML 结构 + JavaScript 逻辑
- `res/style.css` — 样式

两者通过 `include_str!` 编译进二进制，运行时无需额外文件。如需自定义界面，在二进制同目录放置 `res/index.html` 和 `res/style.css` 即可覆盖内置版本，无需重编译。

## 部署

```bash
docker build -t bangumi-rss .
docker run -p 7893:7893 -v /path/to/downloads:/downloads -v /path/to/anime:/anime bangumi-rss
```

或直接复制单个二进制：

```bash
cp target/release/bangumi-rss /usr/local/bin/
bangumi-rss
```

## 协议

MIT
