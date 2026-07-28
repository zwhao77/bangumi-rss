//! Service interfaces — all external side effects go through these traits.
//!
//! Concrete implementations live in `services/`.
//! This decouples the core logic from I/O, making the system testable.

use std::path::Path;

use crate::types::{CompletedDownload, TorrentFile};

// ── Service traits ──

/// Manages torrent downloads (add, list, rename, pause, move, remove).
pub trait TorrentDownloader: Send + Sync {
    /// Add a torrent by URI (magnet or HTTP URL).
    /// Returns the torrent's infohash.
    fn add_uri(&self, uri: &str, dir: &str) -> anyhow::Result<String>;

    /// Add a torrent from raw .torrent file bytes.
    /// The downloader base64-encodes and sends via addTorrent RPC.
    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String>;

    /// List files in a completed/active download identified by infohash.
    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>>;

    /// Rename a file or folder within a torrent download.
    ///
    /// `old_path` is the torrent-relative path to rename (from `TorrentFile.path`).
    /// `new_name` is the new filename (last component only, NOT a full path).
    ///
    /// Returns `Ok(true)` on success, `Ok(false)` if the downloader does not
    /// support this operation (caller should fall back to filesystem rename).
    fn rename_file(
        &self,
        infohash: &str,
        old_path: &str,
        new_name: &str,
    ) -> anyhow::Result<bool>;

    /// Move all files of a torrent to a new directory.
    ///
    /// Returns `Ok(true)` on success, `Ok(false)` if the downloader does not
    /// support this operation (caller should fall back to filesystem move).
    fn move_files(&self, infohash: &str, new_location: &str) -> anyhow::Result<bool>;

    /// Pause (stop) a download task.  For BT downloads this also pauses seeding.
    fn pause(&self, infohash: &str) -> anyhow::Result<bool>;

    /// Remove a download task from the downloader.
    ///
    /// If `delete_files` is false, the downloaded data is preserved on disk.
    /// This is the common case when files have already been moved to the library.
    fn remove(&self, infohash: &str, delete_files: bool) -> anyhow::Result<bool>;

    /// Poll for recently completed downloads.
    fn poll_completed(&self) -> anyhow::Result<Vec<CompletedDownload>>;

    /// Poll for recently failed / errored downloads.
    fn poll_failed(&self) -> anyhow::Result<Vec<CompletedDownload>> {
        Ok(vec![])
    }

    /// Query all current tasks (active + stopped) from the downloader.
    fn query_all(&self) -> anyhow::Result<Vec<crate::types::DownloadSnapshot>> {
        anyhow::bail!("not implemented")
    }

    /// Check if the downloader is reachable and authenticated.
    fn check_connection(&self) -> anyhow::Result<()>;
}

/// File-system operations abstracted for testability.
pub trait FileOps: Send + Sync {
    /// Move (rename) a file from `from` to `to`.
    fn move_file(&self, from: &Path, to: &Path) -> anyhow::Result<()>;

    /// Ensure a directory exists, creating parents as needed.
    fn ensure_dir(&self, path: &Path) -> anyhow::Result<()>;

    /// Read the entire contents of a file into a String.
    fn read_to_string(&self, path: &Path) -> anyhow::Result<String>;

    /// Write a string to a file, overwriting if it exists.
    fn write_string(&self, path: &Path, content: &str) -> anyhow::Result<()>;

    /// Open a file and return a streamable file handle.
    /// For RealFileSystem this delegates to `File::open`;
    /// MockFileSystem returns an in-memory stream from stored bytes.
    fn open_file(&self, path: &Path) -> anyhow::Result<crate::types::FileStream>;
}
