//! Domain types — shared vocabulary across logic, executor, and API.
//!
//! These are pure data, serializable, with no dependency on I/O or services.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

// ── Episode resolution ──

/// A fully identified episode — executor fills this from `list_files` + `tokenizer`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ResolvedEpisode {
    #[allow(dead_code)]
    pub feed_id: Uuid,
    #[allow(dead_code)]
    pub anime: AnimeIdentity,
    pub episode: u32,
    /// Normalised file name, e.g. "Oshi no Ko S01E01.mkv".
    pub target_name: String,
}

impl ResolvedEpisode {
    /// Absolute path inside the download staging directory.
    #[allow(dead_code)]
    pub fn download_path(&self, download_dir: &str) -> String {
        format!("{}/{}/{}", download_dir, self.feed_id, self.target_name)
    }

    /// Absolute path inside the media library.
    #[allow(dead_code)]
    pub fn library_path(&self, library_dir: &str) -> String {
        format!(
            "{}/{}/S{:02}/{}",
            library_dir, self.anime.name, self.anime.season, self.target_name
        )
    }
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
