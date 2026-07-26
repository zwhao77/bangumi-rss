//! Effect executor — the I/O boundary of the TEA architecture.
//!
//! Receives `Effect` values from the logic layer and executes them
//! by delegating to injected service trait objects.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use uuid::Uuid;

use crate::core::effect::Effect;
use crate::core::event::Event;
use crate::services::fetch_pool::{FetchJob, FetchPool};
use crate::traits::{FileOps, TorrentDownloader};
use crate::types::AnimeIdentity;
use crate::utils::notify::{WebhookConfig, render, render_failed};

/// Executes effects by delegating to service trait objects.
///
/// This is the **only** place where side effects happen.
/// Produces follow-up effects (fed back into `effect_tx`) and
/// feedback events (sent to `event_tx`, e.g. `DownloadStarted`).
pub struct EffectExecutor {
    pub downloader: Arc<dyn TorrentDownloader>,
    pub fs: Arc<dyn FileOps>,
    pub webhook: Option<WebhookConfig>,
    pub worker_pool: FetchPool,
    pub event_tx: Sender<Event>,
    /// For self-call patterns: spawned threads feed effects back to this executor.
    pub effect_tx: Sender<Effect>,
}

impl EffectExecutor {
    /// Block on `rx`, execute each effect, push follow-up effects to `tx`.
    pub fn run(&self, rx: Receiver<Effect>, tx: Sender<Effect>) {
        log::info!("started");
        for effect in rx {
            let follow_ups = self.execute(effect);
            for e in follow_ups {
                tx.send(e).ok();
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
            } => self.do_handle_completed(
                &infohash,
                feed_id,
                &anime,
                &library_dir,
                &download_dir,
                expected_episode,
            ),
            Effect::AddTorrentBytes {
                data,
                save_path,
                feed_id,
                torrent_url,
            } => self.do_add_torrent_bytes(&data, &save_path, feed_id, &torrent_url),
            Effect::Notify(notification) => self.do_notify(&notification),
            Effect::QueryAllDownloads => self.do_query_all(),
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
            Effect::PollCompleted => self.do_poll_completed(),
            Effect::PollFailed => self.do_poll_failed(),
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
            vec![]
        } else {
            match self.downloader.add_uri(uri, dir) {
                Ok(infohash) => {
                    log::info!("download started: infohash={infohash}, feed={feed_id}");
                    self.event_tx
                        .send(Event::DownloadStarted {
                            infohash,
                            feed_id,
                            torrent_url: uri.to_string(),
                        })
                        .ok();
                }
                Err(e) => {
                    log::warn!("add torrent failed: {e}");
                }
            }
            vec![]
        }
    }

    fn do_add_torrent_bytes(
        &self,
        data: &[u8],
        dir: &str,
        feed_id: Uuid,
        torrent_url: &str,
    ) -> Vec<Effect> {
        match self.downloader.add_torrent_bytes(data, dir) {
            Ok(infohash) if !infohash.is_empty() => {
                log::info!("download started: infohash={infohash}, feed={feed_id}");
                self.event_tx
                    .send(Event::DownloadStarted {
                        infohash,
                        feed_id,
                        torrent_url: torrent_url.to_string(),
                    })
                    .ok();
            }
            Ok(_) => {
                log::warn!("add_torrent_bytes returned empty infohash for feed={feed_id}");
            }
            Err(e) => {
                log::warn!("add_torrent_bytes failed: {e}");
            }
        }
        vec![]
    }

