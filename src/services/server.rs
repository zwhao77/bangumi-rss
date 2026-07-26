use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::Sender;
use matchit::Router;
use socket2::{Domain, Protocol, Socket, Type};
use tiny_http::{Method, Response};

use crate::core::event::Event;
use crate::traits::{FileOps, TorrentDownloader};
use crate::types::{ApiResponse, BangumiInfo};
use base64::Engine as _;

use crate::utils::preview;

const JSON_TYPE: &str = "Content-Type: application/json; charset=utf-8";
const HTML_TYPE: &str = "Content-Type: text/html; charset=utf-8";
const CSS_TYPE: &str = "Content-Type: text/css; charset=utf-8";

// ── Server config ──

pub struct ServerConfig {
    pub bind_addr: String,
    pub port: u16,
    pub max_connections: u32,
    pub max_queue: u32,
    pub auth_username: String,
    pub auth_password: String,
}

// ── Unified response type ──
enum AppResponse {
    Text {
        code: u16,
        body: String,
        content_type: &'static str,
    },
}

// ── Route table (matchit) ──

#[derive(Debug, Clone, Copy)]
enum Route {
    Feeds,            // GET /api/feeds  | POST /api/feeds
    FeedUpdate,       // POST /api/feeds/update
    FeedId,           // PUT /api/feeds/{id}  | DELETE /api/feeds/{id}
    FeedPreview,      // POST /api/feeds/preview
    Downloads,        // GET /api/downloads
    DownloadsRefresh, // POST /api/downloads/refresh
    Poll,             // POST /api/poll
    BangumiSubjects,  // GET /api/bangumi/subjects/{id}
    BangumiSearch,    // GET /api/bangumi/search
    Health,           // GET /api/health
    Slow,             // GET /api/slow  (benchmark: sleeps 2s)
}

fn build_router() -> Router<Route> {
    let mut r = Router::new();
    r.insert("/api/feeds", Route::Feeds)
        .expect("route: /api/feeds");
    r.insert("/api/feeds/update", Route::FeedUpdate)
        .expect("route: /api/feeds/update");
    r.insert("/api/feeds/{id}", Route::FeedId)
        .expect("route: /api/feeds/{id}");
    r.insert("/api/feeds/preview", Route::FeedPreview)
        .expect("route: /api/feeds/preview");
    r.insert("/api/downloads", Route::Downloads)
        .expect("route: /api/downloads");
    r.insert("/api/downloads/refresh", Route::DownloadsRefresh)
        .expect("route: /api/downloads/refresh");
    r.insert("/api/bangumi/subjects/{id}", Route::BangumiSubjects)
        .expect("route: /api/bangumi/subjects/{id}");
    r.insert("/api/bangumi/search", Route::BangumiSearch)
        .expect("route: /api/bangumi/search");
    r.insert("/api/poll", Route::Poll)
        .expect("route: /api/poll");
    r.insert("/api/health", Route::Health)
        .expect("route: /api/health");
    r.insert("/api/slow", Route::Slow)
        .expect("route: /api/slow");
    r
}

// ── Truncation helper ──

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<_> = s.char_indices().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = max * 3 / 4;
    let tail = max / 4;
    let head_end = chars[head].0;
    let tail_start = chars[chars.len() - tail].0;
    format!(
        "{}...<{} chars omitted>...{}",
        &s[..head_end],
        chars.len().saturating_sub(head + tail),
        &s[tail_start..]
    )
}

// ── Server ──

