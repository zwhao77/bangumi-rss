mod config;
mod core;
mod services;
mod traits;
mod types;
mod utils;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::bounded;

use envconfig::Envconfig;

use crate::config::{Config, Downloader};
use crate::core::effect::Effect;
use crate::core::event::Event;
use crate::core::state::AppState;
use services::EffectExecutor;
use services::TimerManager;
use services::start_server;

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

    // ── shared state (owned by logic thread) ──
    let mut state = AppState::load(config.data_dir.as_deref().unwrap_or(".")).unwrap_or_default();

    // Fill dirs from env if not already set in state.
    if state.download_dir.is_empty() {
        state.download_dir = config.download_dir.unwrap_or_else(|| "/downloads".into());
    }
    if state.library_dir.is_empty() {
        state.library_dir = config.library_dir.unwrap_or_else(|| "/anime".into());
    }

    // ── services (trait objects behind Arc) ──
    let rss_client: Arc<dyn crate::traits::RssFetcher> = if config.mock_downloader {
        Arc::new(services::MockRssClient)
    } else {
        Arc::new(services::RssClient)
    };

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
        ))
    };

    let fs_ops = Arc::new(services::RealFileSystem);
    let notifier = Arc::new(services::NoopNotifier);

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
        let port = config.port;
        thread::spawn(move || {
            start_server(tx, port);
            log::warn!("HTTP server thread exited");
        });
    } else {
        log::info!("HTTP server disabled (NO_SERVER set)");
    }

    // ── effect executor (consumes effects, may publish DownloadStarted events) ──
    let executor = EffectExecutor {
        rss: rss_client,
        downloader,
        fs: fs_ops,
        notifier,
        event_tx: event_tx.clone(),
        effect_tx: effect_tx.clone(),
    };
    let effect_tx_inner = effect_tx.clone();
    thread::spawn(move || executor.run(effect_rx, effect_tx_inner));

    // ── logic thread (owns AppState, runs pure reducer) ──
    let logic_handle = thread::spawn(move || {
        crate::core::event::run_logic(event_rx, effect_tx, state);
    });

    logic_handle.join().ok();
    timer_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    log::info!("shutdown complete");

    Ok(())
}
