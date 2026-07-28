//! Route handlers — pure functions that take trait objects and return responses.
//!
//! Each handler receives the dependencies it needs (channels, service traits, etc.)
//! and produces a `rouille::Response`.  No I/O is performed directly — all side
//! effects go through the injected traits.

use crossbeam_channel::{RecvTimeoutError, Sender};
use rouille::{Request, Response};
use std::path::Path;
use std::time::Duration;

use crate::core::event::Event;
use crate::services::server::range::{resolve_range, serve_file_range};
use crate::services::server::utils::{is_valid_rss_url, json_response};
use crate::traits::{FileOps, TorrentDownloader};
use crate::types::{ApiResult, BangumiInfo, http_code};

use crate::utils::preview;

enum HandlerError {
    BadRequest(String), // 400
    NotFound(String),   // 404
    Timeout,            // 503 — logic thread unresponsive
    ChannelClosed,      // 503 — event channel disconnected
    Internal {
        client: String, // 500 — sent to client (no leak)
        detail: String, //       logged for debugging
    },
}

impl From<HandlerError> for Response {
    fn from(e: HandlerError) -> Self {
        let (code, msg) = match &e {
            HandlerError::BadRequest(m) => (http_code::BAD_REQUEST, m.clone()),
            HandlerError::NotFound(m) => (http_code::NOT_FOUND, m.clone()),
            HandlerError::Timeout => (http_code::SERVICE_UNAVAILABLE, "server busy".into()),
            HandlerError::ChannelClosed => {
                log::error!("logic thread channel closed");
                (http_code::SERVICE_UNAVAILABLE, "server busy".into())
            }
            HandlerError::Internal { client, detail } => {
                log::error!("internal error: {detail}");
                (http_code::INTERNAL, client.clone())
            }
        };
        json_response(code, &msg)
    }
}

impl<T: serde::Serialize> From<ApiResult<T>> for Response {
    fn from(result: ApiResult<T>) -> Self {
        let body = serde_json::to_string(&result).unwrap_or_default();
        let status = match &result {
            ApiResult::OK { .. } => 200,
            ApiResult::Err { code, .. } => *code,
        };
        Response::from_data("application/json", body).with_status_code(status)
    }
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

/// Query the logic thread and wait for a reply (up to 10 s).
/// - Channel disconnected → `HandlerError::ChannelClosed`
/// - Logic thread unresponsive → `HandlerError::Timeout`
/// - No reply received → `HandlerError::Internal("no reply")`
fn query_result<T>(
    tx: &Sender<Event>,
    event: impl FnOnce(Sender<T>) -> Event,
) -> Result<T, HandlerError> {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    tx.send(event(reply_tx))
        .map_err(|_| HandlerError::ChannelClosed)?;
    match reply_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(val) => Ok(val),
        Err(RecvTimeoutError::Timeout) => Err(HandlerError::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(HandlerError::Internal {
            client: "logic thread error".into(),
            detail: "logic thread dropped reply channel without sending".into(),
        }),
    }
}

/// Query the logic thread for an `ApiResult<T>` — uses `Err.code` as HTTP status.
/// On channel timeout / disconnect → 503.
fn query_api_result<T: serde::Serialize>(
    tx: &Sender<Event>,
    event: impl FnOnce(Sender<ApiResult<T>>) -> Event,
) -> Response {
    match query_result(tx, event) {
        Ok(result) => result.into(),
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
        Err(_) => HandlerError::ChannelClosed.into(),
    }
}

// ── Route handlers ──

pub fn handle_index(fs: &dyn FileOps) -> Response {
    let html = fs
        .read_to_string(Path::new("res/index.html"))
        .unwrap_or_else(|_| include_str!("../../../res/index.html").to_string());
    Response::html(html)
}

pub fn handle_style_css(fs: &dyn FileOps) -> Response {
    let css = fs
        .read_to_string(Path::new("res/style.css"))
        .unwrap_or_else(|_| include_str!("../../../res/style.css").to_string());
    Response::from_data("text/css", css)
}

pub fn handle_preview(body: &str) -> Response {
    let url = body.trim();
    if !is_valid_rss_url(url) {
        return ApiResult::<crate::types::FeedPreview>::Err {
            code: http_code::BAD_REQUEST,
            message: "invalid URL".into(),
        }
        .into();
    }
    match preview::fetch_feed_preview(url) {
        Ok(preview) => ApiResult::<crate::types::FeedPreview>::OK { value: preview }.into(),
        Err(e) => ApiResult::<crate::types::FeedPreview>::Err {
            code: http_code::INTERNAL,
            message: format!("preview failed: {e}"),
        }
        .into(),
    }
}

pub fn handle_feed_create(body: &str, tx: &Sender<Event>) -> Response {
    let confirm: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let url = confirm["url"].as_str().unwrap_or("").to_string();
    if !is_valid_rss_url(&url) {
        return HandlerError::BadRequest("invalid URL".into()).into();
    }
    let name = confirm["name"].as_str().unwrap_or("").to_string();
    let season = confirm["season"].as_u64().unwrap_or(1) as u8;
    let bangumi_info: Option<BangumiInfo> = confirm
        .get("bangumi_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    query_api_result(tx, |reply_tx| Event::ConfirmFeed {
        url,
        name,
        season,
        bangumi_info,
        reply_tx,
    })
}

