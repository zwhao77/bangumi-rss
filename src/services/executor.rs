//! Effect executor — the I/O boundary of the TEA architecture.
//!
//! Receives `Effect` values from the logic layer and executes them
//! by delegating to injected service trait objects.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use uuid::Uuid;

use crate::core::effect::Effect;
use crate::core::event::Event;
use crate::services::dl_command::{DlSendError, DlThread};
use crate::services::fetch_pool::{FetchJob, FetchPool};
use crate::traits::FileOps;
use crate::utils::handler::ResolvedFile;
use crate::utils::notify::{WebhookConfig, render, render_failed};

/// Executes effects by delegating to service trait objects.
///
/// This is the **only** place where side effects happen.
/// Produces follow-up effects (fed back into `effect_tx`) and
/// feedback events (sent to `event_tx`, e.g. `DownloadStarted`).
pub struct EffectExecutor {
    pub(crate) dl_thread: DlThread,
    pub fs: Arc<dyn FileOps>,
    pub webhook: Option<WebhookConfig>,
    pub worker_pool: FetchPool,
    pub event_tx: Sender<Event>,
    /// For self-call patterns: spawned threads feed effects back to this executor.
    pub(crate) effect_tx: Sender<Effect>,
}

impl EffectExecutor {
    /// Block on `rx`, execute each effect, push follow-up effects to `tx`.
    pub(crate) fn run(&self, rx: Receiver<Effect>, tx: Sender<Effect>) {
        log::info!("started");
        for effect in rx {
            let follow_ups = self.execute(effect);
            for e in follow_ups {
                tx.send(e).ok();
            }
        }
    }

    /// Handle a downloader command submission result uniformly.
    ///
    /// - `Full` → queue backpressured (thread busy); command dropped — the next
    ///   poll / RSS tick retries naturally.
    /// - `Disconnected` → downloader thread died; logged. (Restart / backoff is
    ///   future work.)
    fn dispatch_dl(&self, r: Result<(), DlSendError>) {
        match r {
            Ok(()) => {}
            Err(DlSendError::Full) => {
                log::warn!("[executor] downloader queue full, command dropped");
            }
            Err(DlSendError::Disconnected) => {
                log::error!("[executor] downloader thread disconnected");
            }
        }
    }

    fn execute(&self, effect: Effect) -> Vec<Effect> {
        match effect {
            Effect::FetchRss {
                url,
                feed_id,
                download_dir,
            } => self.do_fetch_rss(&url, feed_id, &download_dir),
            Effect::AddTorrent {
                torrent_url,
                save_path,
                feed_id,
            } => self.do_add_torrent(&torrent_url, &save_path, feed_id),
            Effect::HandleCompleted {
                infohash,
                feed_id,
                anime,
                library_dir,
                download_dir,
                expected_episode,
            } => {
                self.dispatch_dl(self.dl_thread.send_handle_completed(
                    infohash,
                    feed_id,
                    anime,
                    library_dir,
                    download_dir,
                    expected_episode,
                ));
                vec![]
            }
            Effect::AddTorrentBytes {
                data,
                save_path,
                feed_id,
                torrent_url,
            } => {
                self.dispatch_dl(self.dl_thread.send_add_bytes(
                    torrent_url,
                    data,
                    save_path,
                    feed_id,
                ));
                vec![]
            }
            Effect::Notify(notification) => self.do_notify(&notification),
            Effect::QueryAllDownloads => {
                self.dispatch_dl(self.dl_thread.send_query_all());
                vec![]
            }
            Effect::CheckDownloader { reply_tx } => {
                self.dispatch_dl(self.dl_thread.send_check_connection(reply_tx));
                vec![]
            }
            Effect::RssFetchComplete {
                feed_id,
                items,
                download_dir,
            } => self.do_rss_fetch_complete(feed_id, items, &download_dir),
            Effect::RssFetchFailed { feed_id, error } => {
                self.event_tx
                    .send(Event::RssFetchFailed { feed_id, error })
                    .ok();
                vec![]
            }
            Effect::PollCompleted => {
                self.dispatch_dl(self.dl_thread.send_poll_completed());
                vec![]
            }
            Effect::PollFailed => {
                self.dispatch_dl(self.dl_thread.send_poll_failed());
                vec![]
            }
            Effect::FilesystemFallback {
                infohash,
                resolved,
                season_dir,
            } => self.do_filesystem_fallback(&infohash, &resolved, &season_dir),
        }
    }

    // ── Per-effect handlers ──

