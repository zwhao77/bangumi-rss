mod config;
mod core;
mod services;
mod traits;
mod types;
mod utils;

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Context;

use crossbeam_channel::bounded;

use envconfig::Envconfig;

use crate::config::{Config, Downloader};
use crate::core::effect::Effect;
use crate::core::event::Event;
use services::EffectExecutor;
use services::TimerManager;
use services::persistence::load_state;
use services::start_server;
use services::fetch_pool::FetchPool;

const CHANNEL_CAPACITY: usize = 256;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
        .format_timestamp_millis()
        .init();

    let config = Config::init_from_env()?;

    // Propagate config to stateless modules.
    crate::services::bangumi::init_api_base(config.bangumi_api_base);

    // ── channels ──
    let (event_tx, event_rx) = bounded::<Event>(CHANNEL_CAPACITY);
    let (effect_tx, effect_rx) = bounded::<Effect>(CHANNEL_CAPACITY);

    // ── file system (needed before state loading) ──
    let fs_ops: Arc<dyn crate::traits::FileOps> = if config.mock_downloader {
        Arc::new(services::MockFileSystem::new())
    } else {
        Arc::new(services::RealFileSystem)
    };
    let data_dir = config.data_dir.clone();

    // ── shared state (owned by logic thread) ──
    let mut state = load_state(&*fs_ops, &data_dir).unwrap_or_default();

    // Validate directories early — fail fast if permission denied.
    fs_ops
        .ensure_dir(Path::new(&config.download_dir))
        .context("cannot access DOWNLOAD_DIR")?;
    fs_ops
        .ensure_dir(Path::new(&config.library_dir))
        .context("cannot access LIBRARY_DIR")?;

    // Fill dirs from config (always, not persisted).
    state.download_dir = config.download_dir;
    state.library_dir = config.library_dir;

    // ── services (trait objects behind Arc) ──
    let downloader: Arc<dyn crate::traits::TorrentDownloader> = if config.mock_downloader {
        Arc::new(services::MockDownloader::new())
    } else if matches!(config.downloader, Downloader::Qbittorrent) {
        Arc::new(services::QbittorrentDownloader::from_config(
            config.qbittorrent_url,
            config.qbittorrent_user,
            config.qbittorrent_pass,
        ))
    } else {
        Arc::new(services::Aria2Downloader::with_rpc_url(
            config.aria2_rpc_url,
            config.aria2_rpc_token,
        ))
    };

    let fs_ops_for_executor = Arc::clone(&fs_ops);
    let notifier = Arc::new(services::NoopNotifier);

    // ── Verify downloader connectivity ──
    if let Err(e) = downloader.check_connection() {
        log::warn!("downloader check failed: {e}");
        log::warn!(
            "the service will start, but downloads will not work until the downloader is available"
        );
    } else {
        log::info!("downloader connection OK");
    }

    // ── event sources (publish to event_tx) ──

    let mut tm = TimerManager::new();
    let timer_shutdown = tm.shutdown_handle();
    let rss_interval = Duration::from_secs(config.rss_interval);
    {
        let tx = event_tx.clone();
        tm.add(rss_interval, move || {
            if tx.send(Event::RssTickAll).is_err() {
                log::error!("logic channel disconnected, RSS tick dropped");
                return false;
            }
            true
        });
    }
    {
        let tx = event_tx.clone();
        tm.add(Duration::from_secs(30), move || {
            if tx.send(Event::PollDownloader).is_err() {
                log::error!("logic channel disconnected, poll dropped");
                return false;
            }
            true
        });
    }
    thread::spawn(move || tm.run());

    // HTTP API server — skippable via NO_SERVER.
    if !config.no_server {
        let tx = event_tx.clone();
        let fs = Arc::clone(&fs_ops);
        let dl = downloader.clone();
        thread::spawn(move || {
            start_server(
                tx,
                dl,
                fs,
                services::server::ServerConfig {
                    bind_addr: config.bind_addr,
                    port: config.port,
                    max_connections: config.max_connections,
                    max_queue: config.max_queue,
                    auth_username: config.auth_username,
                    auth_password: config.auth_password,
                },
            );
            log::warn!("HTTP server thread exited");
        });
    } else {
        log::info!("HTTP server disabled (NO_SERVER set)");
    }

    // ── effect executor (consumes effects, may publish DownloadStarted events) ──
    let executor = EffectExecutor {
        downloader,
        fs: fs_ops_for_executor,
        notifier,
        worker_pool: FetchPool::new(config.torrent_concurrency, config.queue_capacity),
        event_tx: event_tx.clone(),
        effect_tx: effect_tx.clone(),
    };
    let effect_tx_inner = effect_tx.clone();
    thread::spawn(move || executor.run(effect_rx, effect_tx_inner));

    // ── logic thread (owns AppState, runs pure reducer) ──
    let logic_handle = thread::spawn(move || {
        crate::core::event::run_logic(event_rx, effect_tx, state, fs_ops, data_dir);
    });

    logic_handle.join().ok();
    timer_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    log::info!("shutdown complete");

    Ok(())
}