pub fn start(
    event_tx: Sender<Event>,
    downloader: Arc<dyn TorrentDownloader>,
    fs: Arc<dyn FileOps>,
    cfg: ServerConfig,
) {
    let server = try_bind(&cfg.bind_addr, cfg.port, cfg.max_connections, cfg.max_queue).unwrap_or_else(|| {
        log::info!("port {} unavailable, trying OS-assigned", cfg.port);
        try_bind(&cfg.bind_addr, 0, cfg.max_connections, cfg.max_queue).unwrap_or_else(|| {
            log::error!("fatal: failed to bind any port");
            std::process::exit(1);
        })
    });

    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(cfg.port);
    log::info!("listening on http://{}:{}", cfg.bind_addr, actual_port);

    let router = build_router();
    let tx = Arc::new(event_tx);
    let dl = downloader;

    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();

        // ── Basic Auth check ──
        if !cfg.auth_username.is_empty() {
            let authorized = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .and_then(|h| {
                    let h = h.value.as_str().strip_prefix("Basic ")?;
                    let decoded = base64::engine::general_purpose::STANDARD.decode(h).ok()?;
                    let s = std::str::from_utf8(&decoded).ok()?;
                    let (u, p) = s.split_once(':')?;
                    Some(u == cfg.auth_username && p == cfg.auth_password)
                })
                .unwrap_or(false);

            if !authorized {
                let _ = request.respond(
                    Response::from_string("401 Unauthorized")
                        .with_status_code(401)
                        .with_header(
                            "WWW-Authenticate: Basic realm=\"bangumi-rss\""
                                .parse::<tiny_http::Header>()
                                .expect("invalid WWW-Authenticate header"),
                        ),
                );
                continue;
            }
        }

        let mut body = String::new();
        let _ = request.as_reader().read_to_string(&mut body);
        log::info!("-> {} {} body={}", method, url, truncate(&body, 200));

        let path = url.split('?').next().unwrap_or(&url);
        let matched = router.at(path).ok();
        let route = matched.as_ref().map(|m| m.value);

        let resp = match (route, method) {
            // ═══ / ═══
            (None, _) if path == "/" => handle_index(&*fs),
            (None, _) if path == "/style.css" => handle_style_css(&*fs),

            // ═══ /api/feeds ═══
            (Some(Route::Feeds), Method::Get) => handle_list_feeds(&tx),
            (Some(Route::Feeds), Method::Post) => handle_feed_create(&body, &tx),

            // ═══ /api/feeds/update ═══
            (Some(Route::FeedUpdate), Method::Post) => handle_feed_update_all(&tx),

            // ═══ /api/feeds/{id} ═══
            (Some(Route::FeedId), Method::Put) => {
                let id = matched
                    .expect("FeedId matched but no route")
                    .params
                    .get("id")
                    .unwrap_or("");
                handle_feed_update(id, &body, &tx)
            }
            (Some(Route::FeedId), Method::Delete) => {
                let id = matched
                    .expect("FeedId matched but no route")
                    .params
                    .get("id")
                    .unwrap_or("");
                handle_feed_delete(id, &tx)
            }

            // ═══ /api/feeds/preview ═══
            (Some(Route::FeedPreview), Method::Post) => handle_preview(&body),

            // ═══ /api/downloads ═══
            (Some(Route::Downloads), Method::Get) => handle_list_downloads(&tx),
            (Some(Route::DownloadsRefresh), Method::Post) => handle_refresh(&tx),

            // ═══ /api/poll ═══
            (Some(Route::Poll), Method::Post) => handle_poll(&tx),

            // ═══ /api/bangumi ═══
            (Some(Route::BangumiSubjects), Method::Get) => {
                let id = matched
                    .expect("BangumiSubjects matched but no route")
                    .params
                    .get("id")
                    .unwrap_or("");
                handle_bangumi_subject(id)
            }
            (Some(Route::BangumiSearch), Method::Get) => handle_bangumi_search(&url),
            (Some(Route::Health), Method::Get) => handle_health(&*dl),
            (Some(Route::Slow), Method::Get) => handle_slow(),

            _ => AppResponse::Text {
                code: 404,
                body: "404".into(),
                content_type: JSON_TYPE,
            },
        };

        respond(request, resp, &url);
    }
}

// ── Unified response sender ──

