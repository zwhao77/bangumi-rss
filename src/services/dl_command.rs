//! Downloader scheduling thread.
//!
//! The [`DlThread`] owns the downloader and runs all downloader RPCs on a
//! dedicated thread — isolating potentially slow calls (especially timeouts)
//! from the main executor thread and the bounded channels it services.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use uuid::Uuid;

use crate::core::effect::Effect;
use crate::core::event::{DownloadStatus, Event};
use crate::traits::{OpResult, TorrentDownloader};
use crate::types::{AnimeIdentity, ApiResult, EpisodeKey, EpisodeRecord, RecordStatus, http_code};
use crate::utils::handler::resolve_files;

// ── Private command enum ──

/// An atomic downloader operation (only `DlThread` constructs these).
pub(crate) enum DlCommand {
    AddUri {
        uri: String,
        dir: String,
        feed_id: Uuid,
    },
    AddBytes {
        data: Vec<u8>,
        dir: String,
        feed_id: Uuid,
    },
    PollCompleted,
    PollFailed,
    QueryAll,
    CheckConnection {
        reply_tx: Sender<ApiResult<()>>,
    },
    /// Atomic post-download: list_files → resolve → pause → move → rename.
    /// On success sends `EpisodeMovedToLibrary` events; on failure sends
    /// `Effect::FilesystemFallback` so executor can do filesystem recovery.
    HandleCompleted {
        infohash: String,
        feed_id: Uuid,
        anime: AnimeIdentity,
        library_dir: String,
        download_dir: String,
        expected_episode: u32,
    },
}

// ── Public thread handle ──

/// A dedicated single-worker thread for downloader RPCs.
///
/// Owns the only caller of [`TorrentDownloader`]; the executor communicates
/// via the public `send_*` methods (fire-and-forget, or synchronous reply
/// for `check_connection`).
pub(crate) struct DlThread {
    tx: Sender<DlCommand>,
}

impl DlThread {
    /// Spawn the downloader thread, returning a handle.
    pub(crate) fn spawn(
        downloader: Arc<dyn TorrentDownloader>,
        event_tx: Sender<Event>,
        effect_tx: Sender<Effect>,
    ) -> Self {
        let (tx, rx) = bounded::<DlCommand>(crate::config::CHANNEL_CAPACITY);
        std::thread::spawn(move || run(downloader, rx, event_tx, effect_tx));
        Self { tx }
    }

    /// Submit a magnet / direct URI (non-`.torrent`).
    /// Returns `Err(())` if the downloader thread has died.
    pub(crate) fn send_add_uri(
        &self,
        uri: String,
        dir: String,
        feed_id: Uuid,
    ) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::AddUri { uri, dir, feed_id })
    }

    /// Submit raw `.torrent` bytes.
    pub(crate) fn send_add_bytes(
        &self,
        data: Vec<u8>,
        dir: String,
        feed_id: Uuid,
    ) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::AddBytes { data, dir, feed_id })
    }

    /// Submit a completed-task poll.
    pub(crate) fn send_poll_completed(&self) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::PollCompleted)
    }

    /// Submit a failed-task poll.
    pub(crate) fn send_poll_failed(&self) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::PollFailed)
    }

    /// Submit a full snapshot query.
    pub(crate) fn send_query_all(&self) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::QueryAll)
    }

    /// Submit a health-check query with synchronous reply.
    pub(crate) fn send_check_connection(
        &self,
        reply_tx: Sender<ApiResult<()>>,
    ) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::CheckConnection { reply_tx })
    }

    /// Submit a completed-download handler (fire-and-forget).
    pub(crate) fn send_handle_completed(
        &self,
        infohash: String,
        feed_id: Uuid,
        anime: AnimeIdentity,
        library_dir: String,
        download_dir: String,
        expected_episode: u32,
    ) -> Result<(), TrySendError<DlCommand>> {
        self._send(DlCommand::HandleCompleted {
            infohash,
            feed_id,
            anime,
            library_dir,
            download_dir,
            expected_episode,
        })
    }

    // ── internal ──

    fn _send(&self, cmd: DlCommand) -> Result<(), TrySendError<DlCommand>> {
        self.tx.try_send(cmd)
    }
}

// ── Thread worker ──

