//! Application bootstrap — service wiring + thread startup.
//!
//! The only entry point is [`run`], which takes a resolved [`Config`] and
//! starts the whole application.  It is pure with respect to environment:
//! callers build the `Config` (e.g. from env vars in `main.rs`), and services
//! are selected via `config.mock_downloader` — making this testable with mocks.
//!
//! The heavy lifting is split into small private helpers so each piece can be
//! unit-tested independently:
//! - [`build_fs`] / [`build_downloader`] — pure DI selection
//! - [`load_initial_state`] — state load + dir validation + config fill-in
//! - [`setup_timers`] — periodic event sources

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Context;

use crossbeam_channel::{Sender, bounded};

use crate::config::{Config, Downloader};
use crate::core::effect::Effect;
use crate::core::event::{Event, run_logic};
use crate::core::state::AppState;
use crate::services;
use crate::services::fetch_pool::FetchPool;
use crate::services::persistence::load_state;
use crate::services::{EffectExecutor, TimerManager, start_server};
use crate::traits::{FileOps, TorrentDownloader};

const CHANNEL_CAPACITY: usize = 256;

/// Bootstrap the full application from a resolved `Config`.
///
/// Wires up services (mock or real per `config.mock_downloader`) and starts
/// all four threads: timers, HTTP server, effect executor, logic.  Blocks
/// until the logic thread exits (i.e. on shutdown).
pub fn run(config: Config) -> anyhow::Result<()> {
    // Resolve webhook config before any partial moves.
    let webhook = config.resolve_webhook()?;

    // Propagate config to stateless modules.
    services::bangumi::init_api_base(config.bangumi_api_base.clone());

    // ── channels ──
    let (event_tx, event_rx) = bounded::<Event>(CHANNEL_CAPACITY);
    let (effect_tx, effect_rx) = bounded::<Effect>(CHANNEL_CAPACITY);

    // ── services + state ──
    let fs_ops = build_fs(&config);
    let data_dir = config.data_dir.clone();
    let state = load_initial_state(&*fs_ops, &config)?;
    let downloader = build_downloader(&config);
    let fs_ops_for_executor = Arc::clone(&fs_ops);

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
    setup_timers(&mut tm, &event_tx, &config);
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
                services::ServerConfig {
                    bind_addr: config.bind_addr,
                    port: config.port,
                    max_connections: config.max_connections,
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
        webhook,
        worker_pool: FetchPool::new(config.torrent_concurrency, config.queue_capacity),
        event_tx: event_tx.clone(),
        effect_tx: effect_tx.clone(),
    };
    let effect_tx_inner = effect_tx.clone();
    thread::spawn(move || executor.run(effect_rx, effect_tx_inner));

    // ── logic thread (owns AppState, runs pure reducer) ──
    let logic_handle = thread::spawn(move || {
        run_logic(event_rx, effect_tx, state, fs_ops, data_dir);
    });

    logic_handle.join().ok();
    timer_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    log::info!("shutdown complete");

    Ok(())
}

// ── Service factories (pure DI selection) ──

/// Pick the filesystem implementation (mock vs real) from config.
fn build_fs(config: &Config) -> Arc<dyn FileOps> {
    if config.mock_downloader {
        Arc::new(services::MockFileSystem::new())
    } else {
        Arc::new(services::RealFileSystem)
    }
}

/// Pick the torrent downloader implementation (mock vs real) from config.
fn build_downloader(config: &Config) -> Arc<dyn TorrentDownloader> {
    if config.mock_downloader {
        Arc::new(services::MockDownloader::new())
    } else {
        match config.downloader {
            Downloader::Qbittorrent => Arc::new(services::QbittorrentDownloader::from_config(
                config.qbittorrent_url.clone(),
                config.qbittorrent_user.clone(),
                config.qbittorrent_pass.clone(),
            )),
            Downloader::Transmission => Arc::new(services::TransmissionDownloader::with_rpc_url(
                config.transmission_rpc_url.clone(),
                config.transmission_user.clone(),
                config.transmission_pass.clone(),
            )),
            Downloader::Aria2 => Arc::new(services::Aria2Downloader::with_rpc_url(
                config.aria2_rpc_url.clone(),
                config.aria2_rpc_token.clone(),
            )),
        }
    }
}

// ── State init ──

/// Load persisted state, validate directories (fail fast on permission
/// errors), and fill download/library dirs from config (never persisted).
fn load_initial_state(fs: &dyn FileOps, config: &Config) -> anyhow::Result<AppState> {
    let mut state = load_state(fs, &config.data_dir).unwrap_or_default();

    fs.ensure_dir(Path::new(&config.download_dir))
        .context("cannot access DOWNLOAD_DIR")?;
    fs.ensure_dir(Path::new(&config.library_dir))
        .context("cannot access LIBRARY_DIR")?;

    state.download_dir = config.download_dir.clone();
    state.library_dir = config.library_dir.clone();

    Ok(state)
}

// ── Timer setup ──

/// Register the periodic RSS poll and downloader poll timers.
fn setup_timers(tm: &mut TimerManager, event_tx: &Sender<Event>, config: &Config) {
    let rss_interval = Duration::from_secs(config.rss_interval);
    let tx = event_tx.clone();
    tm.add(rss_interval, move || {
        if tx.send(Event::RssTickAll).is_err() {
            log::error!("logic channel disconnected, RSS tick dropped");
            return false;
        }
        true
    });

    let poll_interval = Duration::from_secs(config.poll_interval);
    let tx = event_tx.clone();
    tm.add(poll_interval, move || {
        if tx.send(Event::PollDownloader).is_err() {
            log::error!("logic channel disconnected, poll dropped");
            return false;
        }
        true
    });
}
