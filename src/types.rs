//! Domain types — shared vocabulary across logic, executor, and API.
//!
//! These are pure data, serializable, with no dependency on I/O or services.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── API & Service-boundary types ──

/// 泛型 API 结果。序列化为:
///   OK → {"success":true, "data":T, "message":"..."}
///   Err → {"success":false, "code":u16, "message":"..."}
pub enum ApiResult<T> {
    OK { value: T },
    Err { code: u16, message: String },
}

/// HTTP status codes used in `ApiResult::Err.code` and `ApiError`.
pub mod http_code {
    pub const BAD_REQUEST: u16 = 400;
    pub const NOT_FOUND: u16 = 404;
    pub const INTERNAL: u16 = 500;
    pub const SERVICE_UNAVAILABLE: u16 = 503;
}

impl<T: Serialize> Serialize for ApiResult<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ApiResult::OK { value } => {
                let mut s = serializer.serialize_struct("ApiResult", 2)?;
                s.serialize_field("success", &true)?;
                s.serialize_field("data", value)?;
                s.end()
            }
            ApiResult::Err { code, message } => {
                let mut s = serializer.serialize_struct("ApiResult", 3)?;
                s.serialize_field("success", &false)?;
                s.serialize_field("code", code)?;
                s.serialize_field("message", message)?;
                s.end()
            }
        }
    }
}

/// Feed list API DTO (returned to the web UI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FeedInfo {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub season: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bangumi_info: Option<BangumiInfo>,
}

/// An item parsed from an RSS feed.
#[derive(Debug, Clone)]
pub struct RssItem {
    pub title: String,
    pub torrent_url: String,
    /// `true` if the title looks like a batch release (e.g. "01-12").
    /// Set during RSS parsing so the logic layer can skip it directly.
    pub is_batch: bool,
}

/// A file inside a completed torrent download.
#[derive(Debug, Clone)]
pub struct TorrentFile {
    pub name: String,
}

/// A completed download task reported by the downloader.
#[derive(Debug, Clone)]
pub struct CompletedDownload {
    pub infohash: String,
}

/// Lightweight RSS preview — channel title + sample item titles.
#[derive(Debug, Clone)]
pub struct RssPreview {
    #[allow(dead_code)]
    pub channel_title: String,
    pub item_titles: Vec<String>,
}

// ── Anime & Episode identity ──

/// Identifies an anime series uniquely.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnimeIdentity {
    pub name: String,
    pub season: u8,
}

/// Identifies a specific episode — used for dedup across different torrents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpisodeKey {
    pub anime: AnimeIdentity,
    pub episode: u32,
}

/// Tracker record — tracks a torrent from start to library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeRecord {
    /// Torrent infohash (40-char hex).
    pub infohash: String,
    /// Original torrent download URL.
    pub torrent_url: String,
    /// Feed this download belongs to.
    pub feed_id: Uuid,
    /// Anime + episode identity.
    pub key: EpisodeKey,
    /// Current lifecycle status.
    pub status: RecordStatus,
    /// Final path in the media library (set after move).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecordStatus {
    /// Downloading — episode number not yet known (key.episode = 0).
    Downloading,
    /// Tokenizer resolved the episode number.
    Resolved,
    /// File moved into the media library.
    InLibrary,
}

/// Feed preview — returned to the web UI before user confirms subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedPreview {
    pub suggested_name: String,
    pub suggested_season: u8,
    pub latest_episode: Option<u32>,
    pub group: Option<String>,
    pub sample_titles: Vec<String>,
    /// Bangumi metadata — populated if the API search succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bangumi_info: Option<BangumiInfo>,
}

impl Default for FeedPreview {
    fn default() -> Self {
        Self {
            suggested_name: String::new(),
            suggested_season: 1,
            latest_episode: None,
            group: None,
            sample_titles: vec![],
            bangumi_info: None,
        }
    }
}

// ── Bangumi metadata ──

/// Anime metadata fetched from bgm.tv (Bangumi).
/// Image caching is left to the server / web UI layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BangumiInfo {
    /// Bangumi subject ID.
    pub bangumi_id: u32,
    /// Official Chinese name (e.g. "葬送的芙莉莲").
    pub name_cn: String,
    /// Original name (e.g. "葬送のフリーレン").
    pub name: String,
    /// Synopsis (truncated to ~200 chars).
    pub summary: String,
    /// Total episode count according to Bangumi (absent for ongoing shows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eps_count: Option<u32>,
    /// Average rating (0.0–10.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<f32>,
    /// Number of votes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_count: Option<u32>,
    /// First air date (e.g. "2023-09-29").
    pub air_date: String,
    /// Cover image URL (common size, 150×212). Server/web UI handles display / caching.
    pub image_url: String,
    /// Bangumi rank (e.g. 40 = #40).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    /// Air weekday (1=Mon … 7=Sun).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub air_weekday: Option<u8>,
}

// ── Download state ──

/// Unified download state (aria2 ↔ qBittorrent normalised).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Downloading,
    Seeding,
    Waiting,
    Paused,
    Checking,
    Completed,
    Failed,
    Removed,
}

// ── Download list API types ──

/// Raw snapshot from the downloader — executor produces these.
/// logic enriches with feed context from the tracker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadSnapshot {
    pub infohash: String,
    pub state: DownloadState,
    pub progress: f32,
    pub speed: u64,
    pub size: u64,
    pub name: String,
}

/// Enriched view for the API response — logic fills in feed context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadInfo {
    pub infohash: String,
    pub feed_name: String,
    pub season: u8,
    pub state: DownloadState,
    pub progress: f32,
    pub speed: u64,
    pub size: u64,
    pub name: String,
}

// ── Notification types ──

/// A new episode has been downloaded and moved to the library.
#[derive(Debug, Clone)]
pub struct EpisodeDownloadedData {
    pub anime_name: String,
    pub season: u8,
    pub episode: u32,
    pub library_path: String,
    pub name_cn: Option<String>,
    pub name_original: Option<String>,
    pub summary: Option<String>,
    pub rating: Option<f32>,
    pub image_url: Option<String>,
    pub eps_count: Option<u32>,
}

/// Something went wrong.
#[derive(Debug, Clone)]
pub struct FailedData {
    pub title: String,
    pub message: String,
}

/// Notification data — pure data, no I/O.
/// Produced by `logic::reduce`, consumed by the template renderer.
#[derive(Debug, Clone)]
pub enum Notification {
    EpisodeDownloaded(EpisodeDownloadedData),
    Failed(FailedData),
}

// ── Streamable file abstraction ──

use std::io::{Read, Seek};

/// Combined trait: anything that can be read and seeked.
/// Required because Rust doesn't allow `Box<dyn Read + Seek + Send>` directly.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// A streamable file with known size, abstracted over real files and in-memory data.
/// Enables mock file systems to produce streams without temp files.
pub struct FileStream {
    inner: Box<dyn ReadSeek + Send>,
    size: u64,
}

impl FileStream {
    /// Create a FileStream from anything that can be read, seeked, and sent across threads.
    /// - Production: `FileStream::new(File::open(path)?, file.metadata()?.len())`
    /// - Mock: `FileStream::new(Cursor::new(data), data.len() as u64)`
    pub fn new(inner: impl Read + Seek + Send + 'static, size: u64) -> Self {
        Self {
            inner: Box::new(inner),
            size,
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Seek to a position and return a length-limited reader.
    pub fn into_range(
        mut self,
        start: u64,
        length: u64,
    ) -> std::io::Result<impl Read + Send + 'static> {
        self.inner.seek(std::io::SeekFrom::Start(start))?;
        Ok(self.inner.take(length))
    }
}