fn respond(request: tiny_http::Request, resp: AppResponse, url: &str) {
    // Extract metadata before moving `resp`.
    let code = match &resp {
        AppResponse::Text { code, .. } => *code,
    };
    let body_repr = match &resp {
        AppResponse::Text {
            body, content_type, ..
        } if *content_type == HTML_TYPE => {
            format!("<HTML {} chars>", body.len())
        }
        AppResponse::Text { body, .. } => truncate(body, 500),
    };

    let result = match resp {
        AppResponse::Text {
            code,
            body,
            content_type,
        } => {
            let header = content_type
                .parse::<tiny_http::Header>()
                .expect("invalid Content-Type header");
            request.respond(
                Response::from_string(body)
                    .with_status_code(code)
                    .with_header(header),
            )
        }
    };

    match result {
        Ok(()) => log::debug!("<- {} {} body={}", code, url, body_repr),
        Err(e) => log::error!("<- {} {} respond failed: {e}", code, url),
    }
}

// ── Server binding ──

/// Try to bind to a specific port. Returns `None` if the port is unavailable.
fn try_bind(bind_addr: &str, port: u16, backlog: u32, max_queue: u32) -> Option<tiny_http::Server> {
    let addr: SocketAddr = format!("{bind_addr}:{port}").parse().ok()?;
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP)).ok()?;
    socket.set_reuse_address(true).ok()?;
    socket.bind(&addr.into()).ok()?;
    socket.listen(backlog as i32).ok()?;
    let listener: std::net::TcpListener = socket.into();
    let queue = if max_queue > 0 { Some(max_queue as usize) } else { None };
    tiny_http::Server::from_listener(
        listener,
        None::<tiny_http::SslConfig>,
        Some(backlog as usize),
        queue,
    )
    .ok()
}

// ── Route handlers ──

/// Validate that a string looks like a usable RSS feed URL.
fn is_valid_rss_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 2048 {
        return false;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return false;
    }
    // Must have at least a basic domain start after scheme.
    let after_scheme = &url[url.find("://").unwrap_or(usize::MAX) + 3..];
    after_scheme.starts_with(|c: char| c.is_alphanumeric())
}

fn handle_index(fs: &dyn FileOps) -> AppResponse {
    AppResponse::Text {
        code: 200,
        body: load_confirm_page(fs),
        content_type: HTML_TYPE,
    }
}

fn handle_style_css(fs: &dyn FileOps) -> AppResponse {
    AppResponse::Text {
        code: 200,
        body: load_style_css(fs),
        content_type: CSS_TYPE,
    }
}

fn handle_preview(body: &str) -> AppResponse {
    let url = body.trim();
    if !is_valid_rss_url(url) {
        return AppResponse::Text {
            code: 400,
            body: r#"{"success":false,"message":"invalid URL"}"#.into(),
            content_type: JSON_TYPE,
        };
    }
    match preview::fetch_feed_preview(url) {
        Ok(preview) => {
            let json = serde_json::to_string(&preview).unwrap_or_default();
            AppResponse::Text {
                code: 200,
                body: json,
                content_type: JSON_TYPE,
            }
        }
        Err(e) => AppResponse::Text {
            code: 400,
            body: serde_json::json!({"success":false,"message":format!("{e}")}).to_string(),
            content_type: JSON_TYPE,
        },
    }
}

/// POST /api/feeds — create (confirm) a new feed subscription.
fn handle_feed_create(body: &str, tx: &Sender<Event>) -> AppResponse {
    let confirm: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let url = confirm["url"].as_str().unwrap_or("").to_string();
    if !is_valid_rss_url(&url) {
        return AppResponse::Text {
            code: 400,
            body: r#"{"success":false,"message":"invalid URL"}"#.into(),
            content_type: JSON_TYPE,
        };
    }
    let name = confirm["name"].as_str().unwrap_or("").to_string();
    let season = confirm["season"].as_u64().unwrap_or(1) as u8;
    let bangumi_info: Option<BangumiInfo> = confirm
        .get("bangumi_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::ConfirmFeed {
        url,
        name,
        season,
        bangumi_info,
        reply_tx,
    });
    let result = reply_rx.recv().unwrap_or(ApiResponse {
        success: false,
        message: "timeout".into(),
    });
    let json = serde_json::to_string(&result).unwrap_or_default();
    AppResponse::Text {
        code: 200,
        body: json,
        content_type: JSON_TYPE,
    }
}

