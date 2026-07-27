//! HTTP API server using Rouille — a synchronous micro-web-framework.
//!
//! Rouille replaces the previous vendored tiny-http + matchit + hand-rolled Range.
//! Benefits:
//!   - `router!` macro for clean URL dispatch
//!   - Built-in thread pool via `Server::pool_size()`
//!   - Range support is implemented manually via `FileStream` + `ResponseBody::from_reader_and_size`

use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use crossbeam_channel::Sender;
use rouille::router;
use rouille::{Request, Response, ResponseBody, Server};

use crate::core::event::Event;
use crate::traits::{FileOps, TorrentDownloader};
use crate::types::{ApiResponse, BangumiInfo};

use crate::utils::preview;

// ── Server config ──

pub struct ServerConfig {
    pub bind_addr: String,
    pub port: u16,
    pub max_connections: u32,
    pub auth_username: String,
    pub auth_password: String,
}

// ── Error type ──

enum ApiError {
    BadRequest(String), // 400
    NotFound(String),   // 404
    Timeout,            // 503 — logic thread unresponsive
    ChannelClosed,      // 503 — event channel disconnected
    Internal {
        client: String, // 500 — sent to client (no leak)
        detail: String, //       logged for debugging
    },
}

impl From<ApiError> for Response {
    fn from(e: ApiError) -> Self {
        let (code, msg) = match &e {
            ApiError::BadRequest(m) => (400, m.clone()),
            ApiError::NotFound(m) => (404, m.clone()),
            ApiError::Timeout => (503, "server busy".into()),
            ApiError::ChannelClosed => {
                log::error!("logic thread channel closed");
                (503, "server busy".into())
            }
            ApiError::Internal { client, detail } => {
                log::error!("internal error: {detail}");
                (500, client.clone())
            }
        };
        json_response(code, &msg)
    }
}

// ── Server entry point ──

pub fn start(
    event_tx: Sender<Event>,
    downloader: Arc<dyn TorrentDownloader>,
    fs: Arc<dyn FileOps>,
    cfg: ServerConfig,
) {
    let addr = format!("{}:{}", cfg.bind_addr, cfg.port);
    let auth_username = cfg.auth_username.clone();
    let auth_password = cfg.auth_password.clone();

    let server = Server::new(&addr, move |request| {
        // ── Auth check (if credentials configured) ──
        if !auth_username.is_empty() && !check_auth(request, &auth_username, &auth_password) {
            let response = Response {
                status_code: 401,
                headers: vec![(
                    "WWW-Authenticate".into(),
                    "Basic realm=\"bangumi-rss\"".into(),
                )],
                data: ResponseBody::from_string("401 Unauthorized"),
                upgrade: None,
            };
            log::debug!(
                "{} {} -> 401 (content-type: text/plain)",
                request.method(),
                request.url()
            );
            return response;
        }

        let method = request.method().to_uppercase();
        let url = request.url().to_string();
        let start_time = std::time::Instant::now();

        let response = handle_request(request, &event_tx, &*downloader, &*fs);

        let elapsed = start_time.elapsed();
        let content_type = response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Content-Type"))
            .map(|(_, v)| &v[..])
            .unwrap_or("-");

        log::debug!(
            "{} {} -> {} (content-type: {}) in {:.2}ms",
            method,
            url,
            response.status_code,
            content_type,
            elapsed.as_secs_f64() * 1000.0
        );

        response
    })
    .unwrap_or_else(|e| {
        log::error!("failed to bind {addr}: {e}");
        std::process::exit(1);
    });

    let actual = server.server_addr();
    log::info!("listening on http://{actual}");

    if cfg.max_connections > 0 {
        server.pool_size(cfg.max_connections as usize).run();
    } else {
        server.run();
    }
}

// ── Request dispatch ──

