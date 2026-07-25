//! Worker pool — fixed threads, zero `dyn`, job enum dispatch.
//!
//! A fixed number of worker threads pull `Job`s from a bounded channel.
//! Queue size = `threads × 2`, if full the caller blocks briefly on `spawn`.
//! This prevents unlimited memory growth while allowing bursts.

use crossbeam_channel::{bounded, Receiver, Sender};
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
                    let resp = ureq::get(&uri).call()?;
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
        }
    }
}

/// Fixed-size thread pool with bounded task queue.
///
/// - `threads`: number of worker threads (concurrency limit).
/// - Queue size = `threads × 2`.  `spawn` blocks when full (`try_spawn` returns `Err`).
pub struct WorkerPool {
    tx: Sender<Job>,
}

impl WorkerPool {
    /// Create a pool with `threads` workers.
    /// Queue capacity is `threads × 2`.
    pub fn new(threads: usize) -> Self {
        let cap = threads * 2;
        let (tx, rx) = bounded::<Job>(cap);

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

    /// Submit a job (blocks briefly if queue is full).
    pub fn spawn(&self, job: Job) {
        self.tx.send(job).expect("worker pool closed");
    }

    /// Try to submit a job without blocking.  Returns `Err(job)` if queue is full.
    pub fn try_spawn(&self, job: Job) -> Result<(), Job> {
        self.tx.try_send(job).map_err(|e| e.into_inner())
    }
}