pub fn handle_feed_update(id: &str, body: &str, tx: &Sender<Event>) -> Response {
    let feed_id = match uuid::Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return HandlerError::BadRequest("invalid id".into()).into(),
    };
    let update: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let name = update["name"].as_str().unwrap_or("").to_string();
    let season = update["season"].as_u64().unwrap_or(1) as u8;
    let bangumi_info: Option<BangumiInfo> = update
        .get("bangumi_info")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    query_api_result(tx, |reply_tx| Event::UserConfirm {
        feed_id,
        name,
        season,
        bangumi_info,
        reply_tx,
    })
}

pub fn handle_feed_delete(id: &str, tx: &Sender<Event>) -> Response {
    let feed_id = match uuid::Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return HandlerError::BadRequest("invalid id".into()).into(),
    };
    query_api_result(tx, |reply_tx| Event::ApiRemoveFeed { feed_id, reply_tx })
}

pub fn handle_list_feeds(tx: &Sender<Event>) -> Response {
    query_api_result(tx, |reply_tx| Event::ApiListFeeds { reply_tx })
}

pub fn handle_list_downloads(tx: &Sender<Event>) -> Response {
    query_api_result(tx, |reply_tx| Event::ApiListDownloads { reply_tx })
}

pub fn handle_refresh(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::RefreshDownloads, "refresh triggered")
}

pub fn handle_feed_update_all(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::RssTickAll, "RSS refresh triggered")
}

pub fn handle_poll(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::PollDownloader, "downloader poll triggered")
}

pub fn handle_bangumi_subject(id_str: &str) -> Response {
    let id: u32 = match id_str.parse() {
        Ok(n) => n,
        Err(_) => return HandlerError::BadRequest("invalid id".into()).into(),
    };
    match crate::services::bangumi::detail(id) {
        Ok(Some(info)) => ApiResult::OK { value: info }.into(),
        Ok(None) => ApiResult::<BangumiInfo>::Err {
            code: http_code::NOT_FOUND,
            message: "not found".into(),
        }
        .into(),
        Err(e) => HandlerError::Internal {
            client: "upstream error".into(),
            detail: format!("bangumi detail failed: {e}"),
        }
        .into(),
    }
}

pub fn handle_bangumi_search(request: &Request) -> Response {
    let name = match request.get_param("name") {
        Some(n) if !n.is_empty() => n,
        _ => return HandlerError::BadRequest("missing name".into()).into(),
    };

    let result: ApiResult<BangumiInfo> = match crate::services::bangumi::search(&name) {
        Ok(Some(id)) => {
            log::info!("Bangumi search '{name}' → #{id}");
            match crate::services::bangumi::detail(id) {
                Ok(Some(info)) => ApiResult::OK { value: info },
                Ok(None) => ApiResult::Err {
                    code: http_code::NOT_FOUND,
                    message: "no detail".into(),
                },
                Err(e) => {
                    log::error!("bangumi detail failed: {e}");
                    ApiResult::Err {
                        code: http_code::INTERNAL,
                        message: "upstream error".into(),
                    }
                }
            }
        }
        Ok(None) => ApiResult::Err {
            code: http_code::NOT_FOUND,
            message: "not found".into(),
        },
        Err(e) => {
            log::error!("bangumi search failed: {e}");
            ApiResult::Err {
                code: http_code::INTERNAL,
                message: "upstream error".into(),
            }
        }
    };
    result.into()
}