/// PUT /api/feeds/{id} — update feed name / season / bangumi_info.
fn handle_feed_update(id: &str, body: &str, tx: &Sender<Event>) -> AppResponse {
    let feed_id = match uuid::Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => {
            return AppResponse::Text {
                code: 400,
                body: r#"{"success":false,"message":"invalid id"}"#.into(),
                content_type: JSON_TYPE,
            };
        }
    };

    let update: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let name = update["name"].as_str().unwrap_or("").to_string();
    let season = update["season"].as_u64().unwrap_or(1) as u8;
    let bangumi_info: Option<BangumiInfo> = update
        .get("bangumi_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::UserConfirm {
        feed_id,
        name,
        season,
        bangumi_info,
        reply_tx,
    });
    let result = reply_rx.recv().unwrap_or(ApiResponse {
        success: false,
        message: "timeout".into(),
    });
    AppResponse::Text {
        code: if result.success { 200 } else { 404 },
        body: serde_json::to_string(&result).unwrap_or_default(),
        content_type: JSON_TYPE,
    }
}

/// DELETE /api/feeds/{id}
fn handle_feed_delete(id: &str, tx: &Sender<Event>) -> AppResponse {
    let feed_id = match uuid::Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => {
            return AppResponse::Text {
                code: 400,
                body: r#"{"success":false,"message":"invalid id"}"#.into(),
                content_type: JSON_TYPE,
            };
        }
    };

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::ApiRemoveFeed { feed_id, reply_tx });
    let result = reply_rx.recv().unwrap_or(ApiResponse {
        success: false,
        message: "timeout".into(),
    });
    let json = serde_json::to_string(&result).unwrap_or_default();
    AppResponse::Text {
        code: 200,
        body: json,
        content_type: JSON_TYPE,
    }
}

fn handle_list_feeds(tx: &Sender<Event>) -> AppResponse {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::ApiListFeeds { reply_tx });
    let feeds = reply_rx.recv().unwrap_or_default();
    let json = serde_json::to_string(&feeds).unwrap_or_default();
    AppResponse::Text {
        code: 200,
        body: json,
        content_type: JSON_TYPE,
    }
}

fn handle_list_downloads(tx: &Sender<Event>) -> AppResponse {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::ApiListDownloads { reply_tx });
    let downloads = reply_rx.recv().unwrap_or_default();
    let json = serde_json::to_string(&downloads).unwrap_or_default();
    AppResponse::Text {
        code: 200,
        body: json,
        content_type: JSON_TYPE,
    }
}

fn handle_refresh(tx: &Sender<Event>) -> AppResponse {
    let _ = tx.send(Event::RefreshDownloads);
    AppResponse::Text {
        code: 200,
        body: r#"{"success":true,"message":"refresh triggered"}"#.into(),
        content_type: JSON_TYPE,
    }
}

/// POST /api/feeds/update — trigger immediate RSS poll for all feeds.
fn handle_feed_update_all(tx: &Sender<Event>) -> AppResponse {
    let _ = tx.send(Event::RssTickAll);
    AppResponse::Text {
        code: 200,
        body: r#"{"success":true,"message":"RSS refresh triggered"}"#.into(),
        content_type: JSON_TYPE,
    }
}