    fn do_fetch_rss(&self, url: &str, feed_id: Uuid, download_dir: &str) -> Vec<Effect> {
        if download_dir.is_empty() {
            log::warn!("skip fetch: download_dir is empty for feed={feed_id}");
            return vec![];
        }
        self.worker_pool
            .try_spawn(FetchJob::FetchRss {
                url: url.to_string(),
                feed_id,
                download_dir: download_dir.to_string(),
                effect_tx: self.effect_tx.clone(),
            })
            .unwrap_or_else(|_| {
                log::warn!("RSS queue full, skip feed={feed_id}");
            });
        vec![]
    }

    fn do_rss_fetch_complete(
        &self,
        feed_id: Uuid,
        items: Vec<crate::types::RssItem>,
        download_dir: &str,
    ) -> Vec<Effect> {
        self.event_tx
            .send(Event::RssItemsFetched {
                feed_id,
                items,
                download_dir: download_dir.to_string(),
            })
            .ok();
        vec![]
    }

    fn do_add_torrent(&self, uri: &str, dir: &str, feed_id: Uuid) -> Vec<Effect> {
        let is_torrent = uri.ends_with(".torrent") || uri.contains(".torrent?");

        if is_torrent {
            if self
                .worker_pool
                .try_spawn(FetchJob::DownloadTorrent {
                    uri: uri.to_string(),
                    save_path: dir.to_string(),
                    feed_id,
                    effect_tx: self.effect_tx.clone(),
                })
                .is_err()
            {
                log::warn!("torrent queue full, will retry on next RSS poll: {uri}");
            }
        } else {
            // Magnet / direct URL → downloader scheduling thread.
            self.dispatch_dl(self.dl_thread.send_add_uri(
                uri.to_string(),
                dir.to_string(),
                feed_id,
            ));
        }
        vec![]
    }

    fn do_filesystem_fallback(
        &self,
        infohash: &str,
        resolved: &[ResolvedFile],
        season_dir: &str,
    ) -> Vec<Effect> {
        log::info!(
            "filesystem fallback for {}",
            &infohash[..infohash.len().min(16)]
        );
        for r in resolved {
            if self.fs.exists(&r.to) {
                log::debug!("already in library: {:?}", r.to);
                continue;
            }

            // Probe for file: crash may have happened at any point.
            let candidates: [&std::path::Path; 3] = [
                &r.actual,
                &std::path::PathBuf::from(format!("{season_dir}/{}", r.target_name)),
                &std::path::PathBuf::from(format!("{season_dir}/{}", r.original_name)),
            ];
            let found = candidates.iter().find(|p| self.fs.exists(p));

            let src = match found {
                Some(p) => {
                    log::debug!("fallback: found file at {:?}", p);
                    (*p).to_path_buf()
                }
                None => {
                    log::warn!("fallback: file not found: {}", r.original_name);
                    continue;
                }
            };

            if let Some(parent) = r.to.parent()
                && let Err(e) = self.fs.ensure_dir(parent)
            {
                log::warn!("fallback: ensure_dir({parent:?}) failed: {e}");
                continue;
            }
            if let Err(e) = self.fs.move_file(&src, &r.to) {
                log::warn!("fallback: move({src:?} → {:?}) failed: {e}", r.to);
                continue;
            }
            log::info!("fs move: {:?} → {:?}", src, r.to);
            self.event_tx
                .send(Event::EpisodeMovedToLibrary {
                    infohash: infohash.to_string(),
                    episode: r.key.episode,
                    library_path: r.to.to_string_lossy().to_string(),
                })
                .ok();
        }
        vec![]
    }