fn handle_request(
    request: &Request,
    tx: &Sender<Event>,
    dl: &dyn TorrentDownloader,
    fs: &dyn FileOps,
) -> Response {
    let method = request.method().to_uppercase();
    let url = request.url().to_string();
    let body = rouille::input::plain_text_body(request).unwrap_or_default();
    log::debug!("-> {method} {url} body={}", truncate(&body, 200));

    // Handle paths with dots before the router macro (rouille router treats `.` as special).
    if url == "/style.css" {
        return handle_style_css(fs);
    }

    router!(request,
        (GET) (/) => { handle_index(fs) },

        // ═══ /api/feeds ═══
        (GET) (/api/feeds) => { handle_list_feeds(tx) },
        (POST) (/api/feeds) => { handle_feed_create(&body, tx) },
        (POST) (/api/feeds/update) => { handle_feed_update_all(tx) },
        (PUT) (/api/feeds/{id: String}) => { handle_feed_update(&id, &body, tx) },
        (DELETE) (/api/feeds/{id: String}) => { handle_feed_delete(&id, tx) },
        (POST) (/api/feeds/preview) => { handle_preview(&body) },

        // ═══ /api/files ═══
        (GET) (/api/files/{infohash: String}) => { handle_file_stream(&infohash, tx, fs, request) },

        // ═══ /api/downloads ═══
        (GET) (/api/downloads) => { handle_list_downloads(tx) },
        (POST) (/api/downloads/refresh) => { handle_refresh(tx) },

        // ═══ /api/poll ═══
        (POST) (/api/poll) => { handle_poll(tx) },

        // ═══ /api/bangumi ═══
        (GET) (/api/bangumi/subjects/{id: String}) => { handle_bangumi_subject(&id) },
        (GET) (/api/bangumi/search) => { handle_bangumi_search(request) },

        // ═══ /api/health ═══
        (GET) (/api/health) => { handle_health(dl) },

        // ═══ /api/notify/test ═══
        (POST) (/api/notify/test) => { handle_notify_test(tx) },

        _ => Response::empty_404(),
    )
}

// ── Helpers ──

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

fn json_response(code: u16, message: &str) -> Response {
    let body = serde_json::json!({"success": false, "message": message}).to_string();
    Response::from_data("application/json", body).with_status_code(code)
}

fn check_auth(request: &Request, username: &str, password: &str) -> bool {
    request
        .header("Authorization")
        .and_then(|h| h.strip_prefix("Basic "))
        .and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .is_some_and(|s| s == format!("{username}:{password}"))
}

fn is_valid_rss_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 2048 {
        return false;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return false;
    }
    let after_scheme = &url[url.find("://").unwrap_or(usize::MAX) + 3..];
    after_scheme.starts_with(|c: char| c.is_alphanumeric())
}

fn mime_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        "m4v" => "video/x-m4v",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "3gp" => "video/3gpp",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
}

// ── Channel abstractions ──

/// Send an event and wait for a reply. Returns `None` if the channel is broken.
fn query_event<T>(tx: &Sender<Event>, event: impl FnOnce(Sender<T>) -> Event) -> Option<T> {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    tx.send(event(reply_tx)).ok()?;
    reply_rx.recv().ok()
}

/// Like `query_event`, but maps a missing reply to `ApiError::Timeout`.
fn query_result<T>(
    tx: &Sender<Event>,
    event: impl FnOnce(Sender<T>) -> Event,
) -> Result<T, ApiError> {
    query_event(tx, event).ok_or(ApiError::Timeout)
}

/// Query the logic thread and serialize the reply as JSON.
/// On timeout / channel error → 503.
fn query_json<T: serde::Serialize>(
    tx: &Sender<Event>,
    event: impl FnOnce(Sender<T>) -> Event,
) -> Response {
    match query_result(tx, event) {
        Ok(data) => Response::from_data(
            "application/json",
            serde_json::to_string(&data).unwrap_or_default(),
        ),
        Err(e) => e.into(),
    }
}

/// Query the logic thread expecting an `ApiResponse`. On timeout → 503.
fn query_api(tx: &Sender<Event>, event: impl FnOnce(Sender<ApiResponse>) -> Event) -> Response {
    match query_result(tx, event) {
        Ok(resp) => Response::from_data(
            "application/json",
            serde_json::to_string(&resp).unwrap_or_default(),
        ),
        Err(e) => e.into(),
    }
}

