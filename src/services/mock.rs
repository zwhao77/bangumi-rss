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
            tasks: Mutex::new(vec![
                MockTask {
                    infohash: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                    name: "Test Anime S01E01".into(),
                    completed: true,
                },
                MockTask {
                    infohash: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".into(),
                    name: "Test Anime S01E02".into(),
                    completed: true,
                },
                MockTask {
                    infohash: "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".into(),
                    name: "Test Anime S01E03".into(),
                    completed: true,
                },
            ]),
            counter: Mutex::new(3),
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
        /// In-memory file store: path → binary content.
        /// Pre-seeded with a default state.json; unknown files return dummy data.
        files: std::sync::Mutex<HashMap<PathBuf, Vec<u8>>>,
    }

    impl MockFileSystem {
        pub fn new() -> Self {
            let mut files = HashMap::new();

            use crate::core::state::{AppState, Feed};
            use crate::types::{AnimeIdentity, EpisodeKey, EpisodeRecord, RecordStatus};
            use uuid::Uuid;

            let feed_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

            let mut mk_rec = |infohash: &str, ep: u32| -> (String, EpisodeRecord) {
                let path = format!("/mock/anime/Test Anime/S01/Test Anime S01E{ep:02}.mp4");
                // Also pre-populate the file in the mock fs.
                files.insert(PathBuf::from(&path), super::MINI_MP4.to_vec());
                (
                    infohash.into(),
                    EpisodeRecord {
                        infohash: infohash.into(),
                        torrent_url: "https://example.com/test.torrent".into(),
                        feed_id,
                        key: EpisodeKey {
                            anime: AnimeIdentity { name: "Test Anime".into(), season: 1 },
                            episode: ep,
                        },
                        status: RecordStatus::InLibrary,
                        library_path: Some(path),
                    },
                )
            };

            let state = AppState {
                feeds: [(
                    feed_id,
                    Feed {
                        id: feed_id,
                        url: "https://example.com/feed.xml".into(),
                        anime: AnimeIdentity { name: "Test Anime".into(), season: 1 },
                        confirmed: true,
                        bangumi_info: None,
                    },
                )]
                .into_iter()
                .collect(),
                tracker: [
                    mk_rec("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", 1),
                    mk_rec("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", 2),
                    mk_rec("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC", 3),
                ]
                .into(),
                seen_urls: HashSet::new(),
                cached_downloads: vec![],
                download_dir: "/mock/downloads".into(),
                library_dir: "/mock/anime".into(),
                webhook_url: None,
            };
            files.insert(
                PathBuf::from(".").join("state.json"),
                serde_json::to_vec_pretty(&state).unwrap_or_default(),
            );
            Self {
                dirs: std::sync::Mutex::new(HashSet::new()),
                moves: std::sync::Mutex::new(Vec::new()),
                existing: std::sync::Mutex::new(HashSet::new()),
                files: std::sync::Mutex::new(files),
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
            let data = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("mock file not found: {path:?}"))?;
            let s = String::from_utf8(data)
                .map_err(|e| anyhow::anyhow!("mock file not valid UTF-8: {e}"))?;
            log::debug!("[mock-fs] read: {path:?} ({} bytes)", s.len());
            Ok(s)
        }

        fn write_string(&self, path: &Path, content: &str) -> anyhow::Result<()> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), content.as_bytes().to_vec());
            log::debug!("[mock-fs] write: {path:?} ({} bytes)", content.len());
            Ok(())
        }

        fn open_file(&self, path: &Path) -> anyhow::Result<crate::types::FileStream> {
            let files = self.files.lock().unwrap();
            match files.get(path) {
                Some(data) => Ok(crate::types::FileStream::new(
                    std::io::Cursor::new(data.clone()),
                    data.len() as u64,
                )),
                None => {
                    if path.to_string_lossy().ends_with(".mp4") {
                        return Ok(crate::types::FileStream::new(
                            std::io::Cursor::new(super::MINI_MP4.to_vec()),
                            super::MINI_MP4.len() as u64,
                        ));
                    }
                    let dummy = vec![0xAB; super::DUMMY_FILE_SIZE as usize];
                    Ok(crate::types::FileStream::new(
                        std::io::Cursor::new(dummy),
                        super::DUMMY_FILE_SIZE,
                    ))
                }
            }
        }
    }
}

/// Standard dummy file size for mock filesystem (1 MB).
const DUMMY_FILE_SIZE: u64 = 1024 * 1024;

/// Minimal valid H.264 MP4 file (2×2 black frame, ~40 ms) for mock video playback.
const MINI_MP4: &[u8] = include_bytes!("../../res/mini.mp4");

pub use mock_fs::MockFileSystem;
