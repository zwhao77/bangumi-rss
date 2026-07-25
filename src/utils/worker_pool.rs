//! Worker pool — fixed threads, zero `dyn`, job enum dispatch.
//!
//! A fixed number of worker threads pull `Job`s from a bounded channel.
//! Queue size = `threads × 2`, if full the caller blocks briefly on `spawn`.
//! This prevents unlimited memory growth while allowing bursts.

use std::io::Read;

use crossbeam_channel::{Receiver, Sender, bounded};
use uuid::Uuid;

use crate::core::effect::Effect;

/// All background job types.
pub enum Job {
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
}

impl Job {
    fn execute(self) {
        match self {
            Job::DownloadTorrent {
                uri,
                save_path,
                feed_id,
                effect_tx,
            } => {
                log::info!("downloading torrent: {uri}");
                match (|| -> anyhow::Result<Vec<u8>> {
                    let resp = ureq::get(&uri)
                        .timeout(std::time::Duration::from_secs(crate::config::HTTP_TIMEOUT_SECS))
                        .call()?;
                    let mut bytes: Vec<u8> = Vec::new();
                    resp.into_reader().read_to_end(&mut bytes)?;
                    Ok(bytes)
                })() {
                    Ok(bytes) => {
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
            Job::FetchRss {
                url,
                feed_id,
                download_dir,
                effect_tx,
            } => {
                log::debug!("fetching RSS: feed={feed_id}");
                match (|| -> anyhow::Result<Vec<crate::types::RssItem>> {
                    const MAX: u64 = 1_048_576; // 1 MB
                    let resp = ureq::get(&url)
                        .timeout(std::time::Duration::from_secs(crate::config::HTTP_TIMEOUT_SECS))
                        .call()?;
                    let mut body = String::new();
                    resp.into_reader()
                        .take(MAX + 1)
                        .read_to_string(&mut body)?;
                    if body.len() > MAX as usize {
                        anyhow::bail!("RSS response too large: {} bytes", body.len());
                    }
                    log::info!(
                        "RSS body: {} bytes for feed={feed_id}",
                        body.len()
                    );
                    crate::utils::rss::parse_rss(&body)
                })() {
                    Ok(items) => {
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
        }
    }
}

/// Fixed-size thread pool with bounded task queue.
///
/// - `threads`: number of worker threads (concurrency limit).
/// - `capacity`: size of the bounded job queue.  Default 512.
///   `spawn` blocks when full (`try_spawn` returns `Err`).
///   A full queue means the system is overloaded — tasks will be retried on
///   next RSS poll or poll cycle.
pub struct WorkerPool {
    tx: Sender<Job>,
}

impl WorkerPool {
    /// Create a pool with `threads` workers and `capacity` queue slots.
    pub fn new(threads: usize, capacity: usize) -> Self {
        let (tx, rx) = bounded::<Job>(capacity);

        for _ in 0..threads {
            let rx: Receiver<Job> = rx.clone();
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
    pub fn spawn(&self, job: Job) {
        self.tx.send(job).expect("worker pool closed");
    }

    /// Try to submit a job without blocking.  Returns `Err(job)` if queue is full.
    pub fn try_spawn(&self, job: Job) -> Result<(), Job> {
        self.tx.try_send(job).map_err(|e| e.into_inner())
    }
}