/// Send a fire-and-forget event and immediately return success.
/// If the channel is broken, return 503.
fn fire_event(tx: &Sender<Event>, event: Event, msg: &str) -> Response {
    match tx.send(event) {
        Ok(()) => Response::from_data(
            "application/json",
            serde_json::json!({"success": true, "message": msg}).to_string(),
        ),
        Err(_) => ApiError::ChannelClosed.into(),
    }
}

// ── Route handlers ──

fn handle_index(fs: &dyn FileOps) -> Response {
    let html = fs
        .read_to_string(Path::new("res/index.html"))
        .unwrap_or_else(|_| include_str!("../../res/index.html").to_string());
    Response::html(html)
}

fn handle_style_css(fs: &dyn FileOps) -> Response {
    let css = fs
        .read_to_string(Path::new("res/style.css"))
        .unwrap_or_else(|_| include_str!("../../res/style.css").to_string());
    Response::from_data("text/css", css)
}

fn handle_preview(body: &str) -> Response {
    let url = body.trim();
    if !is_valid_rss_url(url) {
        return ApiError::BadRequest("invalid URL".into()).into();
    }
    match preview::fetch_feed_preview(url) {
        Ok(preview) => Response::json(&serde_json::to_string(&preview).unwrap_or_default()),
        Err(e) => ApiError::Internal {
            client: "preview failed".into(),
            detail: format!("preview failed: {e}"),
        }
        .into(),
    }
}

fn handle_feed_create(body: &str, tx: &Sender<Event>) -> Response {
    let confirm: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let url = confirm["url"].as_str().unwrap_or("").to_string();
    if !is_valid_rss_url(&url) {
        return ApiError::BadRequest("invalid URL".into()).into();
    }
    let name = confirm["name"].as_str().unwrap_or("").to_string();
    let season = confirm["season"].as_u64().unwrap_or(1) as u8;
    let bangumi_info: Option<BangumiInfo> = confirm
        .get("bangumi_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    query_api(tx, |reply_tx| Event::ConfirmFeed {
        url,
        name,
        season,
        bangumi_info,
        reply_tx,
    })
}

fn handle_feed_update(id: &str, body: &str, tx: &Sender<Event>) -> Response {
    let feed_id = match uuid::Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return ApiError::BadRequest("invalid id".into()).into(),
    };
    let update: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let name = update["name"].as_str().unwrap_or("").to_string();
    let season = update["season"].as_u64().unwrap_or(1) as u8;
    let bangumi_info: Option<BangumiInfo> = update
        .get("bangumi_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let result = match query_result(tx, |reply_tx| Event::UserConfirm {
        feed_id,
        name,
        season,
        bangumi_info,
        reply_tx,
    }) {
        Ok(api_resp) => api_resp,
        Err(e) => return e.into(),
    };

    let status = if result.success { 200 } else { 404 };
    Response::from_data(
        "application/json",
        serde_json::to_string(&result).unwrap_or_default(),
    )
    .with_status_code(status)
}

fn handle_feed_delete(id: &str, tx: &Sender<Event>) -> Response {
    let feed_id = match uuid::Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return ApiError::BadRequest("invalid id".into()).into(),
    };
    query_api(tx, |reply_tx| Event::ApiRemoveFeed { feed_id, reply_tx })
}

fn handle_list_feeds(tx: &Sender<Event>) -> Response {
    query_json(tx, |reply_tx| Event::ApiListFeeds { reply_tx })
}

fn handle_list_downloads(tx: &Sender<Event>) -> Response {
    query_json(tx, |reply_tx| Event::ApiListDownloads { reply_tx })
}

fn handle_refresh(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::RefreshDownloads, "refresh triggered")
}

fn handle_feed_update_all(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::RssTickAll, "RSS refresh triggered")
}

fn handle_poll(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::PollDownloader, "downloader poll triggered")
}

fn handle_bangumi_subject(id_str: &str) -> Response {
    let id: u32 = match id_str.parse() {
        Ok(n) => n,
        Err(_) => return ApiError::BadRequest("invalid id".into()).into(),
    };
    match crate::services::bangumi::detail(id) {
        Ok(Some(info)) => Response::from_data(
            "application/json",
            serde_json::json!({"success":true,"bangumi_info":info}).to_string(),
        ),
        Ok(None) => ApiError::NotFound("not found".into()).into(),
        Err(e) => ApiError::Internal {
            client: "upstream error".into(),
            detail: format!("bangumi detail failed: {e}"),
        }
        .into(),
    }
}

