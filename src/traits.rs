//! Service interfaces — all external side effects go through these traits.
//!
//! Concrete implementations live in `services/`.
//! This decouples the core logic from I/O, making the system testable.

use std::path::Path;

use crate::types::{CompletedDownload, RssItem, RssPreview, TorrentFile};

// ── Service traits ──

/// Fetches and parses an RSS feed into a list of torrent items.
pub trait RssFetcher: Send + Sync {
    fn fetch(&self, url: &str) -> anyhow::Result<Vec<RssItem>>;
    fn fetch_preview(&self, url: &str) -> anyhow::Result<RssPreview>;
}

/// Manages torrent downloads (add, list, rename, poll completion).
pub trait TorrentDownloader: Send + Sync {
    /// Add a torrent by URI (magnet or HTTP URL).
    /// Returns the torrent's infohash.
    fn add_uri(&self, uri: &str, dir: &str) -> anyhow::Result<String>;

    /// Add a torrent from raw .torrent file bytes.
    /// The downloader base64-encodes and sends via addTorrent RPC.
    fn add_torrent_bytes(&self, data: &[u8], dir: &str) -> anyhow::Result<String>;

    /// List files in a completed/active download identified by infohash.
    fn list_files(&self, infohash: &str) -> anyhow::Result<Vec<TorrentFile>>;

    /// Rename a file within a download task.
    fn rename_file(&self, infohash: &str, old_path: &str, new_name: &str) -> anyhow::Result<bool>;

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
}

/// Sends out-of-band notifications (webhook, Server酱, etc.).
pub trait Notifier: Send + Sync {
    fn send(&self, title: &str, body: &str);
}