    fn do_notify(&self, notification: &crate::types::Notification) -> Vec<Effect> {
        match &self.webhook {
            Some(cfg) => {
                let (body, content_type) = match notification {
                    crate::types::Notification::EpisodeDownloaded(_) => {
                        render(&cfg.template, notification)
                    }
                    crate::types::Notification::Failed(f) => match &cfg.error_template {
                        Some(t) => render(t, notification),
                        None => render_failed(&cfg.template, f),
                    },
                };
                let url = cfg.url.clone();
                std::thread::spawn(move || {
                    let timeout = std::time::Duration::from_secs(crate::config::HTTP_TIMEOUT_SECS);
                    match ureq::post(&url)
                        .set("Content-Type", content_type)
                        .timeout(timeout)
                        .send_string(&body)
                    {
                        Ok(r) if (200..300).contains(&r.status()) => {
                            log::info!("webhook sent ({})", r.status());
                        }
                        Ok(r) => log::warn!("webhook returned {}", r.status()),
                        Err(e) => log::warn!("webhook failed: {e}"),
                    }
                });
            }
            None => {
                // Log to stdout when no webhook is configured
                match notification {
                    crate::types::Notification::EpisodeDownloaded(d) => {
                        log::info!("[notify] {} 第{}集 下载完成", d.anime_name, d.episode);
                    }
                    crate::types::Notification::Failed(f) => {
                        log::warn!("[notify] 失败: {} - {}", f.title, f.message);
                    }
                }
            }
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::core::effect::Effect;
    use crate::core::event::Event;
    use crate::core::state::AppState;
    use crate::services::downloader::mock::{MockDownloader, MockFileSystem};
    use crate::traits::TorrentDownloader;
    use crate::types::{AnimeIdentity, EpisodeKey, EpisodeRecord, RecordStatus};

    #[test]
    fn handle_completed_flow_with_mocks() {
        let (event_tx, event_rx) = crossbeam_channel::bounded(16);
        let (effect_tx, effect_rx) = crossbeam_channel::bounded(16);

        // ── setup services ──
        let downloader = Arc::new(MockDownloader::new());
        let fs = Arc::new(MockFileSystem::new());
        let infohash = downloader.add_uri("test.torrent", "/dl/test-feed").unwrap();

        let feed_id = uuid::Uuid::new_v4();
        let anime = AnimeIdentity {
            name: "Test Anime".into(),
            season: 2,
        };

        // Register the source file in mock filesystem at the path resolve_files will construct.
        // resolve_files builds: {download_dir}/{feed_id}/{file.path}
        let src = std::path::PathBuf::from(format!(
            "/dl/{}/[MockSubs] test.torrent - 01 [1080p].mp4",
            feed_id
        ));
        fs.existing.lock().unwrap().insert(src);

        let executor = EffectExecutor {
            dl_thread: DlThread::spawn(downloader.clone(), event_tx.clone(), effect_tx.clone()),
            fs: fs.clone(),
            webhook: None,
            worker_pool: FetchPool::new(4, 512),
            event_tx: event_tx.clone(),
            effect_tx: effect_tx.clone(),
        };

        // ── populate tracker in AppState ──
        let mut state = AppState {
            download_dir: "/dl".into(),
            library_dir: "/lib".into(),
            ..AppState::default()
        };
        state.feeds.insert(
            feed_id,
            crate::core::state::Feed {
                id: feed_id,
                url: "https://example.com/rss".into(),
                anime: anime.clone(),
                confirmed: true,
                bangumi_info: None,
                filter: Default::default(),
            },
        );
        state.tracker.insert(
            infohash.clone(),
            EpisodeRecord {
                infohash: infohash.clone(),
                torrent_url: String::new(),
                feed_id,
                key: EpisodeKey {
                    anime: anime.clone(),
                    episode: 0,
                },
                status: RecordStatus::Downloading,
                library_path: None,
            },
        );

        // ── spawn executor ──
        let fx = effect_tx.clone();
        std::thread::spawn(move || executor.run(effect_rx, fx));

        // ── simulate poll → HandleCompleted ──
        effect_tx.send(Effect::PollCompleted).unwrap();

        // Consume events: DownloaderNotification
        let mut got_notification = false;
        let mut got_completed = false;
        for _ in 0..10 {
            if let Ok(ev) = event_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                match ev {
                    Event::DownloaderNotification { .. } => {
                        got_notification = true;
                        // Now feed HandleCompleted manually (simulating logic).
                        effect_tx
                            .send(Effect::HandleCompleted {
                                infohash: infohash.clone(),
                                feed_id,
                                anime: anime.clone(),
                                library_dir: "/lib".into(),
                                download_dir: "/dl".into(),
                                expected_episode: 0,
                            })
                            .unwrap();
                    }
                    Event::EpisodeMovedToLibrary {
                        episode,
                        library_path,
                        ..
                    } => {
                        assert_eq!(episode, 1);
                        assert!(library_path.contains("/lib/Test Anime/S02/"));
                        got_completed = true;
                        break;
                    }
                    _ => {}
                }
            }
        }

        assert!(got_notification, "should receive DownloaderNotification");
        assert!(
            got_completed,
            "should receive EpisodeMovedToLibrary after handle"
        );
        // File moved to library via filesystem rename (breaks seeding).
        assert_eq!(fs.move_count(), 1, "should have called move_file once");
    }

    /// Stress test: a slow downloader (200ms poll) must NOT block the executor
    /// from processing unrelated effects.  Proves the DlThread isolation —
    /// if the executor called the downloader directly, the fast effect below
    /// would be delayed past the 100ms assertion window.
    #[test]
    fn slow_downloader_does_not_block_other_effects() {
        use std::time::{Duration, Instant};

        let (event_tx, event_rx) = crossbeam_channel::bounded(16);
        let (effect_tx, effect_rx) = crossbeam_channel::bounded(16);

        // Artificial 200ms downloader latency.
        let dl = Arc::new(MockDownloader {
            poll_delay_ms: 200,
            ..MockDownloader::new()
        });
        let fs = Arc::new(MockFileSystem::new());

        let executor = EffectExecutor {
            dl_thread: DlThread::spawn(dl.clone(), event_tx.clone(), effect_tx.clone()),
            fs: fs.clone(),
            webhook: None,
            worker_pool: FetchPool::new(2, 32),
            event_tx: event_tx.clone(),
            effect_tx: effect_tx.clone(),
        };
        let fx = effect_tx.clone();
        std::thread::spawn(move || executor.run(effect_rx, fx));

        // 1) Slow downloader command — blocks the dl-thread for 200ms.
        effect_tx.send(Effect::PollCompleted).unwrap();

        // 2) Fast non-downloader effect — must NOT wait for the slow poll.
        let start = Instant::now();
        effect_tx
            .send(Effect::RssFetchComplete {
                feed_id: uuid::Uuid::new_v4(),
                items: vec![],
                download_dir: "/dl".into(),
            })
            .unwrap();

        let ev = event_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("RssItemsFetched should arrive well before the 200ms poll finishes");
        assert!(
            matches!(ev, Event::RssItemsFetched { .. }),
            "expected RssItemsFetched, got {ev:?}"
        );
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "executor must not wait for the slow downloader"
        );
    }