/// POST /api/poll — trigger immediate downloader poll (completed/failed).
fn handle_poll(tx: &Sender<Event>) -> AppResponse {
    let _ = tx.send(Event::PollDownloader);
    AppResponse::Text {
        code: 200,
        body: r#"{"success":true,"message":"downloader poll triggered"}"#.into(),
        content_type: JSON_TYPE,
    }
}

/// GET /api/bangumi/subjects/{id}
fn handle_bangumi_subject(id_str: &str) -> AppResponse {
    let id: u32 = match id_str.parse() {
        Ok(n) => n,
        Err(_) => {
            return AppResponse::Text {
                code: 400,
                body: r#"{"success":false,"message":"invalid id"}"#.into(),
                content_type: JSON_TYPE,
            };
        }
    };

    match crate::services::bangumi::detail(id) {
        Ok(Some(info)) => AppResponse::Text {
            code: 200,
            body: serde_json::json!({"success":true,"bangumi_info":info}).to_string(),
            content_type: JSON_TYPE,
        },
        Ok(None) => AppResponse::Text {
            code: 404,
            body: r#"{"success":false,"message":"not found"}"#.into(),
            content_type: JSON_TYPE,
        },
        Err(e) => AppResponse::Text {
            code: 502,
            body: serde_json::json!({"success":false,"message":format!("{e}")}).to_string(),
            content_type: JSON_TYPE,
        },
    }
}

/// GET /api/bangumi/search?name=<url_encoded>
fn handle_bangumi_search(url: &str) -> AppResponse {
    let query_str = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let name = serde_urlencoded::from_str::<Vec<(String, String)>>(query_str)
        .ok()
        .and_then(|pairs| pairs.into_iter().find(|(k, _)| k == "name"))
        .map(|(_, v)| v)
        .unwrap_or_default();

    if name.is_empty() {
        return AppResponse::Text {
            code: 400,
            body: r#"{"success":false,"message":"missing name"}"#.into(),
            content_type: JSON_TYPE,
        };
    }

    let result = match crate::services::bangumi::search(&name) {
        Ok(Some(id)) => {
            log::info!("Bangumi search '{name}' → #{id}");
            match crate::services::bangumi::detail(id) {
                Ok(Some(info)) => {
                    serde_json::json!({ "success": true, "bangumi_info": info })
                }
                Ok(None) => serde_json::json!({ "success": false, "message": "no detail" }),
                Err(e) => serde_json::json!({ "success": false, "message": format!("{e}") }),
            }
        }
        Ok(None) => serde_json::json!({ "success": false, "message": "not found" }),
        Err(e) => serde_json::json!({ "success": false, "message": format!("{e}") }),
    };

    AppResponse::Text {
        code: 200,
        body: result.to_string(),
        content_type: JSON_TYPE,
    }
}

/// GET /api/health
fn handle_health(dl: &dyn TorrentDownloader) -> AppResponse {
    match dl.check_connection() {
        Ok(()) => AppResponse::Text {
            code: 200,
            body: r#"{"success":true,"downloader":"connected"}"#.into(),
            content_type: JSON_TYPE,
        },
        Err(e) => AppResponse::Text {
            code: 503,
            body:
                serde_json::json!({"success":false,"downloader":"error","message":format!("{e}")})
                    .to_string(),
            content_type: JSON_TYPE,
        },
    }
}

fn handle_slow() -> AppResponse {
    std::thread::sleep(std::time::Duration::from_secs(2));
    AppResponse::Text {
        code: 200,
        body: r#"{"success":true,"slow":"ok"}"#.into(),
        content_type: JSON_TYPE,
    }
}

fn load_confirm_page(fs: &dyn FileOps) -> String {
    fs.read_to_string(Path::new("res/index.html"))
        .unwrap_or_else(|_| include_str!("../../res/index.html").to_string())
}

fn load_style_css(fs: &dyn FileOps) -> String {
    fs.read_to_string(Path::new("res/style.css"))
        .unwrap_or_else(|_| include_str!("../../res/style.css").to_string())
}