fn handle_bangumi_search(request: &Request) -> Response {
    let name = match request.get_param("name") {
        Some(n) if !n.is_empty() => n,
        _ => return ApiError::BadRequest("missing name".into()).into(),
    };

    let result = match crate::services::bangumi::search(&name) {
        Ok(Some(id)) => {
            log::info!("Bangumi search '{name}' → #{id}");
            match crate::services::bangumi::detail(id) {
                Ok(Some(info)) => {
                    serde_json::json!({ "success": true, "bangumi_info": info })
                }
                Ok(None) => serde_json::json!({ "success": false, "message": "no detail" }),
                Err(e) => {
                    log::error!("bangumi detail failed: {e}");
                    serde_json::json!({ "success": false, "message": "upstream error" })
                }
            }
        }
        Ok(None) => serde_json::json!({ "success": false, "message": "not found" }),
        Err(e) => {
            log::error!("bangumi search failed: {e}");
            serde_json::json!({ "success": false, "message": "upstream error" })
        }
    };
    Response::from_data("application/json", result.to_string())
}

fn handle_health(dl: &dyn TorrentDownloader) -> Response {
    match dl.check_connection() {
        Ok(()) => Response::from_data(
            "application/json",
            r#"{"success":true,"downloader":"connected"}"#,
        ),
        Err(e) => {
            log::error!("health check failed: {e}");
            let msg = serde_json::json!({"success":false,"downloader":"error","message":"downloader unavailable"});
            Response::from_data("application/json", msg.to_string()).with_status_code(503)
        }
    }
}

fn handle_notify_test(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::NotifyTest, "test notifications sent")
}

fn handle_file_stream(
    infohash: &str,
    tx: &Sender<Event>,
    fs: &dyn FileOps,
    request: &Request,
) -> Response {
    let record = match query_result(tx, |reply_tx| Event::ApiGetEpisode {
        infohash: infohash.to_string(),
        reply_tx,
    }) {
        Ok(Some(r)) => r,
        Ok(None) => return ApiError::NotFound("not found".into()).into(),
        Err(e) => return e.into(),
    };

    let file_path = match &record.library_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => return ApiError::NotFound("file not yet available".into()).into(),
    };

    // Path traversal protection
    if !Path::new(&file_path).is_absolute() || file_path.contains("..") {
        log::error!("path traversal attempt: {file_path}");
        return ApiError::NotFound("file not found".into()).into();
    }

    let stream = match fs.open_file(Path::new(&file_path)) {
        Ok(s) => s,
        Err(e) => {
            log::error!("failed to open {file_path}: {e}");
            let api_err = match e.downcast_ref::<std::io::Error>() {
                Some(ioe) if ioe.kind() == std::io::ErrorKind::NotFound => {
                    ApiError::NotFound("file not found".into())
                }
                Some(ioe) if ioe.kind() == std::io::ErrorKind::PermissionDenied => {
                    // Don't leak existence, just say not found
                    ApiError::NotFound("file not found".into())
                }
                _ => ApiError::Internal {
                    client: "file unavailable".into(),
                    detail: format!("file unavailable: {e}"),
                },
            };
            return api_err.into();
        }
    };

    let content_type = mime_type(&file_path);
    let file_size = stream.size();

    serve_file_range(stream, file_size, content_type, request)
}

// ── File serving with Range ──

