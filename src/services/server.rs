use crate::core::event::Event;
use crate::types::{ApiResponse, BangumiInfo};
use crate::utils::preview;
use crossbeam_channel::Sender;
use matchit::Router;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use tiny_http::{Method, Response};

const JSON_TYPE: &str = "Content-Type: application/json; charset=utf-8";
const HTML_TYPE: &str = "Content-Type: text/html; charset=utf-8";

/// Unified response type — every handler returns this, `respond()` sends it.
enum AppResponse {
    Text {
        code: u16,
        body: String,
        content_type: &'static str,
    },
    Image {
        code: u16,
        data: Vec<u8>,
        content_type: String,
    },
}

// ── Route table (matchit) ──

#[derive(Debug, Clone, Copy)]
enum Route {
    Feeds,            // GET /api/feeds  | POST /api/feeds
    FeedId,           // PUT /api/feeds/{id}  | DELETE /api/feeds/{id}
    FeedPreview,      // POST /api/feeds/preview
    Downloads,        // GET /api/downloads
    DownloadsRefresh, // POST /api/downloads/refresh
    BangumiSubjects,  // GET /api/bangumi/subjects/{id}
    BangumiSearch,    // GET /api/bangumi/search
    ImageProxy,       // GET /api/bangumi/image
}

fn build_router() -> Router<Route> {
    let mut r = Router::new();
    r.insert("/api/feeds", Route::Feeds).unwrap();
    r.insert("/api/feeds/{id}", Route::FeedId).unwrap();
    r.insert("/api/feeds/preview", Route::FeedPreview).unwrap();
    r.insert("/api/downloads", Route::Downloads).unwrap();
    r.insert("/api/downloads/refresh", Route::DownloadsRefresh)
        .unwrap();
    r.insert("/api/bangumi/subjects/{id}", Route::BangumiSubjects)
        .unwrap();
    r.insert("/api/bangumi/search", Route::BangumiSearch)
        .unwrap();
    r.insert("/api/bangumi/image", Route::ImageProxy).unwrap();
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

pub fn start(event_tx: Sender<Event>, preferred: u16) {
    let server = try_bind(preferred).unwrap_or_else(|| {
        log::info!("port {preferred} unavailable, trying OS-assigned");
        try_bind(0).unwrap_or_else(|| {
            log::error!("fatal: failed to bind any port");
            std::process::exit(1);
        })
    });

    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(preferred);
    log::info!("listening on http://127.0.0.1:{actual_port}");

    let router = build_router();
    let tx = Arc::new(event_tx);

    for mut request in server.incoming_requests() {
        let tx = tx.clone();
        let method = request.method().clone();
        let url = request.url().to_string();

        let mut body = String::new();
        let _ = request.as_reader().read_to_string(&mut body);
        log::info!("-> {} {} body={}", method, url, truncate(&body, 200));

        // Strip query string for matchit routing.
        let path = url.split('?').next().unwrap_or(&url);
        let matched = router.at(path).ok();
        let route = matched.as_ref().map(|m| m.value);

        let resp = match (route, method) {
            // ═══ / ═══
            (None, _) if path == "/" => handle_index(),

            // ═══ /api/feeds ═══
            (Some(Route::Feeds), Method::Get) => handle_list_feeds(&tx),
            (Some(Route::Feeds), Method::Post) => handle_feed_create(&body, &tx),

            // ═══ /api/feeds/{id} ═══
            (Some(Route::FeedId), Method::Put) => {
                let id = matched.unwrap().params.get("id").unwrap_or("");
                handle_feed_update(id, &body, &tx)
            }
            (Some(Route::FeedId), Method::Delete) => {
                let id = matched.unwrap().params.get("id").unwrap_or("");
                handle_feed_delete(id, &tx)
            }

            // ═══ /api/feeds/preview ═══
            (Some(Route::FeedPreview), Method::Post) => handle_preview(&body),

            // ═══ /api/downloads ═══
            (Some(Route::Downloads), Method::Get) => handle_list_downloads(&tx),
            (Some(Route::DownloadsRefresh), Method::Post) => handle_refresh(&tx),

            // ═══ /api/bangumi ═══
            (Some(Route::BangumiSubjects), Method::Get) => {
                let id = matched.unwrap().params.get("id").unwrap_or("");
                handle_bangumi_subject(id)
            }
            (Some(Route::BangumiSearch), Method::Get) => handle_bangumi_search(&url),
            (Some(Route::ImageProxy), Method::Get) => handle_image_proxy(&url),

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
        AppResponse::Image { code, .. } => *code,
    };
    let body_repr = match &resp {
        AppResponse::Text {
            body, content_type, ..
        } if *content_type == HTML_TYPE => {
            format!("<HTML {} chars>", body.len())
        }
        AppResponse::Text { body, .. } => truncate(body, 500),
        AppResponse::Image {
            data, content_type, ..
        } => {
            format!("<image {} bytes, {content_type}>", data.len())
        }
    };

    // Move ownership of body/data — zero-copy.
    let result = match resp {
        AppResponse::Text {
            code,
            body,
            content_type,
        } => {
            let header = content_type.parse::<tiny_http::Header>().unwrap();
            request.respond(
                Response::from_string(body)
                    .with_status_code(code)
                    .with_header(header),
            )
        }
        AppResponse::Image {
            code,
            data,
            content_type,
        } => {
            // content_type is pre-validated in the handler, unwrap is safe.
            let header = format!("Content-Type: {content_type}")
                .parse::<tiny_http::Header>()
                .unwrap();
            let cache = "Cache-Control: public, max-age=86400"
                .parse::<tiny_http::Header>()
                .unwrap();
            request.respond(
                Response::from_data(data)
                    .with_status_code(code)
                    .with_header(header)
                    .with_header(cache),
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
fn try_bind(port: u16) -> Option<tiny_http::Server> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    tiny_http::Server::http(addr).ok()
}

// ── Route handlers ──

fn handle_index() -> AppResponse {
    AppResponse::Text {
        code: 200,
        body: CONFIRM_PAGE.into(),
        content_type: HTML_TYPE,
    }
}

fn handle_preview(body: &str) -> AppResponse {
    let url = body.trim();
    let preview = preview::fetch_feed_preview(url).unwrap_or_default();
    let json = serde_json::to_string(&preview).unwrap_or_default();
    AppResponse::Text {
        code: 200,
        body: json,
        content_type: JSON_TYPE,
    }
}

/// POST /api/feeds — create (confirm) a new feed subscription.
fn handle_feed_create(body: &str, tx: &Sender<Event>) -> AppResponse {
    let confirm: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let url = confirm["url"].as_str().unwrap_or("").to_string();
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

/// PUT /api/feeds/{id} — update feed name / season.
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

    let _ = tx.send(Event::UserConfirm {
        feed_id,
        name,
        season,
    });
    AppResponse::Text {
        code: 200,
        body: r#"{"success":true,"message":"updated"}"#.into(),
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

/// GET /api/bangumi/image?url=<encoded_url>
fn handle_image_proxy(path: &str) -> AppResponse {
    let query_str = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let img_url = serde_urlencoded::from_str::<Vec<(String, String)>>(query_str)
        .ok()
        .and_then(|pairs| pairs.into_iter().find(|(k, _)| k == "url"))
        .map(|(_, v)| v)
        .unwrap_or_default();

    if img_url.is_empty() {
        return AppResponse::Image {
            code: 400,
            data: Vec::new(),
            content_type: "text/plain".into(),
        };
    }

    match ureq::get(&img_url).call() {
        Ok(resp) => {
            let raw_ct = resp.content_type().to_string();
            let ct = if format!("Content-Type: {raw_ct}")
                .parse::<tiny_http::Header>()
                .is_ok()
            {
                raw_ct
            } else {
                log::warn!("image proxy: invalid content-type '{raw_ct}', using octet-stream");
                "application/octet-stream".into()
            };
            let mut buf = Vec::new();
            if resp.into_reader().read_to_end(&mut buf).is_ok() {
                return AppResponse::Image {
                    code: 200,
                    data: buf,
                    content_type: ct,
                };
            }
        }
        Err(e) => log::warn!("image proxy failed for {img_url}: {e}"),
    }
    AppResponse::Image {
        code: 404,
        data: Vec::new(),
        content_type: "text/plain".into(),
    }
}

/// Rewrite raw Bangumi CDN URL to go through our image proxy.
/// This ensures images work when the browser can't directly reach lain.bgm.tv.
fn rewrite_image_url(mut info: BangumiInfo) -> BangumiInfo {
    if !info.image_url.is_empty() {
        let encoded: String = info
            .image_url
            .bytes()
            .flat_map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    vec![b as char]
                }
                b' ' => vec!['+'],
                _ => format!("%{:02X}", b).chars().collect(),
            })
            .collect();
        info.image_url = format!("/api/bangumi/image?url={encoded}");
    }
    info
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
            body: serde_json::json!({"success":true,"bangumi_info":rewrite_image_url(info)}).to_string(),
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
                    serde_json::json!({ "success": true, "bangumi_info": rewrite_image_url(info) })
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

const CONFIRM_PAGE: &str = include_str!("../../res/confirm.html");