    /// Stress test: a burst of 260 downloader commands (over the 256 queue cap)
    /// must NOT block the executor.  The queue overflows (commands dropped via
    /// `dispatch_dl`), while unrelated effects still process immediately.
    #[test]
    fn burst_over_capacity_does_not_block_executor() {
        use std::time::{Duration, Instant};

        let (event_tx, event_rx) = crossbeam_channel::bounded(16);
        let (effect_tx, effect_rx) = crossbeam_channel::bounded(16);

        // Slow downloader (5ms/poll) so the 256-capacity queue overflows.
        let dl = Arc::new(MockDownloader {
            poll_delay_ms: 5,
            poll_count: std::sync::atomic::AtomicU64::new(0),
            ..MockDownloader::new()
        });
        let fs = Arc::new(MockFileSystem::new());

        let executor = EffectExecutor {
            dl_thread: DlThread::spawn(dl.clone(), event_tx.clone(), effect_tx.clone()),
            fs: fs.clone(),
            webhook: None,
            worker_pool: FetchPool::new(2, 32),
            event_tx: event_tx.clone(),
            effect_tx: effect_tx.clone(),
        };
        let fx = effect_tx.clone();
        std::thread::spawn(move || executor.run(effect_rx, fx));

        // Burst of downloader commands beyond queue capacity.
        for _ in 0..260 {
            effect_tx.send(Effect::PollCompleted).unwrap();
        }

        // A fast unrelated effect afterwards — must still process promptly.
        let start = Instant::now();
        effect_tx
            .send(Effect::RssFetchComplete {
                feed_id: uuid::Uuid::new_v4(),
                items: vec![],
                download_dir: "/dl".into(),
            })
            .unwrap();

        let ev = event_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("RssItemsFetched should arrive fast despite 260 queued commands");
        assert!(
            matches!(ev, Event::RssItemsFetched { .. }),
            "expected RssItemsFetched, got {ev:?}"
        );
        // If the executor blocked on the downloader, 260×5ms serial would
        // exceed this window by far.
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "executor must not be blocked by the command burst"
        );

        // Give the dl-thread time to drain its queue (≤256 × 5ms ≈ 1.3s).
        std::thread::sleep(Duration::from_millis(1600));

        // The 260 commands exceeded the 256-capacity queue, so at least some
        // MUST have been dropped — proven by the poll counter staying below 260.
        let processed = dl.poll_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            processed < 260,
            "expected some commands to be dropped, but all {processed} were processed"
        );
        // And it should have processed (nearly) the whole queue capacity.
        assert!(
            processed >= 250,
            "suspiciously few commands processed: {processed}"
        );
    }
}
