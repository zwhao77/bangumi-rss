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
| `POLL_INTERVAL` | `30` | 下载状态轮询间隔（秒） |
| `ARIA2_RPC_URL` | `http://localhost:6800/jsonrpc` | Aria2 JSON-RPC 端点 |
| `DOWNLOAD_DIR` | `/downloads` | 种子暂存目录 |
| `LIBRARY_DIR` | `/anime` | 媒体库输出目录 |
| `DOWNLOADER` | `aria2` | `aria2`、`qbittorrent` 或 `transmission` |
| `MOCK_DOWNLOADER` | `false` | 启用内存模拟下载器（测试用） |
| `QBITTORRENT_URL` | `http://localhost:8080` | qBittorrent Web UI 地址 |
| `QBITTORRENT_USER` | `admin` | qBittorrent 用户名 |
| `QBITTORRENT_PASS` | `adminadmin` | qBittorrent 密码 |
| `TRANSMISSION_RPC_URL` | `http://localhost:9091/transmission/rpc` | Transmission RPC 端点 |
| `TRANSMISSION_USER` | (空) | Transmission HTTP Basic Auth 用户名 |
| `TRANSMISSION_PASS` | (空) | Transmission HTTP Basic Auth 密码 |
| `BANGUMI_API_BASE` | `https://api.bgm.tv` | Bangumi API 基础 URL |
| `TORRENT_CONCURRENCY` | `4` | Worker pool 线程数（RSS + 种子下载） |
| `QUEUE_CAPACITY` | `512` | Worker pool 队列容量 |
| `BIND_ADDR` | `127.0.0.1` | HTTP 监听地址（`0.0.0.0` 监听所有接口） |
| `AUTH_USERNAME` | — | Basic Auth 用户名（留空不启用） |
| `AUTH_PASSWORD` | — | Basic Auth 密码 |
| `RUST_LOG` | `info` | 日志级别（设为 `warn` 可减少输出） |
| `WEBHOOK_URL` | — | Webhook URL（如 `http://gotify:8080/message?token=xxx`） |
| `WEBHOOK_FORMAT` | — | 预设格式：`bark`、`gotify` 或 `serverchan` |
| `WEBHOOK_TEMPLATE` | — | 自定义 JSON/Form 模板（覆盖预设） |
| `WEBHOOK_ERROR_TEMPLATE` | — | 自定义错误模板（覆盖默认错误格式） |

## 下载器选择

| 下载器 | rename_file | move_files | 做种保留 | 推荐场景 |
|---|---|---|---|---|
| **Transmission** (4.1.0+) | ✅ `torrent_rename_path` | ✅ `torrent_set_location` | ✅ | 路由器 / NAS，默认推荐 |
| **qBittorrent** | ✅ `torrents/renameFile` | ✅ `torrents/setLocation` | ✅ | x86 软路由 / NAS（≥512MB RAM） |
| **aria2** | ❌ 不支持 BT 多文件重命名 | ❌ 无内置 move API | ❌ 下载完成后立即停止 | 轻量直链下载，或作为后备方案 |

### aria2 的 BitTorrent 限制

aria2 对于 BitTorrent 种子存在以下已知限制：

1. **不支持 torrent 内文件重命名**：`changeOption("out")` 仅对单文件 HTTP 下载有效，对 BT 多文件种子无效。bangumi-rss 在检测到 aria2 时会 fallback 到文件系统操作（使用 `fs.move_file`）。
2. **不支持下载器感知的移动**：aria2 没有类似 `torrent_set_location` 或 `setLocation` 的 API，移文件后下载器不知道新位置。
3. **做种中断**：由于以上限制，aria2 在下载完成后会被停止并从列表中移除，再做文件系统搬移。这种行为**违背 BT 共享精神**——如果你希望持续做种，请使用 Transmission 或 qBittorrent。

> **建议**：如果下载内容主要是 BT 种子，优先选择 **Transmission**。它的 JSON-RPC 2.0 接口完善、内存占用低（空闲 15-25MB）、在 OpenWrt 上有官方包，且支持 `rename_path` + `set_location` 避免做种中断。

## 通知

bangumi-rss 在下载完成或失败时发送 Webhook 通知。

### 快速配置（Gotify）

```bash
# 部署 Gotify
docker run -d -p 7894:80 gotify/server

# 在 Gotify Web UI 中创建应用，复制 token
# 启动 bangumi-rss 时配置 webhook
WEBHOOK_URL=http://localhost:7894/message?token=你的_TOKEN \
WEBHOOK_FORMAT=gotify \
./bangumi-rss
```

### 预设格式

| 预设 | WEBHOOK_FORMAT | 预期 URL |
|--------|---------------|----------|
| [Gotify](https://gotify.net) | `gotify` | `http://host:port/message?token=xxx` |
| [Bark](https://github.com/Finb/Bark) | `bark` | `http://host:port/your-key` |
| [Server酱](https://sct.ftqq.com) | `serverchan` | `https://sctapi.ftqq.com/SCTxxx.send` |

### 自定义模板

使用 `WEBHOOK_TEMPLATE` 自定义 JSON/Form 模板，`WEBHOOK_ERROR_TEMPLATE` 自定义错误模板。可用占位符：

- `{{title}}`、`{{message}}` — 通用
- `{{anime_name}}`、`{{episode}}`、`{{season}}`、`{{library_path}}` — 下载事件
- `{{name_cn}}`、`{{name_original}}`、`{{summary}}`、`{{rating}}`、`{{image_url}}`、`{{eps_count}}` — 下载事件（Bangumi 元数据）

### 测试端点

```bash
curl -X POST http://localhost:7893/api/notify/test
```

发送一条正常通知和一条错误通知，用于验证 Webhook 配置。也可通过 Web UI 中的 **📢 测试通知** 按钮调用。

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
