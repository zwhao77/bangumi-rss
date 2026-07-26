//! Mock service implementations — useful for development without real backends.
//!
//! Enable via environment variables:
//!   MOCK_DOWNLOADER=1  →  fake torrent downloader
//!   MOCK_RSS=1         →  fake RSS feed (TODO)

use std::sync::Mutex;

use crate::traits::TorrentDownloader;
use crate::types::{CompletedDownload, DownloadSnapshot, DownloadState, TorrentFile};

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

        let name = format!("[MockSubs] {} - 01 [1080p].mkv", task.name);
        log::debug!("[mock-dl] list_files: infohash={infohash} → {name}");
        Ok(vec![TorrentFile { name }])
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
        log::debug!("[mock-dl] query_all: {} tasks", tasks.len());
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

    fn check_connection(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── Mock file system ──

mod mock_fs {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    use crate::traits::FileOps;

    /// Fake file system — tracks created dirs, moved files, and file contents in memory.
    pub struct MockFileSystem {
        dirs: std::sync::Mutex<HashSet<PathBuf>>,
        moves: std::sync::Mutex<Vec<(PathBuf, PathBuf)>>,
        #[allow(dead_code)]
        pub existing: std::sync::Mutex<HashSet<PathBuf>>,
        files: std::sync::Mutex<HashMap<PathBuf, String>>,
    }

    impl MockFileSystem {
        pub fn new() -> Self {
            Self {
                dirs: std::sync::Mutex::new(HashSet::new()),
                moves: std::sync::Mutex::new(Vec::new()),
                existing: std::sync::Mutex::new(HashSet::new()),
                files: std::sync::Mutex::new(HashMap::new()),
            }
        }

        #[allow(dead_code)]
        pub fn move_count(&self) -> usize {
            self.moves.lock().unwrap().len()
        }
    }

    impl FileOps for MockFileSystem {
        fn ensure_dir(&self, path: &Path) -> anyhow::Result<()> {
            self.dirs.lock().unwrap().insert(path.to_path_buf());
            log::debug!("[mock-fs] ensure_dir: {path:?}");
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

        fn read_to_string(&self, path: &Path) -> anyhow::Result<String> {
            let content = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("mock file not found: {path:?}"));
            match &content {
                Ok(s) => log::debug!("[mock-fs] read: {path:?} ({} bytes)", s.len()),
                Err(e) => log::debug!("[mock-fs] read miss: {path:?} — {e}"),
            }
            content
        }

        fn write_string(&self, path: &Path, content: &str) -> anyhow::Result<()> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), content.to_string());
            log::debug!("[mock-fs] write: {path:?} ({} bytes)", content.len());
            Ok(())
        }
    }
}

pub use mock_fs::MockFileSystem;