pub fn handle_health(dl: &dyn TorrentDownloader) -> Response {
    match dl.check_connection() {
        Ok(()) => ApiResult::OK { value: () }.into(),
        Err(e) => {
            log::error!("health check failed: {e}");
            ApiResult::<()>::Err {
                code: http_code::SERVICE_UNAVAILABLE,
                message: "downloader unavailable".into(),
            }
            .into()
        }
    }
}

pub fn handle_notify_test(tx: &Sender<Event>) -> Response {
    fire_event(tx, Event::NotifyTest, "test notifications sent")
}

pub fn handle_file_stream(
    infohash: &str,
    tx: &Sender<Event>,
    fs: &dyn FileOps,
    request: &Request,
) -> Response {
    let result: ApiResult<crate::types::EpisodeRecord> =
        match query_result(tx, |reply_tx| Event::ApiGetEpisode {
            infohash: infohash.to_string(),
            reply_tx,
        }) {
            Ok(r) => r,
            Err(e) => return e.into(),
        };

    let record = match result {
        ApiResult::OK { value } => value,
        ApiResult::Err { code, message } => {
            return json_response(code, &message);
        }
    };

    let file_path = match &record.library_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => return HandlerError::NotFound("file not yet available".into()).into(),
    };

    // Path traversal protection
    if !Path::new(&file_path).is_absolute() || file_path.contains("..") {
        log::error!("path traversal attempt: {file_path}");
        return HandlerError::NotFound("file not found".into()).into();
    }

    let stream = match fs.open_file(Path::new(&file_path)) {
        Ok(s) => s,
        Err(e) => {
            log::error!("failed to open {file_path}: {e}");
            let api_err = match e.downcast_ref::<std::io::Error>() {
                Some(ioe) if ioe.kind() == std::io::ErrorKind::NotFound => {
                    HandlerError::NotFound("file not found".into())
                }
                Some(ioe) if ioe.kind() == std::io::ErrorKind::PermissionDenied => {
                    HandlerError::NotFound("file not found".into())
                }
                _ => HandlerError::Internal {
                    client: "file unavailable".into(),
                    detail: format!("file unavailable: {e}"),
                },
            };
            return api_err.into();
        }
    };

    let content_type = mime_type(&file_path);
    let file_size = stream.size();

    // Parse Range header → serve requested range or full file.
    let range = match resolve_range(request, file_size) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    serve_file_range(stream, file_size, content_type, range)
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mime_type() {
        assert_eq!(mime_type("video.mp4"), "video/mp4");
        assert_eq!(mime_type("anime.mkv"), "video/x-matroska");
        assert_eq!(mime_type("song.mp3"), "audio/mpeg");
        assert_eq!(mime_type("unknown.xyz"), "application/octet-stream");
        assert_eq!(mime_type("no_ext"), "application/octet-stream");
    }

    #[test]
    fn handle_list_feeds_timeout() {
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
            fn move_files(&self, _: &str, _: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            fn pause(&self, _: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            fn remove(&self, _: &str, _: bool) -> anyhow::Result<bool> {
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
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_fire_event_closed_channel() {
        let (_tx, _rx) = crossbeam_channel::bounded::<Event>(0);
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
    fn test_handle_file_stream_unknown_infohash() {
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
        drop(rx);
        let req = Request::fake_http("GET", "/file", vec![], vec![]);
        let resp = handle_file_stream("unknown_hash", &tx, &EmptyFs, &req);
        assert_eq!(resp.status_code, 503);
    }
}
