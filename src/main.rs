mod core;
mod services;
mod traits;
mod types;
mod utils;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::bounded;

use crate::core::effect::Effect;
use crate::core::event::Event;
use crate::core::state::AppState;
use services::EffectExecutor;
use services::TimerManager;
use services::start_server;

const CHANNEL_CAPACITY: usize = 256;

fn main() -> anyhow::Result<()> {
    // ── channels ──
    let (event_tx, event_rx) = bounded::<Event>(CHANNEL_CAPACITY);
    let (effect_tx, effect_rx) = bounded::<Effect>(CHANNEL_CAPACITY);

    // ── shared state (owned by logic thread) ──
    let mut state = AppState::load().unwrap_or_default();

    // Fill dirs from env if not already set in state.
    if state.download_dir.is_empty() {
        state.download_dir = std::env::var("DOWNLOAD_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                eprintln!("[main] DOWNLOAD_DIR not set, using /downloads");
                "/downloads".into()
            });
    }
    if state.library_dir.is_empty() {
        state.library_dir = std::env::var("LIBRARY_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                eprintln!("[main] LIBRARY_DIR not set, using /anime");
                "/anime".into()
            });
    }

    // ── services (trait objects behind Arc) ──
    let use_mock = std::env::var("MOCK_DOWNLOADER").is_ok();
    let backend = std::env::var("DOWNLOADER").unwrap_or_else(|_| "aria2".into());

    let rss_client: Arc<dyn crate::traits::RssFetcher> = if use_mock {
        Arc::new(services::MockRssClient)
    } else {
        Arc::new(services::RssClient)
    };

    let downloader: Arc<dyn crate::traits::TorrentDownloader> = if use_mock {
        Arc::new(services::MockDownloader::new())
    } else if backend == "qbittorrent" {
        Arc::new(services::QbittorrentDownloader::from_env())
    } else {
        Arc::new(services::Aria2Downloader::from_env())
    };

    let fs_ops = Arc::new(services::RealFileSystem);
    let notifier = Arc::new(services::NoopNotifier);

    // ── event sources (publish to event_tx) ──

    // Combined timer: RSS ticks + download poll
    let mut tm = TimerManager::new();
    let timer_shutdown = tm.shutdown_handle();
    let rss_interval = Duration::from_secs(
        std::env::var("RSS_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(900),
    );
    {
        let tx = event_tx.clone();
        tm.add(rss_interval, move || {
            if tx.send(Event::RssTickAll).is_err() {
                eprintln!("[timer] logic channel disconnected, RSS tick dropped");
                return false; // stop this timer
            }
            true
        });
    }
    {
        let tx = event_tx.clone();
        tm.add(Duration::from_secs(30), move || {
            if tx.send(Event::PollDownloader).is_err() {
                eprintln!("[timer] logic channel disconnected, poll dropped");
                return false;
            }
            true
        });
    }
    thread::spawn(move || tm.run());

    // HTTP API server (logs on exit, but process continues)
    let tx = event_tx.clone();
    thread::spawn(move || {
        start_server(tx);
        eprintln!("[main] HTTP server thread exited");
    });

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

    // ── main thread: wait for logic to exit, then clean up ──
    logic_handle.join().ok();
    timer_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    println!("[main] shutdown complete");

    Ok(())
}
