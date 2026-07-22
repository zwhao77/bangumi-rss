//! Mock service implementations — useful for development without real backends.
//!
//! Enable via environment variables:
//!   MOCK_DOWNLOADER=1  →  fake torrent downloader
//!   MOCK_RSS=1         →  fake RSS feed (TODO)

use std::sync::Mutex;

use crate::traits::{RssFetcher, TorrentDownloader};
use crate::types::{
    CompletedDownload, DownloadSnapshot, DownloadState, RssItem, RssPreview, TorrentFile,
};

// ── Mock downloader ──

/// Fake downloader — simulates torrent lifecycle without aria2/qBittorrent.
///
/// - `add_uri`: generates a fake infohash, stores the task.
/// - `poll_completed`: returns tasks not yet completed, marks them done.
/// - `query_all`: returns all tasks with random progress/states (seeded by infohash).
pub struct MockDownloader {
    tasks: Mutex<Vec<MockTask>>,
    counter: Mutex<u32>,
}

struct MockTask {
    infohash: String,
    name: String,
    completed: bool,
}

impl MockDownloader {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
            counter: Mutex::new(0),
        }
    }
}

impl TorrentDownloader for MockDownloader {
    fn add_uri(&self, uri: &str, dir: &str) -> anyhow::Result<String> {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let infohash = format!("MOCK{:08X}", *counter);

        let name = uri.split('/').next_back().unwrap_or(uri).to_string();

        self.tasks.lock().unwrap().push(MockTask {
            infohash: infohash.clone(),
            name,
            completed: false,
        });
        log::debug!("[mock-dl] added: infohash={infohash}, dir={dir}");
        Ok(infohash)
    }

    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String> {
        // Same as add_uri for mock purposes.
        self.add_uri(&format!("mock-torrent-{}", data.len()), dir)
    }

    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>> {
        let tasks = self.tasks.lock().unwrap();
        let task = tasks
            .iter()
            .find(|t| t.infohash == infohash)
            .ok_or_else(|| anyhow::anyhow!("mock task not found: {infohash}"))?;

        Ok(vec![TorrentFile {
            name: format!("[MockSubs] {} - 01 [1080p].mkv", task.name),
        }])
    }

    fn rename_file(&self, infohash: &str, _old_path: &str, new_name: &str) -> anyhow::Result<bool> {
        log::debug!("[mock-dl] rename: {infohash} → {new_name}");
        Ok(true)
    }

    fn poll_completed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        let mut tasks = self.tasks.lock().unwrap();
        let fresh: Vec<CompletedDownload> = tasks
            .iter_mut()
            .filter(|t| !t.completed)
            .map(|t| {
                t.completed = true;
                CompletedDownload {
                    infohash: t.infohash.clone(),
                }
            })
            .collect();

        if !fresh.is_empty() {
            log::debug!("[mock-dl] poll_completed: {} new", fresh.len());
        }
        Ok(fresh)
    }

    fn query_all(&self) -> anyhow::Result<Vec<DownloadSnapshot>> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let tasks = self.tasks.lock().unwrap();
        Ok(tasks
            .iter()
            .map(|t| {
                let mut h = DefaultHasher::new();
                t.infohash.hash(&mut h);
                let seed = h.finish();
                let pct = ((seed % 100) as f32) / 100.0;

                let (state, progress) = if t.completed {
                    (DownloadState::Completed, 1.0)
                } else if pct < 0.05 {
                    (DownloadState::Waiting, 0.0)
                } else if pct < 0.10 {
                    (DownloadState::Paused, (seed % 80) as f32 / 100.0)
                } else {
                    (DownloadState::Downloading, pct)
                };

                let speed = if progress < 1.0 && progress > 0.0 {
                    (seed % 8_000_000) + 500_000
                } else {
                    0
                };

                DownloadSnapshot {
                    infohash: t.infohash.clone(),
                    state,
                    progress,
                    speed,
                    size: 500_000_000 + (seed % 200_000_000),
                    name: t.name.clone(),
                }
            })
            .collect())
    }
}

// ── Mock RSS fetcher ──

/// Fake RSS client — returns a canned response for testing.
pub struct MockRssClient;

impl RssFetcher for MockRssClient {
    fn fetch(&self, url: &str) -> anyhow::Result<Vec<RssItem>> {
        log::debug!("[mock-rss] fetch: {url}");
        Ok(vec![RssItem {
            title: "[MockSubs] 葬送的芙莉莲 第二季 - 38 [1080p]".into(),
            torrent_url: format!(
                "https://mock.example/{}/ep38.torrent",
                url.replace('/', "_")
            ),
        }])
    }

    fn fetch_preview(&self, url: &str) -> anyhow::Result<RssPreview> {
        log::debug!("[mock-rss] preview: {url}");
        Ok(RssPreview {
            channel_title: "葬送的芙莉莲".into(),
            item_titles: vec![
                "[MockSubs] 葬送的芙莉莲 第二季 - 38 [1080p]".into(),
                "[MockSubs] 葬送的芙莉莲 第二季 - 37 [1080p]".into(),
                "[MockSubs] 葬送的芙莉莲 第二季 - 36 [1080p]".into(),
            ],
        })
    }
}

// ── Mock file system ──

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::traits::FileOps;

/// Fake file system — tracks created dirs and moved files in memory.
pub struct MockFileSystem {
    dirs: Mutex<HashSet<PathBuf>>,
    moves: Mutex<Vec<(PathBuf, PathBuf)>>,
    pub existing: Mutex<HashSet<PathBuf>>,
}

impl MockFileSystem {
    pub fn new() -> Self {
        Self {
            dirs: Mutex::new(HashSet::new()),
            moves: Mutex::new(Vec::new()),
            existing: Mutex::new(HashSet::new()),
        }
    }

    pub fn move_count(&self) -> usize {
        self.moves.lock().unwrap().len()
    }
}

impl FileOps for MockFileSystem {
    fn ensure_dir(&self, path: &Path) -> anyhow::Result<()> {
        self.dirs.lock().unwrap().insert(path.to_path_buf());
        Ok(())
    }

    fn move_file(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
        self.moves
            .lock()
            .unwrap()
            .push((from.to_path_buf(), to.to_path_buf()));
        log::debug!("[mock-fs] move: {from:?} → {to:?}");
        Ok(())
    }
}
