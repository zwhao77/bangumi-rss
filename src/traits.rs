//! Service interfaces — all external side effects go through these traits.
//!
//! Concrete implementations live in `services/`.
//! This decouples the core logic from I/O, making the system testable.

use std::path::Path;

// ── Data types shared across the service boundary ──

/// An item parsed from an RSS feed.
///
/// Logic receives these and translates them into `AddTorrent` effects
/// (potentially filtering by title, skipping duplicates, etc.).
#[derive(Debug, Clone)]
pub struct RssItem {
    /// Torrent title from `<title>`, e.g. "[字幕组] 番剧名 - 01 [1080p]".
    pub title: String,
    /// Torrent / magnet URL from `<enclosure url>`.
    pub torrent_url: String,
    /// Mikan episode page URL from `<link>` — reserved for future metadata scraping.
    #[allow(dead_code)]
    pub homepage: Option<String>,
}

/// A file inside a completed torrent download.
#[derive(Debug, Clone)]
pub struct TorrentFile {
    pub name: String,
    #[allow(dead_code)]
    pub size: u64,
}

/// A completed download task reported by the downloader.
#[derive(Debug, Clone)]
pub struct CompletedDownload {
    /// Cross-downloader stable identifier (aria2: `infoHash`, qB: `hash`).
    pub infohash: String,
}

// ── Service traits ──

/// Fetches and parses an RSS feed into a list of torrent items.
pub trait RssFetcher: Send + Sync {
    /// Fetch and parse all torrent items from an RSS feed.
    fn fetch(&self, url: &str) -> anyhow::Result<Vec<RssItem>>;

    /// Fetch the RSS channel title and items for preview (no torrent URLs needed).
    fn fetch_preview(&self, url: &str) -> anyhow::Result<RssPreview>;
}

/// Lightweight RSS preview — channel title + sample item titles.
#[derive(Debug, Clone)]
pub struct RssPreview {
    /// `<channel><title>` — suggested anime name.
    pub channel_title: String,
    /// `<item><title>` samples — used to tokenize and extract season/episode.
    pub item_titles: Vec<String>,
}

/// Manages torrent downloads (add, list, rename, poll completion).
///
/// All operations identify tasks by **infohash** (40-char hex, stable across
/// restarts and downloader implementations).
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
}

/// Sends out-of-band notifications (webhook, Server酱, etc.).
pub trait Notifier: Send + Sync {
    fn send(&self, title: &str, body: &str);
}

/// Searches Bangumi / bgm.tv for anime metadata.
pub trait BangumiSearcher: Send + Sync {
    /// Search by keyword, return the best match's Bangumi subject ID.
    fn search_subject_id(&self, keyword: &str) -> anyhow::Result<Option<u32>>;

    /// Fetch full metadata for a Bangumi subject.
    /// Cover image URL is included in the result; caching is left to the server/web UI layer.
    fn get_subject_detail(
        &self,
        subject_id: u32,
    ) -> anyhow::Result<Option<crate::types::BangumiInfo>>;
}