    fn do_handle_completed(
        &self,
        infohash: &str,
        feed_id: Uuid,
        anime: &AnimeIdentity,
        library_dir: &str,
        download_dir: &str,
        expected_episode: u32,
    ) -> Vec<Effect> {
        log::info!(
            "handle_completed: infohash={} feed={} anime={}",
            &infohash[..infohash.len().min(16)],
            feed_id,
            anime.name
        );
        let files = match self.downloader.list_files(infohash) {
            Ok(f) => {
                log::debug!("list_files: {} file(s)", f.len());
                for fi in &f {
                    log::debug!("  - {}", fi.name);
                }
                f
            }
            Err(e) => {
                log::warn!("list_files failed: {e}");
                return vec![];
            }
        };

        let record = crate::types::EpisodeRecord {
            infohash: infohash.to_string(),
            torrent_url: String::new(),
            feed_id,
            key: crate::types::EpisodeKey {
                anime: anime.clone(),
                episode: expected_episode,
            },
            status: crate::types::RecordStatus::Downloading,
            library_path: None,
        };
        let resolved =
            crate::utils::handler::resolve_files(&files, &record, download_dir, library_dir);

        let effects = Vec::new();

        for r in &resolved {
            log::debug!(
                "  episode={} -> rename to '{}'",
                r.key.episode,
                r.target_name
            );

            // Idempotent: if target already exists (crash recovery), skip move.
            if r.to.exists() {
                log::debug!("already in library: {:?}", r.to);
            } else {
                if let Some(parent) = r.to.parent()
                    && let Err(e) = self.fs.ensure_dir(parent)
                {
                    log::warn!("ensure_dir failed: {parent:?}: {e}");
                    continue;
                }
                match self.fs.move_file(&r.from, &r.to) {
                    Ok(()) => log::info!("moved: {:?} → {:?}", r.from, r.to),
                    Err(e) => {
                        log::warn!("move failed: {:?} → {:?}: {e}", r.from, r.to);
                        continue;
                    }
                }
            }

            // Try aria2 rename (best-effort).
            match self
                .downloader
                .rename_file(infohash, &r.original_name, &r.target_name)
            {
                Ok(true) => {
                    log::debug!("aria2 renamed: '{}' → '{}'", r.original_name, r.target_name)
                }
                Ok(false) => log::debug!("aria2 rename returned false for '{}'", r.original_name),
                Err(e) => log::warn!("aria2 rename failed: {e}"),
            }

            // Notify logic: episode + path resolved, move complete.
            self.event_tx
                .send(Event::EpisodeCompleted {
                    infohash: infohash.to_string(),
                    episode: r.key.episode,
                    library_path: r.to.to_string_lossy().to_string(),
                })
                .ok();
        }

        effects
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
                        .set("Content-Type", &content_type)
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

    fn do_query_all(&self) -> Vec<Effect> {
        match self.downloader.query_all() {
            Ok(snapshots) => {
                self.event_tx
                    .send(Event::DownloadsRefreshed { snapshots })
                    .ok();
            }
            Err(e) => {
                log::warn!("query_all failed: {e}");
            }
        }
        vec![]
    }

    fn do_poll_completed(&self) -> Vec<Effect> {
        match self.downloader.poll_completed() {
            Ok(tasks) if tasks.is_empty() => {}
            Ok(tasks) => {
                log::info!("poll_completed: {} task(s) complete", tasks.len());
                for task in tasks {
                    log::debug!("  -> {}", &task.infohash[..task.infohash.len().min(16)]);
                    let _ = self.event_tx.send(Event::DownloaderNotification {
                        infohash: task.infohash,
                        status: crate::core::event::DownloadStatus::Completed,
                    });
                }
            }
            Err(e) => {
                log::warn!("poll_completed failed: {e}");
            }
        }
        vec![]
    }

    fn do_poll_failed(&self) -> Vec<Effect> {
        match self.downloader.poll_failed() {
            Ok(tasks) if tasks.is_empty() => {}
            Ok(tasks) => {
                log::info!("poll_failed: {} task(s) failed", tasks.len());
                for task in tasks {
                    let _ = self.event_tx.send(Event::DownloaderNotification {
                        infohash: task.infohash,
                        status: crate::core::event::DownloadStatus::Failed,
                    });
                }
            }
            Err(e) => {
                log::warn!("poll_failed failed: {e}");
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
    use crate::services::mock::{MockDownloader, MockFileSystem};
    use crate::types::{AnimeIdentity, EpisodeKey, EpisodeRecord, RecordStatus};

    #[test]
    fn handle_completed_flow_with_mocks() {
        let (event_tx, event_rx) = crossbeam_channel::bounded(16);
        let (effect_tx, effect_rx) = crossbeam_channel::bounded(16);

        // ── setup services ──
        let downloader = Arc::new(MockDownloader::new());
        let fs = Arc::new(MockFileSystem::new());
        // Register a file that "exists" on disk so move succeeds.
        let infohash = downloader.add_uri("test.torrent", "/dl/test-feed").unwrap();

        // The mock downloader's list_files returns "[MockSubs] test.torrent - 01 [1080p].mkv"
        // The source path is: /dl/test-feed/[MockSubs] test.torrent - 01 [1080p].mkv
        let src = std::path::PathBuf::from(
            "/dl/test-feed/[MockSubs] test.torrent - 01 [1080p].mkv".to_string(),
        );
        fs.existing.lock().unwrap().insert(src);

        let executor = EffectExecutor {
            downloader: downloader.clone(),
            fs: fs.clone(),
            webhook: None,
            worker_pool: FetchPool::new(4, 512),
            event_tx: event_tx.clone(),
            effect_tx: effect_tx.clone(),
        };

        // ── populate tracker in AppState ──
        let feed_id = uuid::Uuid::new_v4();
        let anime = AnimeIdentity {
            name: "Test Anime".into(),
            season: 2,
        };
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
                    Event::EpisodeCompleted {
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
            "should receive EpisodeCompleted after handle"
        );
        assert_eq!(fs.move_count(), 1, "should have called move_file once");
    }
}
