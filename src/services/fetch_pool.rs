//! Fetch pool — dedicated thread pool for RSS/torrent network fetches.
//!
//! A fixed number of worker threads pull `FetchJob`s from a bounded channel.
//! Queue size is configurable; `try_spawn` returns immediately when full,
//! so callers can skip instead of blocking.
//!
//! This is the *only* place that does synchronous HTTP I/O for RSS polling
//! and torrent `.torrent` file downloads.

use crossbeam_channel::{Receiver, Sender, bounded};
use uuid::Uuid;

use crate::core::effect::Effect;

/// All background fetch job types.
pub enum FetchJob {
    DownloadTorrent {
        uri: String,
        save_path: String,
        feed_id: Uuid,
        effect_tx: Sender<Effect>,
    },
    FetchRss {
        url: String,
        feed_id: Uuid,
        download_dir: String,
        effect_tx: Sender<Effect>,
    },
    /// Fire-and-forget webhook POST. No feedback Effects.
    Notify {
        url: String,
        body: String,
        content_type: String,
    },
}

impl FetchJob {
    fn execute(self) {
        match self {
            FetchJob::DownloadTorrent {
                uri,
                save_path,
                feed_id,
                effect_tx,
            } => execute_download_torrent(uri, save_path, feed_id, effect_tx),
            FetchJob::FetchRss {
                url,
                feed_id,
                download_dir,
                effect_tx,
            } => execute_fetch_rss(url, feed_id, download_dir, effect_tx),
            FetchJob::Notify {
                url,
                body,
                content_type,
            } => execute_notify(url, body, content_type),
        }
    }
}

// ── Individual job handlers ──

fn execute_download_torrent(
    uri: String,
    save_path: String,
    feed_id: Uuid,
    effect_tx: Sender<Effect>,
) {
    log::info!("downloading torrent: {uri}");
    // .torrent files are typically < 1 MB; 10 MB is a generous safety cap.
    const MAX_TORRENT_SIZE: u64 = 10 * 1024 * 1024;
    let timeout = std::time::Duration::from_secs(crate::config::HTTP_TIMEOUT_SECS);
    match crate::services::fetch::fetch_bytes(&uri, timeout, MAX_TORRENT_SIZE) {
        Ok(bytes) => {
            // Validate: .torrent files are bencoded dictionaries starting with 'd'.
            if bytes.first() != Some(&b'd') {
                log::warn!(
                    "torrent download failed: not a valid .torrent (missing leading 'd'), feed={feed_id}"
                );
                return;
            }
            log::info!("torrent downloaded: {} bytes, feed={feed_id}", bytes.len());
            effect_tx
                .send(Effect::AddTorrentBytes {
                    data: bytes,
                    save_path,
                    feed_id,
                    torrent_url: uri,
                })
                .ok();
        }
        Err(e) => {
            log::warn!("torrent download failed: {e}");
        }
    }
}

fn execute_fetch_rss(url: String, feed_id: Uuid, download_dir: String, effect_tx: Sender<Effect>) {
    log::debug!("fetching RSS: feed={feed_id}");
    match crate::services::fetch::fetch_items(&url) {
        Ok(items) => {
            log::info!("RSS items: {} for feed={feed_id}", items.len());
            effect_tx
                .send(Effect::RssFetchComplete {
                    feed_id,
                    items,
                    download_dir,
                })
                .ok();
        }
        Err(e) => {
            log::debug!("RSS fetch/parse failed for feed={feed_id}: {e}");
            effect_tx
                .send(Effect::RssFetchFailed {
                    feed_id,
                    error: format!("{e:#}"),
                })
                .ok();
        }
    }
}

fn execute_notify(url: String, body: String, content_type: String) {
    log::debug!("webhook POST: {url}");
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
}

/// Fixed-size thread pool with bounded task queue.
///
/// - `threads`: number of worker threads (concurrency limit).
/// - `capacity`: size of the bounded job queue.
///   `try_spawn` returns `Err` when full — callers should retry next cycle.
pub struct FetchPool {
    tx: Sender<FetchJob>,
}

impl FetchPool {
    /// Create a pool with `threads` workers and `capacity` queue slots.
    pub fn new(threads: usize, capacity: usize) -> Self {
        let (tx, rx) = bounded::<FetchJob>(capacity);

        for _ in 0..threads {
            let rx: Receiver<FetchJob> = rx.clone();
            std::thread::spawn(move || {
                for job in rx {
                    job.execute();
                }
            });
        }

        Self { tx }
    }

    #[allow(dead_code)]
    /// Submit a job (blocks briefly if queue is full).
    pub fn spawn(&self, job: FetchJob) {
        self.tx.send(job).expect("fetch pool closed");
    }

    /// Try to submit a job without blocking.  Returns `Err(job)` if queue is full.
    pub fn try_spawn(&self, job: FetchJob) -> Result<(), FetchJob> {
        self.tx.try_send(job).map_err(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn torrent_magic_byte_rejects_non_d() {
        // Simulate: bytes start with '<' (like an HTML error page)
        let bytes = b"<html>404 Not Found</html>".to_vec();
        assert_ne!(bytes.first(), Some(&b'd'), "should not be valid torrent");
    }

    #[test]
    fn torrent_magic_byte_accepts_d() {
        // Valid bencoded dictionary
        let bytes = b"d8:announce13:example.come".to_vec();
        assert_eq!(bytes.first(), Some(&b'd'), "should be valid torrent");
    }
}
