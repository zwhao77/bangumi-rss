use crate::core::event::Event;
use crate::types::{ApiResponse, BangumiInfo};
use crate::utils::preview;
use crossbeam_channel::Sender;
use std::net::SocketAddr;
use std::sync::Arc;
use tiny_http::{Method, Response};

const JSON_TYPE: &str = "Content-Type: application/json; charset=utf-8";
const HTML_TYPE: &str = "Content-Type: text/html; charset=utf-8";

/// Start the HTTP API + confirmation web app.
pub fn start(event_tx: Sender<Event>) {
    let preferred: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7893);

    let server = try_bind(preferred).unwrap_or_else(|| {
        eprintln!("[http] port {preferred} unavailable, trying OS-assigned port");
        try_bind(0).unwrap_or_else(|| {
            eprintln!("[http] fatal: failed to bind any port");
            std::process::exit(1);
        })
    });

    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(preferred);
    println!("[http] listening on http://127.0.0.1:{actual_port}");

    let tx = Arc::new(event_tx);

    for request in server.incoming_requests() {
        let tx = tx.clone();
        let url = request.url().to_string();
        let method = request.method().clone();
        let result = match (&*url, &method) {
            ("/", _) => handle_index(request),
            ("/api/feeds/preview", &Method::Post) => handle_preview(request),
            ("/api/feeds/confirm", &Method::Post) => handle_confirm(request, &tx),
            ("/api/feeds/update", &Method::Post) => handle_feed_update(request, &tx),
            (u, &Method::Delete) if u.starts_with("/api/feeds") => handle_feed_delete(request, &tx),
            ("/api/feeds", _) => handle_list_feeds(request, &tx),
            ("/api/downloads", _) => handle_list_downloads(request, &tx),
            ("/api/downloads/refresh", &Method::Post) => handle_refresh(request, &tx),
            (u, _) if u.starts_with("/api/bangumi/image") => handle_image_proxy(request, u),
            (u, _) if u.starts_with("/api/bangumi/search") => handle_bangumi_search(request, u),
            _ => {
                let _ =
                    request.respond(tiny_http::Response::from_string("404").with_status_code(404));
                Ok(())
            }
        };
        if let Err(e) = result {
            eprintln!("[http] request error: {:?}", e);
        }
    }
}

// ── Response helpers ──

/// Try to bind to a specific port. Returns `None` if the port is unavailable.
fn try_bind(port: u16) -> Option<tiny_http::Server> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    tiny_http::Server::http(addr).ok()
}

fn respond(req: tiny_http::Request, code: u16, body: &str) -> Result<(), ()> {
    req.respond(
        Response::from_string(body)
            .with_status_code(code)
            .with_header(JSON_TYPE.parse::<tiny_http::Header>().unwrap()),
    )
    .map_err(|_| ())
}

fn respond_ok(req: tiny_http::Request, json: &str) -> Result<(), ()> {
    respond(req, 200, json)
}

// ── Route handlers ──

fn handle_index(req: tiny_http::Request) -> Result<(), ()> {
    req.respond(
        Response::from_string(CONFIRM_PAGE)
            .with_header(HTML_TYPE.parse::<tiny_http::Header>().unwrap()),
    )
    .map_err(|_| ())
}

fn handle_preview(mut req: tiny_http::Request) -> Result<(), ()> {
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    let url = body.trim().to_string();

    let preview = preview::fetch_feed_preview(&url).unwrap_or_default();
    let json = serde_json::to_string(&preview).unwrap_or_default();
    respond_ok(req, &json)
}

fn handle_confirm(mut req: tiny_http::Request, tx: &Sender<Event>) -> Result<(), ()> {
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    let confirm: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
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
    respond_ok(req, &json)
}