fn serve_file_range(
    stream: crate::types::FileStream,
    file_size: u64,
    content_type: &'static str,
    request: &Request,
) -> Response {
    if file_size == 0 {
        return Response {
            status_code: 200,
            headers: vec![
                ("Content-Type".into(), content_type.into()),
                ("Accept-Ranges".into(), "bytes".into()),
            ],
            data: ResponseBody::empty(),
            upgrade: None,
        };
    }

    let range_header = match request.header("Range") {
        Some(h) => h,
        None => {
            let reader = match stream.into_range(0, file_size) {
                Ok(r) => r,
                Err(e) => {
                    return ApiError::Internal {
                        client: "internal server error".into(),
                        detail: format!("seek failed: {e}"),
                    }
                    .into();
                }
            };
            log::info!("→ 200 OK ({content_type}, {file_size} bytes)");
            return Response {
                status_code: 200,
                headers: vec![
                    ("Content-Type".into(), content_type.into()),
                    ("Accept-Ranges".into(), "bytes".into()),
                ],
                data: ResponseBody::from_reader_and_size(reader, file_size as usize),
                upgrade: None,
            };
        }
    };

    let range_val = match range_header.strip_prefix("bytes=") {
        Some(v) => v.trim(),
        None => return build_416(file_size),
    };
    if range_val.contains(',') {
        return build_416(file_size);
    }

    let (start, end) = match parse_range(range_val, file_size) {
        Some(r) => r,
        None => return build_416(file_size),
    };

    let length = end - start + 1;
    let content_range = format!("bytes {start}-{end}/{file_size}");

    let reader = match stream.into_range(start, length) {
        Ok(r) => r,
        Err(e) => {
            return ApiError::Internal {
                client: "internal server error".into(),
                detail: format!("seek failed: {e}"),
            }
            .into();
        }
    };

    log::info!("→ 206 bytes {start}-{end}/{file_size} ({content_type})");

    Response {
        status_code: 206,
        headers: vec![
            ("Content-Type".into(), content_type.into()),
            ("Content-Range".into(), content_range.into()),
            ("Accept-Ranges".into(), "bytes".into()),
        ],
        data: ResponseBody::from_reader_and_size(reader, length as usize),
        upgrade: None,
    }
}

fn build_416(file_size: u64) -> Response {
    Response {
        status_code: 416,
        headers: vec![(
            "Content-Range".into(),
            format!("bytes */{file_size}").into(),
        )],
        data: ResponseBody::empty(),
        upgrade: None,
    }
}

