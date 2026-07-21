use crate::event::{ApiResponse, Event};
use crate::types::BangumiInfo;
use crate::util;
use crossbeam_channel::Sender;
use std::net::SocketAddr;
use std::sync::Arc;

/// Start the HTTP API + confirmation web app.
pub fn start(event_tx: Sender<Event>) {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7893);

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let server = tiny_http::Server::http(addr).expect("failed to start HTTP server");
    println!("[http] listening on http://{addr}");

    let tx = Arc::new(event_tx);

    for mut request in server.incoming_requests() {
        let tx = tx.clone();
        match request.url() {
            "/" => {
                let _ = request.respond(
                    tiny_http::Response::from_string(CONFIRM_PAGE).with_header(
                        "Content-Type: text/html; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
            }
            "/api/feeds/preview" if request.method() == &tiny_http::Method::Post => {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let url = body.trim().to_string();

                let preview = util::fetch_feed_preview(&url).unwrap_or_default();
                let json = serde_json::to_string(&preview).unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(json).with_header(
                        "Content-Type: application/json; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
            }
            "/api/feeds/confirm" if request.method() == &tiny_http::Method::Post => {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let confirm: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let url = confirm["url"].as_str().unwrap_or("").to_string();
                let name = confirm["name"].as_str().unwrap_or("").to_string();
                let season = confirm["season"].as_u64().unwrap_or(1) as u8;
                let bangumi_info: Option<BangumiInfo> = confirm
                    .get("bangumi_info")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());

                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                tx.send(Event::ConfirmFeed {
                    url,
                    name,
                    season,
                    bangumi_info,
                    reply_tx,
                })
                .ok();
                let result = reply_rx.recv().unwrap_or(ApiResponse {
                    success: false,
                    message: "timeout".into(),
                });
                let json = serde_json::to_string(&result).unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(json).with_header(
                        "Content-Type: application/json; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
            }
            "/api/feeds/update" if request.method() == &tiny_http::Method::Post => {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let update: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let feed_id = uuid::Uuid::parse_str(update["id"].as_str().unwrap_or(""));
                let name = update["name"].as_str().unwrap_or("").to_string();
                let season = update["season"].as_u64().unwrap_or(1) as u8;

                match feed_id {
                    Ok(id) => {
                        tx.send(Event::UserConfirm {
                            feed_id: id,
                            name,
                            season,
                        })
                        .ok();
                        let _ = request.respond(
                            tiny_http::Response::from_string(
                                "{\"success\":true,\"message\":\"updated\"}",
                            )
                            .with_header(
                                "Content-Type: application/json; charset=utf-8"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            ),
                        );
                    }
                    Err(_) => {
                        let _ = request.respond(
                            tiny_http::Response::from_string(
                                "{\"success\":false,\"message\":\"invalid id\"}",
                            )
                            .with_status_code(400)
                            .with_header(
                                "Content-Type: application/json; charset=utf-8"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            ),
                        );
                    }
                }
            }
            _ if request.url().starts_with("/api/feeds")
                && request.method() == &tiny_http::Method::Delete =>
            {
                let query = request.url().to_string();
                let feed_id = query
                    .split("?id=")
                    .nth(1)
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());

                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                match feed_id {
                    Some(id) => {
                        tx.send(Event::ApiRemoveFeed {
                            feed_id: id,
                            reply_tx,
                        })
                        .ok();
                        let result = reply_rx.recv().unwrap_or(ApiResponse {
                            success: false,
                            message: "timeout".into(),
                        });
                        let json = serde_json::to_string(&result).unwrap();
                        let _ = request.respond(
                            tiny_http::Response::from_string(json).with_header(
                                "Content-Type: application/json; charset=utf-8"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            ),
                        );
                    }
                    None => {
                        let _ = request.respond(
                            tiny_http::Response::from_string(
                                "{\"success\":false,\"message\":\"invalid id\"}",
                            )
                            .with_status_code(400)
                            .with_header(
                                "Content-Type: application/json; charset=utf-8"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            ),
                        );
                    }
                }
            }
            "/api/feeds" => {
                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                tx.send(Event::ApiListFeeds { reply_tx }).ok();
                let feeds = reply_rx.recv().unwrap_or_default();
                let json = serde_json::to_string(&feeds).unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(json).with_header(
                        "Content-Type: application/json; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
            }
            "/api/downloads" => {
                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                tx.send(Event::ApiListDownloads { reply_tx }).ok();
                let downloads = reply_rx.recv().unwrap_or_default();
                let json = serde_json::to_string(&downloads).unwrap();
                let _ = request.respond(
                    tiny_http::Response::from_string(json).with_header(
                        "Content-Type: application/json; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
            }
            "/api/downloads/refresh" if request.method() == &tiny_http::Method::Post => {
                tx.send(Event::RefreshDownloads).ok();
                let _ = request.respond(
                    tiny_http::Response::from_string(
                        "{\"success\":true,\"message\":\"refresh triggered\"}",
                    )
                    .with_header(
                        "Content-Type: application/json; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
            }
            _ => {
                let _ =
                    request.respond(tiny_http::Response::from_string("404").with_status_code(404));
            }
        }
    }
}

const CONFIRM_PAGE: &str = include_str!("confirm.html");