fn handle_feed_update(mut req: tiny_http::Request, tx: &Sender<Event>) -> Result<(), ()> {
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    let update: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let feed_id = uuid::Uuid::parse_str(update["id"].as_str().unwrap_or(""));
    let name = update["name"].as_str().unwrap_or("").to_string();
    let season = update["season"].as_u64().unwrap_or(1) as u8;

    match feed_id {
        Ok(id) => {
            let _ = tx.send(Event::UserConfirm {
                feed_id: id,
                name,
                season,
            });
            respond_ok(req, "{\"success\":true,\"message\":\"updated\"}")
        }
        Err(_) => respond(req, 400, "{\"success\":false,\"message\":\"invalid id\"}"),
    }
}

fn handle_feed_delete(req: tiny_http::Request, tx: &Sender<Event>) -> Result<(), ()> {
    let query = req.url().to_string();
    let feed_id = query
        .split("?id=")
        .nth(1)
        .and_then(|s| uuid::Uuid::parse_str(s).ok());

    match feed_id {
        Some(id) => {
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            let _ = tx.send(Event::ApiRemoveFeed {
                feed_id: id,
                reply_tx,
            });
            let result = reply_rx.recv().unwrap_or(ApiResponse {
                success: false,
                message: "timeout".into(),
            });
            let json = serde_json::to_string(&result).unwrap_or_default();
            respond_ok(req, &json)
        }
        None => respond(req, 400, "{\"success\":false,\"message\":\"invalid id\"}"),
    }
}

fn handle_list_feeds(req: tiny_http::Request, tx: &Sender<Event>) -> Result<(), ()> {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::ApiListFeeds { reply_tx });
    let feeds = reply_rx.recv().unwrap_or_default();
    let json = serde_json::to_string(&feeds).unwrap_or_default();
    respond_ok(req, &json)
}

fn handle_list_downloads(req: tiny_http::Request, tx: &Sender<Event>) -> Result<(), ()> {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(Event::ApiListDownloads { reply_tx });
    let downloads = reply_rx.recv().unwrap_or_default();
    let json = serde_json::to_string(&downloads).unwrap_or_default();
    respond_ok(req, &json)
}

fn handle_refresh(req: tiny_http::Request, tx: &Sender<Event>) -> Result<(), ()> {
    let _ = tx.send(Event::RefreshDownloads);
    respond_ok(req, "{\"success\":true,\"message\":\"refresh triggered\"}")
}

/// Proxy Bangumi cover images through the backend so they work behind proxies.
/// GET /api/bangumi/image?url=<encoded_url>
fn handle_image_proxy(req: tiny_http::Request, path: &str) -> Result<(), ()> {
    let encoded = path.strip_prefix("/api/bangumi/image?url=").unwrap_or("");
    let img_url = percent_decode(encoded);

    match ureq::get(&img_url).call() {
        Ok(resp) => {
            let ct = resp.content_type().to_string();
            let mut buf = Vec::new();
            if resp.into_reader().read_to_end(&mut buf).is_ok() {
                let _ = req.respond(
                    tiny_http::Response::from_data(buf).with_header(
                        format!("Content-Type: {ct}")
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
                return Ok(());
            }
        }
        Err(e) => eprintln!("[http] image proxy failed for {img_url}: {e}"),
    }
    let _ = req.respond(tiny_http::Response::from_string("").with_status_code(404));
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hi = iter.next().and_then(|b| (b as char).to_digit(16)).unwrap_or(0) as u8;
            let lo = iter.next().and_then(|b| (b as char).to_digit(16)).unwrap_or(0) as u8;
            bytes.push(hi << 4 | lo);
        } else if b == b'+' {
            bytes.push(b' ');
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// GET /api/bangumi/search?name=<url_encoded>
/// Re-search Bangumi by name (for user corrections).
fn handle_bangumi_search(req: tiny_http::Request, path: &str) -> Result<(), ()> {
    let name = path
        .strip_prefix("/api/bangumi/search?name=")
        .map(percent_decode)
        .unwrap_or_default();

    if name.is_empty() {
        return respond(req, 400, r#"{"success":false,"message":"missing name"}"#);
    }

    let result = match crate::services::bangumi::search(&name) {
        Ok(Some(id)) => {
            println!("[http] Bangumi search '{name}' → #{id}");
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

    respond_ok(req, &result.to_string())
}

const CONFIRM_PAGE: &str = include_str!("../../res/confirm.html");