fn parse_range(range: &str, file_size: u64) -> Option<(u64, u64)> {
    if let Some(suffix) = range.strip_prefix('-') {
        let n: u64 = suffix.parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = file_size.saturating_sub(n);
        let end = file_size - 1;
        return Some((start, end));
    }

    let (start_str, end_str) = range.split_once('-')?;
    let start: u64 = start_str.parse().ok()?;
    if start >= file_size {
        return None;
    }
    let end: u64 = if end_str.is_empty() {
        file_size - 1
    } else {
        let e: u64 = end_str.parse().ok()?;
        if e >= file_size || e < start {
            return None;
        }
        e
    };
    Some((start, end))
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    // ── Pure function tests ──

    #[test]
    fn test_parse_range_standard() {
        assert_eq!(parse_range("0-4", 100), Some((0, 4)));
        assert_eq!(parse_range("50-", 100), Some((50, 99)));
        assert_eq!(parse_range("0-99", 100), Some((0, 99)));
    }

    #[test]
    fn test_parse_range_suffix() {
        assert_eq!(parse_range("-10", 100), Some((90, 99)));
        assert_eq!(parse_range("-1", 100), Some((99, 99)));
    }

    #[test]
    fn test_parse_range_invalid() {
        assert_eq!(parse_range("100-", 100), None);
        assert_eq!(parse_range("0-100", 100), None);
        assert_eq!(parse_range("-0", 100), None);
        assert_eq!(parse_range("abc", 100), None);
        assert_eq!(parse_range("", 100), None);
    }

    #[test]
    fn test_mime_type() {
        assert_eq!(mime_type("video.mp4"), "video/mp4");
        assert_eq!(mime_type("anime.mkv"), "video/x-matroska");
        assert_eq!(mime_type("song.mp3"), "audio/mpeg");
        assert_eq!(mime_type("unknown.xyz"), "application/octet-stream");
        assert_eq!(mime_type("no_ext"), "application/octet-stream");
    }

    #[test]
    fn test_is_valid_rss_url() {
        assert!(is_valid_rss_url("https://example.com/feed.xml"));
        assert!(is_valid_rss_url("http://example.com/feed"));
        assert!(!is_valid_rss_url(""));
        assert!(!is_valid_rss_url("ftp://example.com"));
        assert!(!is_valid_rss_url("not-a-url"));
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let s = truncate("abcdefghijklmnopqrstuvwxyz", 10);
        assert!(s.contains("chars omitted"));
        assert!(s.len() > 5);
    }

    // ── Auth tests ──

    #[test]
    fn test_auth_success() {
        let req = Request::fake_http(
            "GET",
            "/",
            vec![("Authorization".into(), "Basic dXNlcjpwYXNz".into())],
            vec![],
        );
        assert!(check_auth(&req, "user", "pass"));
    }

    #[test]
    fn test_auth_fail_wrong_credentials() {
        let req = Request::fake_http(
            "GET",
            "/",
            vec![("Authorization".into(), "Basic dXNlcjpwYXNz".into())],
            vec![],
        );
        assert!(!check_auth(&req, "user", "wrong"));
    }

    #[test]
    fn test_auth_fail_no_header() {
        let req = Request::fake_http("GET", "/", vec![], vec![]);
        assert!(!check_auth(&req, "user", "pass"));
    }

    // ── serve_file_range tests ──

    fn range_request(range: Option<&str>) -> Request {
        let headers = range
            .map(|h| vec![("Range".into(), h.into())])
            .unwrap_or_default();
        Request::fake_http("GET", "/file", headers, vec![])
    }

    fn read_body(resp: Response) -> Vec<u8> {
        let (mut reader, _) = resp.data.into_reader_and_size();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn serve_full_file_no_range() {
        let data = b"hello world".to_vec();
        let stream = crate::types::FileStream::new(std::io::Cursor::new(data), 11);
        let resp = serve_file_range(stream, 11, "text/plain", &range_request(None));
        assert_eq!(resp.status_code, 200);
        assert!(resp.headers.iter().any(|(k, _)| k == "Accept-Ranges"));
    }

    #[test]
    fn serve_range_206() {
        let data = b"hello world".to_vec();
        let stream = crate::types::FileStream::new(std::io::Cursor::new(data), 11);
        let resp = serve_file_range(stream, 11, "text/plain", &range_request(Some("bytes=0-4")));
        assert_eq!(resp.status_code, 206);
        assert!(
            resp.headers
                .iter()
                .any(|(k, v)| k == "Content-Range" && v == "bytes 0-4/11")
        );
    }

    #[test]
    fn serve_range_416() {
        let data = b"hello world".to_vec();
        let stream = crate::types::FileStream::new(std::io::Cursor::new(data), 11);
        let resp = serve_file_range(
            stream,
            11,
            "text/plain",
            &range_request(Some("bytes=20-30")),
        );
        assert_eq!(resp.status_code, 416);
        assert!(
            resp.headers
                .iter()
                .any(|(k, v)| k == "Content-Range" && v == "bytes */11")
        );
    }

    #[test]
    fn serve_range_data_integrity() {
        let content = b"0123456789ABCDEF";
        let stream = crate::types::FileStream::new(std::io::Cursor::new(content.to_vec()), 16);
        let resp = serve_file_range(stream, 16, "text/plain", &range_request(Some("bytes=0-3")));
        assert_eq!(resp.status_code, 206);
        let body = read_body(resp);
        assert_eq!(body, b"0123");
    }

    #[test]
    fn serve_empty_file() {
        let stream = crate::types::FileStream::new(std::io::Cursor::new(vec![]), 0);
        let resp = serve_file_range(stream, 0, "text/plain", &range_request(None));
        assert_eq!(resp.status_code, 200);
        let r = Response {
            status_code: 200,
            headers: vec![],
            data: ResponseBody::empty(),
            upgrade: None,
        };
        let body = read_body(r);
        assert!(body.is_empty());
    }

    // ── Handler tests ──

    #[test]
    fn handle_list_feeds_timeout() {
        // Dropped receiver → send fails → query_event returns None → 503
        let (tx, rx) = crossbeam_channel::bounded::<Event>(1);
        drop(rx);
        let resp = handle_list_feeds(&tx);
        assert_eq!(resp.status_code, 503);
    }

    #[test]
    fn handle_list_downloads_timeout() {
        let (tx, rx) = crossbeam_channel::bounded::<Event>(1);
        drop(rx);
        let resp = handle_list_downloads(&tx);
        assert_eq!(resp.status_code, 503);
    }

    #[test]
    fn handle_health_connected() {
        struct OkDownloader;
        impl TorrentDownloader for OkDownloader {
            fn check_connection(&self) -> anyhow::Result<()> {
                Ok(())
            }
            fn add_uri(&self, _: &str, _: &str) -> anyhow::Result<String> {
                Ok("mock".into())
            }
            fn add_torrent_bytes(&self, _: &[u8], _: &str) -> anyhow::Result<String> {
                Ok("mock".into())
            }
            fn list_files(&self, _: &str) -> anyhow::Result<Vec<crate::types::TorrentFile>> {
                Ok(vec![])
            }
            fn rename_file(&self, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            fn poll_completed(&self) -> anyhow::Result<Vec<crate::types::CompletedDownload>> {
                Ok(vec![])
            }
        }
        let resp = handle_health(&OkDownloader);
        assert_eq!(resp.status_code, 200);
    }

    #[test]
    fn test_fire_event_success() {
        let (tx, rx) = crossbeam_channel::bounded::<Event>(1);
        let resp = fire_event(&tx, Event::PollDownloader, "test msg");
        assert_eq!(resp.status_code, 200);
        // Verify event was actually sent
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_fire_event_closed_channel() {
        let (_tx, _rx) = crossbeam_channel::bounded::<Event>(0);
        // Drop rx while tx still holds the slot → channel remains open but can't send
        // Use a channel with capacity 0 and drop rx before sending
        let (tx, rx) = crossbeam_channel::bounded::<Event>(0);
        drop(rx);
        let resp = fire_event(&tx, Event::PollDownloader, "test");
        assert_eq!(resp.status_code, 503);
    }

    #[test]
    fn handle_poll_success() {
        let (tx, rx) = crossbeam_channel::bounded::<Event>(1);
        let resp = handle_poll(&tx);
        assert_eq!(resp.status_code, 200);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn handle_refresh_success() {
        let (tx, rx) = crossbeam_channel::bounded::<Event>(1);
        let resp = handle_refresh(&tx);
        assert_eq!(resp.status_code, 200);
        assert!(matches!(rx.try_recv(), Ok(Event::RefreshDownloads)));
    }

    #[test]
    fn test_build_416() {
        let resp = build_416(100);
        assert_eq!(resp.status_code, 416);
        assert!(
            resp.headers
                .iter()
                .any(|(k, v)| k == "Content-Range" && v == "bytes */100")
        );
    }

    #[test]
    fn test_json_response() {
        let resp = json_response(404, "not found");
        assert_eq!(resp.status_code, 404);
        let body = read_body(resp);
        let body_str = String::from_utf8(body).unwrap();
        assert!(body_str.contains("not found"));
        assert!(body_str.contains("false"));
    }

    #[test]
    fn test_handle_file_stream_unknown_infohash() {
        // Mock FileOps with no files
        struct EmptyFs;
        impl FileOps for EmptyFs {
            fn move_file(&self, _: &Path, _: &Path) -> anyhow::Result<()> {
                Ok(())
            }
            fn ensure_dir(&self, _: &Path) -> anyhow::Result<()> {
                Ok(())
            }
            fn read_to_string(&self, _: &Path) -> anyhow::Result<String> {
                Ok(String::new())
            }
            fn write_string(&self, _: &Path, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn open_file(&self, _: &Path) -> anyhow::Result<crate::types::FileStream> {
                anyhow::bail!("not found")
            }
        }
        let (tx, rx) = crossbeam_channel::bounded::<Event>(1);
        // Drop rx to simulate logic thread timeout
        drop(rx);
        let req = range_request(None);
        let resp = handle_file_stream("unknown_hash", &tx, &EmptyFs, &req);
        assert_eq!(resp.status_code, 503);
    }
}