fn run(
    dl: Arc<dyn TorrentDownloader>,
    rx: Receiver<DlCommand>,
    event_tx: Sender<Event>,
    effect_tx: Sender<Effect>,
) {
    log::info!("[dl-thread] started");
    for cmd in rx {
        match cmd {
            DlCommand::AddUri { uri, dir, feed_id } => match dl.add_uri(&uri, &dir) {
                Ok(infohash) => {
                    log::info!("[dl-thread] add_uri: infohash={infohash}");
                    let _ = event_tx.send(Event::DownloadStarted {
                        infohash,
                        feed_id,
                        torrent_url: uri,
                    });
                }
                Err(e) => log::warn!("[dl-thread] add_uri failed: {e}"),
            },
            DlCommand::AddBytes { data, dir, feed_id } => match dl.add_torrent_bytes(&data, &dir) {
                Ok(ref ih) if !ih.is_empty() => {
                    log::info!("[dl-thread] add_bytes: infohash={ih}");
                    let _ = event_tx.send(Event::DownloadStarted {
                        infohash: ih.clone(),
                        feed_id,
                        torrent_url: String::new(),
                    });
                }
                Ok(_) => log::warn!("[dl-thread] add_bytes returned empty infohash"),
                Err(e) => log::warn!("[dl-thread] add_bytes failed: {e}"),
            },
            DlCommand::PollCompleted => match dl.poll_completed() {
                Ok(tasks) if tasks.is_empty() => {}
                Ok(tasks) => {
                    log::info!("[dl-thread] poll_completed: {} task(s)", tasks.len());
                    for task in tasks {
                        let _ = event_tx.send(Event::DownloaderNotification {
                            infohash: task.infohash,
                            status: DownloadStatus::Completed,
                        });
                    }
                }
                Err(e) => log::warn!("[dl-thread] poll_completed failed: {e}"),
            },
            DlCommand::PollFailed => match dl.poll_failed() {
                Ok(tasks) if tasks.is_empty() => {}
                Ok(tasks) => {
                    log::info!("[dl-thread] poll_failed: {} task(s)", tasks.len());
                    for task in tasks {
                        let _ = event_tx.send(Event::DownloaderNotification {
                            infohash: task.infohash,
                            status: DownloadStatus::Failed,
                        });
                    }
                }
                Err(e) => log::warn!("[dl-thread] poll_failed failed: {e}"),
            },
            DlCommand::QueryAll => match dl.query_all() {
                Ok(snapshots) => {
                    let _ = event_tx.send(Event::DownloadsRefreshed { snapshots });
                }
                Err(e) => log::warn!("[dl-thread] query_all failed: {e}"),
            },
            DlCommand::HandleCompleted {
                infohash,
                feed_id,
                anime,
                library_dir,
                download_dir,
                expected_episode,
            } => {
                log::info!(
                    "[dl-thread] handle_completed: {} feed={feed_id} anime={}",
                    &infohash[..infohash.len().min(16)],
                    anime.name
                );

                let files = match dl.list_files(&infohash) {
                    Ok(f) => {
                        log::debug!("[dl-thread] list_files: {} file(s)", f.len());
                        f
                    }
                    Err(e) => {
                        log::warn!("[dl-thread] list_files failed: {e}");
                        continue;
                    }
                };

                let record = EpisodeRecord {
                    infohash: infohash.clone(),
                    torrent_url: String::new(),
                    feed_id,
                    key: EpisodeKey {
                        anime: anime.clone(),
                        episode: expected_episode,
                    },
                    status: RecordStatus::Downloading,
                    library_path: None,
                };
                let mut resolved = resolve_files(&files, &record, &download_dir, &library_dir);
                let season_dir = format!("{library_dir}/{}/S{:02}", anime.name, anime.season);

                match try_downloader_ops_blocking(&*dl, &infohash, &mut resolved, &season_dir) {
                    Ok(()) => {
                        // Let transmission sync rename_path result (§3.7) before resume.
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        if let Err(e) = dl.resume(&infohash) {
                            log::warn!("[dl-thread] resume after ops failed: {e}");
                        }
                        log::info!(
                            "[dl-thread] completed: {}",
                            &infohash[..infohash.len().min(16)]
                        );
                        for r in &resolved {
                            let _ = event_tx.send(Event::EpisodeMovedToLibrary {
                                infohash: infohash.clone(),
                                episode: r.key.episode,
                                library_path: r.to.to_string_lossy().to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "[dl-thread] downloader ops failed for {}: {e:#}",
                            &infohash[..infohash.len().min(16)]
                        );
                        if let Err(e) = dl.remove(&infohash, false) {
                            log::warn!("[dl-thread] remove after ops failure also failed: {e}");
                        }
                        let _ = effect_tx.send(Effect::FilesystemFallback {
                            infohash,
                            resolved,
                            season_dir,
                        });
                    }
                }
            }
            DlCommand::CheckConnection { reply_tx } => match dl.check_connection() {
                Ok(()) => {
                    log::info!("[dl-thread] downloader connection OK");
                    let _ = reply_tx.send(ApiResult::OK { value: () });
                }
                Err(e) => {
                    log::warn!("[dl-thread] downloader check failed: {e}");
                    let _ = reply_tx.send(ApiResult::Err {
                        code: http_code::SERVICE_UNAVAILABLE,
                        message: "downloader unavailable".into(),
                    });
                }
            },
        }
    }
    log::info!("[dl-thread] stopped");
}

// ── Downloader ops helper ──
/// If any step fails, returns `Err` so caller can fall back to filesystem ops.
fn try_downloader_ops_blocking(
    dl: &dyn TorrentDownloader,
    infohash: &str,
    resolved: &mut [crate::utils::handler::ResolvedFile],
    season_dir: &str,
) -> anyhow::Result<()> {
    dl.pause(infohash)?;

    match dl.move_files(infohash, season_dir)? {
        OpResult::Done => {
            log::info!("[dl-thread] move: → {season_dir}");
            for r in resolved.iter_mut() {
                r.actual = std::path::PathBuf::from(format!("{season_dir}/{}", r.original_name));
            }
        }
        OpResult::Unsupported => {
            log::debug!("[dl-thread] move not supported by downloader");
            anyhow::bail!("downloader does not support move");
        }
    }

    for r in resolved.iter_mut() {
        let clean_path = r
            .original_path
            .strip_suffix(".part")
            .unwrap_or(&r.original_path);
        match dl.rename_file(infohash, clean_path, &r.target_name)? {
            OpResult::Done => {
                log::info!(
                    "[dl-thread] rename: {} → {}",
                    r.original_path,
                    r.target_name
                );
                r.actual = std::path::PathBuf::from(format!("{season_dir}/{}", r.target_name));
            }
            OpResult::Unsupported => {
                log::debug!("[dl-thread] rename not supported by downloader");
                anyhow::bail!("downloader does not support rename");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::downloader::mock::MockDownloader;
    use crossbeam_channel::bounded;

    #[test]
    fn add_uri_produces_download_started() {
        let dl = Arc::new(MockDownloader::new());
        let (event_tx, event_rx) = bounded::<Event>(4);
        let (effect_tx, _) = bounded::<Effect>(4);
        let thread = DlThread::spawn(dl, event_tx, effect_tx);
        let feed_id = uuid::Uuid::new_v4();

        thread
            .send_add_uri("magnet:?xt=urn:btih:abc".into(), "/dl".into(), feed_id)
            .unwrap();

        let ev = event_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("should receive DownloadStarted");
        match ev {
            Event::DownloadStarted {
                infohash,
                feed_id: f,
                ..
            } => {
                assert!(
                    !infohash.is_empty(),
                    "mock should generate non-empty infohash"
                );
                assert_eq!(f, feed_id);
            }
            other => panic!("expected DownloadStarted, got {other:?}"),
        }
    }

    #[test]
    fn poll_produces_notification() {
        let dl = Arc::new(MockDownloader::new());
        // Pre-populate the mock with a completed task so poll returns it.
        let _ = dl.add_uri("magnet:?xt=urn:btih:xyz", "/dl");

        let (event_tx, event_rx) = bounded::<Event>(4);
        let (effect_tx, _) = bounded::<Effect>(4);
        let thread = DlThread::spawn(dl, event_tx, effect_tx);
        thread.send_poll_completed().unwrap();

        let ev = event_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("should receive DownloaderNotification");
        assert!(matches!(
            ev,
            Event::DownloaderNotification {
                status: DownloadStatus::Completed,
                ..
            }
        ));
    }

    #[test]
    fn query_all_produces_refreshed() {
        let dl = Arc::new(MockDownloader::new());
        let (event_tx, event_rx) = bounded::<Event>(4);
        let (effect_tx, _) = bounded::<Effect>(4);
        let thread = DlThread::spawn(dl, event_tx, effect_tx);
        thread.send_query_all().unwrap();

        let ev = event_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("should receive DownloadsRefreshed");
        assert!(matches!(ev, Event::DownloadsRefreshed { .. }));
    }

    #[test]
    fn check_connection_returns_ok() {
        let dl = Arc::new(MockDownloader::new());
        let (event_tx, _) = bounded::<Event>(4);
        let (effect_tx, _) = bounded::<Effect>(4);
        let thread = DlThread::spawn(dl, event_tx, effect_tx);
        let (reply_tx, reply_rx) = bounded(1);

        thread.send_check_connection(reply_tx).unwrap();

        let result = reply_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("should receive health reply");
        assert!(matches!(result, ApiResult::OK { .. }));
    }
}
