// ── Server entry point ──
use crossbeam_channel::Sender;
use rouille::router;
use rouille::{Request, Response, ResponseBody, Server};
use std::sync::Arc;

use crate::core::event::Event;
use crate::services::server::ServerConfig;
use crate::services::server::handle::*;
use crate::services::server::utils::check_auth;
use crate::traits::FileOps;

pub fn start_server(
    event_tx: Sender<Event>,
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

        let response = handle_request(request, &event_tx, &*fs);

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

fn handle_request(
    request: &Request,
    tx: &Sender<Event>,
    fs: &dyn FileOps,
) -> Response {
    let method = request.method().to_uppercase();
    let url = request.url().to_string();
    log::debug!("-> {method} {url}");

    // Handle paths with dots before the router macro (rouille router treats `.` as special).
    if url == "/style.css" {
        return handle_style_css(fs);
    }

    router!(request,
        (GET) (/) => { handle_index(fs) },

        // ═══ /api/feeds ═══
        (GET) (/api/feeds) => { handle_feeds_list(tx) },
        (POST) (/api/feeds) => { handle_feed_create(request, tx) },
        (POST) (/api/feeds/refresh) => { handle_feeds_refresh(tx) },
        (PUT) (/api/feeds/{id: String}) => { handle_feed_update(&id, request, tx) },
        (DELETE) (/api/feeds/{id: String}) => { handle_feed_delete(&id, tx) },
        (POST) (/api/feeds/preview) => { handle_preview(request) },

        // ═══ /api/files ═══
        (GET) (/api/files/{infohash: String}) => { handle_file_stream(&infohash, tx, fs, request) },

        // ═══ /api/downloads ═══
        (GET) (/api/downloads) => { handle_downloads_list(tx) },
        (POST) (/api/downloads/refresh) => { handle_downloads_refresh(tx) },
        (POST) (/api/downloads/poll) => { handle_downloads_poll(tx) },

        // ═══ /api/bangumi ═══
        (GET) (/api/bangumi/subjects/{id: String}) => { handle_bangumi_subject(&id) },
        (GET) (/api/bangumi/search) => { handle_bangumi_search(request) },

        // ═══ /api/health ═══
        (GET) (/api/health) => { handle_health(tx) },

        // ═══ /api/notify/test ═══
        (POST) (/api/notify/test) => { handle_notify_test(tx) },

        _ => Response::empty_404(),
    )
}
